//! The gateway's private character library. Seedance refuses an input image with a
//! realistic face unless it is sent as `asset://<id>` from this library, so an image
//! whose name matches a library entry is sent by reference instead of as a file.
//! The list (ids and names only, no secret) is cached in settings per connection.
use crate::core::{now_ms, Core};
use crate::error::{AppError, Res};
use serde_json::{json, Value};

const FRESH_MS: i64 = 10 * 60 * 1000;
const MAX_UPLOAD: usize = 20 * 1024 * 1024;
const UPLOAD_TYPES: [&str; 6] = ["image/jpeg", "image/png", "image/webp", "image/gif", "image/tiff", "image/heic"];
/// How often a new entry's status is read while BytePlus processes it (shortened in tests).
pub static POLL_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(3000);
const WAIT_MS: i64 = 90_000;

/// The owner's switch for this connection: "my reference images are AI-generated; add new
/// ones to the gateway library". Only then is anything uploaded, with the declaration header.
pub fn uploads_on(core: &Core) -> bool {
    available(core) && core.setting(&format!("library_upload:{}", core.region())).as_deref() == Some("1")
}

fn map_key(core: &Core) -> String {
    format!("gateway_library_map:{}", core.region())
}
/// Images this app added itself: local image id → library id, so a match never depends on the name.
pub fn uploaded(core: &Core) -> serde_json::Map<String, Value> {
    core.setting(&map_key(core)).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// The entry a video sends for one image: the node's own choice, else the entry this app
/// uploaded for it, else a match by name.
pub fn resolve(core: &Core, lib: &[Value], asset_id: &str, choice: Option<&str>, names: &[&str]) -> Result<Option<Value>, String> {
    if choice.filter(|c| !c.is_empty()).is_none() {
        if let Some(a) = uploaded(core).get(asset_id).and_then(|id| lib.iter().find(|a| &a["id"] == id)) {
            if a["status"].as_str().map(|s| s.eq_ignore_ascii_case("active")).unwrap_or(true) { return Ok(Some(a.clone())); }
        }
    }
    pick(lib, choice, names)
}

/// Add one project image to the gateway library and wait until BytePlus has it ready.
/// One POST, never repeated on an unknown outcome; the status reads are GETs.
pub async fn upload(core: &Core, set: &crate::core::ProviderSet, asset_id: &str) -> Res<Value> {
    let bad = |m: String| AppError::new("library_upload", m);
    if !uploads_on(core) { return Err(bad("Adding images to the gateway library is switched off in Settings.".into())); }
    let (_, mime, _, bytes) = core.asset_bytes(asset_id)?;
    let title: String = core.db(|c| Ok(c.query_row("SELECT title FROM assets WHERE id=?1", [asset_id], |r| r.get(0)).unwrap_or_default()))?;
    let name = if norm(&title).is_empty() { asset_id.to_string() } else { title };
    if !UPLOAD_TYPES.contains(&mime.as_str()) { return Err(bad(format!("“{name}” is {mime}; the gateway library takes JPEG, PNG, WebP, GIF, TIFF or HEIC."))); }
    if bytes.len() > MAX_UPLOAD { return Err(bad(format!("“{name}” is larger than 20 MB, the gateway library’s limit."))); }
    let mut tries = 0;
    let made = loop {
        tries += 1;
        match set.chat.library_upload(&name, &mime, bytes.clone(), true).await {
            Ok(v) => break v,
            // A definite "not yet" from the gateway (nothing was stored): the one case that is tried again.
            Err(e) if e.code.as_deref() == Some("abpp_auto_group_pending") && tries < 4 => tokio::time::sleep(std::time::Duration::from_secs(5)).await,
            Err(e) => {
                let why = match e.code.as_deref() {
                    Some("abpp_asset_quota") => "The gateway library is full. Old entries have to be deleted at the gateway before new images can be added. ",
                    Some("abpp_staging_full") | Some("abpp_auto_group_pending") => "The gateway is busy preparing the library. Nothing was added; try again in a minute. ",
                    Some("abpp_attestation_required") => "The gateway did not accept the AI-generated declaration. ",
                    _ if e.kind == crate::provider::ErrKind::Uncertain => "The upload to the gateway library was cut off, so it is not known whether the image was added. It was not sent again; refresh the library list in the image node’s panel before generating. ",
                    _ => "The image could not be added to the gateway library. ",
                };
                return Err(bad(format!("{why}No video was requested. {}", e.to_app(&set.region, "library upload").message)));
            }
        }
    };
    let id = made["id"].as_str().map(String::from)
        .or_else(|| made["uri"].as_str().map(|u| u.trim_start_matches("asset://").to_string()))
        .ok_or_else(|| bad("The gateway accepted the image but returned no id. Refresh the library list before generating.".into()))?;
    // Remember it at once, so a later request finds it even if waiting is interrupted.
    let mut map = uploaded(core);
    map.insert(asset_id.to_string(), json!(id));
    core.set_setting(&map_key(core), &Value::Object(map).to_string())?;
    let (mut status, start) = (made["status"].as_str().unwrap_or("Processing").to_string(), now_ms());
    while !status.eq_ignore_ascii_case("active") {
        if status.eq_ignore_ascii_case("failed") { break; }
        if now_ms() - start > WAIT_MS { return Err(bad(format!("“{name}” was added to the gateway library but is still being processed after 90 seconds. No video was requested; generate again in a moment."))); }
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MS.load(std::sync::atomic::Ordering::Relaxed))).await;
        if let Ok(v) = set.chat.library_asset(&id).await {
            status = v["status"].as_str().unwrap_or("Processing").to_string();
            if status.eq_ignore_ascii_case("failed") {
                let why = v["error"].as_str().map(String::from).or_else(|| v["error"]["message"].as_str().map(String::from)).unwrap_or_default();
                return Err(bad(format!("BytePlus did not accept “{name}” into the gateway library. No video was requested. {why}")));
            }
        }
    }
    if status.eq_ignore_ascii_case("failed") { return Err(bad(format!("BytePlus did not accept “{name}” into the gateway library. No video was requested."))); }
    Ok(json!({"id": id, "name": name}))
}

fn cache_key(core: &Core) -> String {
    format!("gateway_library:{}", core.region())
}

/// Whether the selected connection has a library at all.
pub fn available(core: &Core) -> bool {
    core.provider_mode() != "mock" && core.gateway()["assetsUrl"].is_string()
}

/// A file name reduced to what identifies it: no extension, case, spaces or punctuation.
pub fn norm(name: &str) -> String {
    let stem = match name.rsplit_once('.') {
        Some((a, ext)) if !a.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => a,
        _ => name,
    };
    stem.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// The gateway's answer, whatever wraps the list, as `{id, name, status, group}` rows.
fn parse(v: &Value) -> Vec<Value> {
    let list = [v, &v["assets"], &v["data"], &v["items"], &v["data"]["assets"], &v["data"]["items"]]
        .into_iter().find_map(|x| x.as_array()).cloned().unwrap_or_default();
    list.iter().filter_map(|a| {
        let id = a["id"].as_str().or_else(|| a["asset_id"].as_str())?.trim_start_matches("asset://").to_string();
        let name = a["name"].as_str().unwrap_or("").to_string();
        let status = a["status"].as_str().unwrap_or("Active").to_string();
        let group = a["group_name"].as_str().or_else(|| a["group"]["name"].as_str()).or_else(|| a["group_id"].as_str()).unwrap_or("");
        Some(json!({"id": id, "name": if name.is_empty() { id.clone() } else { name }, "status": status, "group": group}))
    }).collect()
}

fn stored(core: &Core) -> Value {
    core.setting(&cache_key(core)).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

/// The cached list for the selected connection (empty when there is none).
pub fn cached(core: &Core) -> Vec<Value> {
    if !available(core) { return vec![]; }
    stored(core)["assets"].as_array().cloned().unwrap_or_default()
}

/// Read the list from the gateway (a GET, safe to repeat) and cache it.
pub async fn refresh(core: &Core) -> Res<Value> {
    if !available(core) { return Err(AppError::new("no_library", "This connection has no character library.")); }
    let set = core.providers()?;
    let v = set.chat.library().await.map_err(|e| e.to_app(&set.region, "GET assets"))?;
    let out = json!({"assets": parse(&v), "fetchedAt": now_ms()});
    core.set_setting(&cache_key(core), &out.to_string())?;
    Ok(out)
}

/// Refresh before a video is planned when the cache is old. A failure leaves the cache as it is.
pub async fn ensure(core: &Core) {
    if available(core) && core.has_key(&core.region()) && now_ms() - stored(core)["fetchedAt"].as_i64().unwrap_or(0) > FRESH_MS {
        let _ = refresh(core).await;
    }
}

/// The library entry for one image. `choice`: an entry id, "off", or nothing for a match by name.
pub fn pick(lib: &[Value], choice: Option<&str>, names: &[&str]) -> Result<Option<Value>, String> {
    let usable = |a: &&Value| a["status"].as_str().map(|s| s.eq_ignore_ascii_case("active")).unwrap_or(true);
    match choice.filter(|c| !c.is_empty()) {
        Some("off") => Ok(None),
        Some(id) => match lib.iter().find(|a| a["id"] == id) {
            Some(a) if usable(&a) => Ok(Some(a.clone())),
            Some(a) => Err(format!("The library image “{}” is not active yet ({}).", a["name"].as_str().unwrap_or(id), a["status"].as_str().unwrap_or("?"))),
            None => Err("The chosen library image is no longer in the gateway’s library. Pick another one in the image node’s panel.".into()),
        },
        None => {
            let wanted: Vec<String> = names.iter().map(|n| norm(n)).filter(|n| !n.is_empty()).collect();
            Ok(lib.iter().filter(usable).find(|a| wanted.contains(&norm(a["name"].as_str().unwrap_or("")))).cloned())
        }
    }
}

/// The list plus, for each image node on the canvas, the entry a video would send.
pub fn matches(core: &Core, project_id: &str) -> Res<Value> {
    let lib = cached(core);
    let mut by_node = serde_json::Map::new();
    if !lib.is_empty() {
        let row = core.db(|c| Core::load_canvas(c, project_id))?;
        for n in row.graph.nodes.iter().filter(|n| n.kind == "imageAsset") {
            let text = |k: &str| n.data.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let asset_title: String = core.db(|c| Ok(c.query_row("SELECT title FROM assets WHERE id=?1", [text("assetId")], |r| r.get(0)).unwrap_or_default()))?;
            if let Ok(Some(a)) = resolve(core, &lib, &text("assetId"), Some(text("library").as_str()), &[&text("title"), &asset_title]) {
                by_node.insert(n.id.clone(), a);
            }
        }
    }
    // Images this app added itself and that are still in the list, by local image id.
    let mine: serde_json::Map<String, Value> = uploaded(core).into_iter().filter(|(_, id)| lib.iter().any(|a| &a["id"] == id)).collect();
    Ok(json!({"available": available(core), "uploads": uploads_on(core), "uploaded": mine, "assets": lib, "byNode": by_node, "fetchedAt": stored(core)["fetchedAt"]}))
}
