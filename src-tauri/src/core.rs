//! Shared application state: SQLite, asset files, settings, provider selection,
//! and the one transactional path through which the canvas changes.
use crate::catalog::Catalog;
use crate::db;
use crate::error::{AppError, Res};
use crate::graph::{self, Ctx, Graph, Op, Outcome};
use crate::provider::{hex, ChatProvider, ImageProvider, MockProvider, ModelArk, VideoProvider};
use crate::secrets::{self, SecretStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub trait Emitter: Send + Sync {
    fn emit(&self, event: &str, payload: Value);
}

pub struct NullEmitter;
impl Emitter for NullEmitter {
    fn emit(&self, _: &str, _: Value) {}
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub struct ProviderSet {
    pub chat: Arc<dyn ChatProvider>,
    pub image: Arc<dyn ImageProvider>,
    pub video: Arc<dyn VideoProvider>,
    /// "modelark" or "mock" — recorded with every result.
    pub name: String,
    pub region: String,
}

pub enum Actor {
    User,
    Agent { turn_id: String, tool_call_id: String, tool: String, thread_id: Option<String> },
}

pub struct PatchResult {
    pub revision: i64,
    pub outcome: Outcome,
}

pub struct CanvasRow {
    pub revision: i64,
    pub graph: Graph,
    pub viewport: Value,
}

pub struct AssetFile {
    pub id: String,
    pub rel_path: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub sha: String,
}

pub struct Core {
    db: Arc<Mutex<Connection>>,
    pub api_log: Arc<crate::apilog::ApiLog>,
    pub data_dir: PathBuf,
    pub emitter: Arc<dyn Emitter>,
    pub secrets: Arc<dyn SecretStore>,
    pub catalog: Catalog,
    pub cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub mock: Arc<MockProvider>,
    /// Tests only: point the real ModelArk client at a local fake server.
    pub base_override: Mutex<Option<String>>,
}

pub const PROFILE: &str = "default";

impl Core {
    pub fn open(data_dir: PathBuf, emitter: Arc<dyn Emitter>, secrets: Arc<dyn SecretStore>, mock_delay_ms: u64) -> Res<Core> {
        std::fs::create_dir_all(data_dir.join("assets"))?;
        let conn = Connection::open(data_dir.join("seed-studio.sqlite"))?;
        db::migrate(&conn)?;
        // Anything that was in flight when the app last closed has an unknown outcome.
        conn.execute("UPDATE node_runs SET status='uncertain', error='The app closed during the request; the outcome is unknown', finished_at=?1 WHERE status='sending'", params![now_ms()])?;
        conn.execute("UPDATE runs SET status='interrupted' WHERE status='running'", [])?;
        conn.execute("UPDATE videos SET status='uncertain', error='The app closed while this request was being sent; the outcome is unknown', updated_at=?1 WHERE status='submitting'", params![now_ms()])?;
        conn.execute("UPDATE messages SET status='incomplete' WHERE status='streaming'", [])?;
        crate::apilog::close_open_rows(&conn)?;
        let db = Arc::new(Mutex::new(conn));
        Ok(Core {
            api_log: crate::apilog::ApiLog::new(db.clone()),
            db,
            data_dir,
            emitter,
            secrets,
            catalog: Catalog::load(),
            cancels: Mutex::new(HashMap::new()),
            mock: Arc::new(MockProvider { delay_ms: mock_delay_ms, ..Default::default() }),
            base_override: Mutex::new(None),
        })
    }

    /// Run a closure with the connection. Never hold this across an await.
    pub fn db<T>(&self, f: impl FnOnce(&mut Connection) -> Res<T>) -> Res<T> {
        let mut guard = self.db.lock().unwrap();
        f(&mut guard)
    }

    pub fn emit(&self, event: &str, payload: Value) {
        self.emitter.emit(event, payload);
    }

    // ── settings ──
    pub fn setting(&self, key: &str) -> Option<String> {
        self.db(|c| Ok(c.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?)).ok().flatten()
    }
    pub fn set_setting(&self, key: &str, value: &str) -> Res<()> {
        self.db(|c| {
            c.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
            Ok(())
        })
    }
    pub fn provider_mode(&self) -> String {
        self.setting("provider_mode").unwrap_or_else(|| "modelark".into())
    }
    pub fn region(&self) -> String {
        self.setting("region").unwrap_or_else(|| "ap-southeast-1".into())
    }
    pub fn chat_model_key(&self) -> String {
        self.setting("chat_model").filter(|k| self.catalog.model(k).is_some()).unwrap_or_else(|| self.catalog.suggested("chat"))
    }
    pub fn image_model_key(&self) -> String {
        self.setting("image_model").filter(|k| self.catalog.model(k).is_some()).unwrap_or_else(|| self.catalog.suggested("image"))
    }

    /// The id that will actually be sent for a catalog key: the owner's explicit
    /// endpoint id, else a candidate seen in this region's account, else the documented id.
    pub fn resolve_model_id(&self, key: &str) -> Res<String> {
        let model = self.catalog.model(key).ok_or_else(|| AppError::new("unknown_model", format!("Unknown model: {key}")))?;
        // A gateway that refuses endpoint ids gets the documented model id instead.
        let endpoints_ok = self.gateway()["acceptsEndpointIds"].as_bool().unwrap_or(true);
        if let Some(ep) = self.setting(&format!("endpoint:{key}")).filter(|s| endpoints_ok && !s.trim().is_empty()) {
            return Ok(ep.trim().to_string());
        }
        let region = self.region();
        let visible: HashSet<String> = self.db(|c| {
            let mut st = c.prepare("SELECT model_id FROM model_status WHERE region=?1 AND status IN ('visible','tried')")?;
            let rows = st.query_map([&region], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
            Ok(rows)
        })?;
        Ok(model.candidate_ids.iter().find(|id| visible.contains(*id)).unwrap_or(&model.candidate_ids[0]).clone())
    }

    /// Whether a key is saved for a region. Answered from a non-secret flag so that
    /// opening the app never reads the Keychain (which can raise a system prompt);
    /// the secret itself is read only when a request is actually sent.
    pub fn has_key(&self, region: &str) -> bool {
        match self.setting(&Self::key_flag(region)).as_deref() {
            Some("1") => true,
            Some(_) => false,
            None => {
                let found = matches!(self.secrets.get(&secrets::account(PROFILE, region)), Ok(Some(_)));
                self.note_key(region, found);
                found
            }
        }
    }
    fn key_flag(region: &str) -> String {
        format!("key_saved:{PROFILE}:{region}")
    }
    pub fn note_key(&self, region: &str, saved: bool) {
        let _ = self.set_setting(&Self::key_flag(region), if saved { "1" } else { "0" });
    }

    /// Rules of the selected connection when it is a gateway (Null for a direct region).
    pub fn gateway(&self) -> Value {
        self.catalog.raw["gateways"][self.region().as_str()].clone()
    }

    pub fn providers(&self) -> Res<ProviderSet> {
        if self.provider_mode() == "mock" {
            return Ok(ProviderSet { chat: self.mock.clone(), image: self.mock.clone(), video: self.mock.clone(), name: "mock".into(), region: "-".into() });
        }
        let region = self.region();
        let overridden = self.base_override.lock().unwrap().clone();
        let test_balance = overridden.as_ref().map(|b| b.replace("/api/v3", "/abpp/balance"));
        let test_assets = overridden.as_ref().map(|b| b.replace("/api/v3", "/abpp/assets"));
        let base = match overridden {
            Some(b) => b,
            None => self.catalog.regions.get(&region).cloned()
                .ok_or_else(|| AppError::new("unknown_region", format!("Unknown region: {region}")))?,
        };
        let found = self.secrets.get(&secrets::account(PROFILE, &region))?;
        self.note_key(&region, found.is_some());
        let key = found
            .ok_or_else(|| AppError::new("no_key", format!("No ModelArk API key is saved for {region}. Add it in Settings, or switch to Simulation to try the app offline.")))?;
        let gw = self.gateway();
        let client = Arc::new(ModelArk::new(&base, key).gateway(
            gw["maxCompletionTokens"].as_u64(),
            gw["balanceUrl"].as_str().map(|u| test_balance.unwrap_or_else(|| u.to_string())),
            gw["assetsUrl"].as_str().map(|u| test_assets.unwrap_or_else(|| u.to_string())),
        ).logged(self.api_log.clone(), &region));
        Ok(ProviderSet { chat: client.clone(), image: client.clone(), video: client, name: "modelark".into(), region })
    }

    pub fn mark_tried(&self, set: &ProviderSet, model_id: &str) {
        if set.name == "modelark" {
            let _ = self.db(|c| {
                c.execute("INSERT INTO model_status(region,model_id,status,checked_at) VALUES(?1,?2,'tried',?3)
                           ON CONFLICT(region,model_id) DO UPDATE SET status='tried', checked_at=excluded.checked_at",
                    params![set.region, model_id, now_ms()])?;
                Ok(())
            });
        }
    }
    pub fn mark_needs_activation(&self, set: &ProviderSet, model_id: &str) {
        if set.name == "modelark" {
            let _ = self.db(|c| {
                c.execute("INSERT INTO model_status(region,model_id,status,checked_at) VALUES(?1,?2,'needs_activation',?3)
                           ON CONFLICT(region,model_id) DO UPDATE SET status='needs_activation', checked_at=excluded.checked_at
                           WHERE model_status.status!='tried'",
                    params![set.region, model_id, now_ms()])?;
                Ok(())
            });
        }
    }

    // ── canvas ──
    pub fn load_canvas(c: &Connection, project_id: &str) -> Res<CanvasRow> {
        let exists: Option<i64> = c.query_row("SELECT 1 FROM projects WHERE id=?1", [project_id], |r| r.get(0)).optional()?;
        if exists.is_none() {
            return Err(AppError::new("not_found", "Project not found"));
        }
        let row: Option<(i64, String, String)> = c.query_row(
            "SELECT revision, graph_json, viewport_json FROM canvases WHERE project_id=?1", [project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        match row {
            Some((revision, g, v)) => Ok(CanvasRow { revision, graph: serde_json::from_str(&g)?, viewport: serde_json::from_str(&v)? }),
            None => {
                let viewport = json!({"x": 40, "y": 40, "zoom": 0.8});
                c.execute("INSERT INTO canvases(project_id,revision,schema_version,graph_json,viewport_json,updated_at) VALUES(?1,0,1,?2,?3,?4)",
                    params![project_id, serde_json::to_string(&Graph::default())?, viewport.to_string(), now_ms()])?;
                Ok(CanvasRow { revision: 0, graph: Graph::default(), viewport })
            }
        }
    }

    pub fn asset_ids(c: &Connection, project_id: &str) -> Res<HashSet<String>> {
        let mut st = c.prepare("SELECT id FROM assets WHERE project_id=?1")?;
        let rows = st.query_map([project_id], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The only way the graph changes. Same validation for the user and the agent;
    /// graph, undo entry and agent log are written in one SQLite transaction.
    pub fn apply_patch(&self, project_id: &str, expected: Option<i64>, ops: &[Op], actor: &Actor, label: &str) -> Res<PatchResult> {
        self.db(|c| {
            let tx = c.transaction()?;
            let row = Self::load_canvas(&tx, project_id)?;
            if let Some(e) = expected {
                if e != row.revision {
                    return Err(AppError::new("conflict", format!("The canvas changed (current revision {}). Read the canvas and try again.", row.revision))
                        .with(json!({"currentRevision": row.revision})));
                }
            }
            let assets = Self::asset_ids(&tx, project_id)?;
            let ctx = Ctx { catalog: &self.catalog, asset_ids: &assets };
            let (g, outcome) = graph::apply_ops(&row.graph, ops, &ctx)
                .map_err(|e| AppError::new("invalid_patch", e.message).with(json!({"opIndex": e.op_index})))?;
            let revision = row.revision + 1;
            let (actor_name, group) = match actor {
                Actor::User => ("user", None),
                Actor::Agent { turn_id, tool_call_id, tool, thread_id } => {
                    let n = tx.execute(
                        "INSERT OR IGNORE INTO agent_actions(id,project_id,thread_id,turn_id,tool_call_id,tool,rev_before,rev_after,summary,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                        params![new_id(), project_id, thread_id, turn_id, tool_call_id, tool, row.revision, revision, outcome.summary(), now_ms()])?;
                    if n == 0 {
                        return Err(AppError::new("duplicate_tool_call", "Duplicate tool call; ignored"));
                    }
                    ("agent", Some(turn_id.clone()))
                }
            };
            let top: Option<Option<String>> = tx.query_row(
                "SELECT group_id FROM canvas_history WHERE project_id=?1 AND stack='undo' ORDER BY id DESC LIMIT 1",
                [project_id], |r| r.get(0)).optional()?;
            let merge = group.is_some() && top.flatten() == group;
            if !merge {
                tx.execute("INSERT INTO canvas_history(project_id,stack,graph_json,label,actor,group_id) VALUES(?1,'undo',?2,?3,?4,?5)",
                    params![project_id, serde_json::to_string(&row.graph)?, label, actor_name, group])?;
                tx.execute("DELETE FROM canvas_history WHERE project_id=?1 AND stack='undo' AND id NOT IN
                            (SELECT id FROM canvas_history WHERE project_id=?1 AND stack='undo' ORDER BY id DESC LIMIT 100)", [project_id])?;
            }
            tx.execute("DELETE FROM canvas_history WHERE project_id=?1 AND stack='redo'", [project_id])?;
            tx.execute("UPDATE canvases SET graph_json=?1, revision=?2, updated_at=?3 WHERE project_id=?4",
                params![serde_json::to_string(&g)?, revision, now_ms(), project_id])?;
            tx.commit()?;
            Ok(PatchResult { revision, outcome })
        })
    }

    /// Undo/redo swap whole structural snapshots. Viewport and saved images are untouched.
    pub fn undo_redo(&self, project_id: &str, undo: bool) -> Res<String> {
        let (from, to) = if undo { ("undo", "redo") } else { ("redo", "undo") };
        self.db(|c| {
            let tx = c.transaction()?;
            let row = Self::load_canvas(&tx, project_id)?;
            let top: Option<(i64, String, String, String, Option<String>)> = tx.query_row(
                "SELECT id, graph_json, label, actor, group_id FROM canvas_history WHERE project_id=?1 AND stack=?2 ORDER BY id DESC LIMIT 1",
                params![project_id, from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?;
            let Some((id, graph_json, label, actor, group)) = top else {
                return Err(AppError::new("nothing_to_undo", if undo { "Nothing to undo" } else { "Nothing to redo" }));
            };
            tx.execute("DELETE FROM canvas_history WHERE id=?1", [id])?;
            // A redone agent batch must not merge with a later turn's group.
            tx.execute("INSERT INTO canvas_history(project_id,stack,graph_json,label,actor,group_id) VALUES(?1,?2,?3,?4,?5,?6)",
                params![project_id, to, serde_json::to_string(&row.graph)?, label, actor, group.map(|g| format!("{g}:done"))])?;
            tx.execute("UPDATE canvases SET graph_json=?1, revision=?2, updated_at=?3 WHERE project_id=?4",
                params![graph_json, row.revision + 1, now_ms(), project_id])?;
            tx.commit()?;
            Ok(label)
        })
    }

    pub fn canvas_state(&self, project_id: &str) -> Res<Value> {
        self.db(|c| {
            let row = Self::load_canvas(c, project_id)?;
            let top = |stack: &str| -> Res<Option<(String, String)>> {
                Ok(c.query_row("SELECT label, actor FROM canvas_history WHERE project_id=?1 AND stack=?2 ORDER BY id DESC LIMIT 1",
                    params![project_id, stack], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
            };
            let undo = top("undo")?;
            let redo = top("redo")?;
            let mut results: HashMap<String, Value> = HashMap::new();
            let mut st = c.prepare("SELECT node_id, status, asset_id, text_output, error, provider, model_id, id, started_at, usage_json FROM node_runs WHERE project_id=?1 ORDER BY started_at ASC, rowid ASC")?;
            let mut rows = st.query([project_id])?;
            while let Some(r) = rows.next()? {
                let node_id: String = r.get(0)?;
                let status: String = r.get(1)?;
                let entry = results.entry(node_id).or_insert_with(|| json!({"versions": []}));
                entry["status"] = json!(status);
                entry["error"] = json!(r.get::<_, Option<String>>(4)?);
                entry["provider"] = json!(r.get::<_, String>(5)?);
                entry["modelId"] = json!(r.get::<_, String>(6)?);
                entry["startedAt"] = json!(r.get::<_, i64>(8)?);
                entry["usage"] = r.get::<_, Option<String>>(9)?.and_then(|u| serde_json::from_str::<Value>(&u).ok()).unwrap_or(Value::Null);
                if status == "succeeded" {
                    if let Some(a) = r.get::<_, Option<String>>(2)? {
                        // The newest run is the current image; choosing an older version re-records it as newest.
                        entry["assetId"] = json!(a);
                        let list = entry["versions"].as_array_mut().unwrap();
                        if !list.contains(&json!(a)) { list.push(json!(a)); }
                    }
                    if let Some(t) = r.get::<_, Option<String>>(3)? {
                        entry["text"] = json!(t);
                    }
                }
            }
            Ok(json!({
                "projectId": project_id, "revision": row.revision, "graph": row.graph, "viewport": row.viewport,
                "results": results,
                "undo": undo.map(|(l, a)| json!({"label": l, "actor": a})),
                "redo": redo.map(|(l, a)| json!({"label": l, "actor": a})),
            }))
        })
    }

    // ── assets ──
    /// Validate that bytes are a supported image and write them atomically.
    pub fn write_asset_file(&self, project_id: &str, bytes: &[u8]) -> Res<AssetFile> {
        let bad = || AppError::new("bad_image", "The file is not a valid PNG, JPEG or WebP image");
        let (mime, ext) = match image::guess_format(bytes).map_err(|_| bad())? {
            image::ImageFormat::Png => ("image/png", "png"),
            image::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
            image::ImageFormat::WebP => ("image/webp", "webp"),
            _ => return Err(bad()),
        };
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()
            .and_then(|r| r.into_dimensions().ok()).unwrap_or((0, 0));
        if ext != "webp" && width == 0 {
            return Err(bad());
        }
        let id = new_id();
        let dir = self.data_dir.join("assets").join(project_id);
        std::fs::create_dir_all(&dir)?;
        let name = format!("{id}.{ext}");
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, dir.join(&name))?;
        Ok(AssetFile { id, rel_path: format!("assets/{project_id}/{name}"), mime: mime.into(), width, height, sha: hex(&Sha256::digest(bytes)) })
    }

    pub fn insert_reference_asset(&self, project_id: &str, f: &AssetFile, title: &str) -> Res<()> {
        self.db(|c| {
            c.execute("INSERT INTO assets(id,project_id,kind,rel_path,mime,width,height,sha256,title,created_at) VALUES(?1,?2,'reference',?3,?4,?5,?6,?7,?8,?9)",
                params![f.id, project_id, f.rel_path, f.mime, f.width, f.height, f.sha, title, now_ms()])?;
            Ok(())
        })
    }

    /// (project_id, mime, sha256, bytes) for an asset id.
    pub fn asset_bytes(&self, id: &str) -> Res<(String, String, String, Vec<u8>)> {
        // "<id>~t": a small preview of an image, made once and kept next to the original.
        if let Some(base) = id.strip_suffix("~t") {
            let (project, mime, sha, bytes) = self.asset_bytes(base)?;
            if !mime.starts_with("image/") { return Ok((project, mime, sha, bytes)); }
            let dir = self.data_dir.join("assets").join(&project).join("previews");
            let file = dir.join(format!("{base}.jpg"));
            if let Ok(small) = std::fs::read(&file) {
                return Ok((project, "image/jpeg".into(), sha, small));
            }
            let Ok(img) = image::load_from_memory(&bytes) else { return Ok((project, mime, sha, bytes)) };
            if img.color().has_alpha() || (img.width() <= 900 && img.height() <= 900) {
                return Ok((project, mime, sha, bytes)); // keep transparency and already-small images as they are
            }
            let mut out = std::io::Cursor::new(vec![]);
            let small = img.thumbnail(900, 900).to_rgb8();
            if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82).encode_image(&small).is_err() {
                return Ok((project, mime, sha, bytes));
            }
            let small = out.into_inner();
            let _ = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &small));
            return Ok((project, "image/jpeg".into(), sha, small));
        }
        if id == "__probe" {
            // Startup self-check: lets the UI confirm the local image channel works.
            let mut png = std::io::Cursor::new(vec![]);
            image::RgbaImage::new(1, 1).write_to(&mut png, image::ImageFormat::Png).map_err(|_| AppError::new("io", "probe"))?;
            return Ok((String::new(), "image/png".into(), String::new(), png.into_inner()));
        }
        let (project, rel, mime, sha): (String, String, String, String) = self.db(|c| {
            let image = c.query_row("SELECT project_id, rel_path, mime, sha256 FROM assets WHERE id=?1", [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
            if let Some(found) = image {
                return Ok(found);
            }
            // Saved video files are served through the same local channel.
            let video = c.query_row("SELECT project_id, rel_path FROM videos WHERE id=?1 AND rel_path IS NOT NULL", [id],
                |r| Ok((r.get(0)?, r.get(1)?, "video/mp4".to_string(), String::new()))).optional()?;
            if let Some(found) = video {
                return Ok(found);
            }
            c.query_row("SELECT project_id, rel_path FROM audio WHERE id=?1", [id],
                |r| Ok((r.get(0)?, r.get(1)?, "audio/wav".to_string(), String::new()))).optional()?
                .ok_or_else(|| AppError::new("not_found", "File not found"))
        })?;
        let bytes = std::fs::read(self.data_dir.join(&rel))
            .map_err(|_| AppError::new("missing_file", "The image file is missing from the project folder"))?;
        Ok((project, mime, sha, bytes))
    }

    pub fn cancel_flag(&self, key: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(key.to_string(), flag.clone());
        flag
    }
    pub fn cancel(&self, key: &str) {
        if let Some(f) = self.cancels.lock().unwrap().get(key) {
            f.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
