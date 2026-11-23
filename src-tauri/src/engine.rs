//! The one execution engine shared by the canvas and the chat.
//! `build_plan` is local and free; `execute` is the only place paid calls start.
use crate::core::{new_id, now_ms, Core, ProviderSet};
use crate::error::{AppError, Res};
use crate::graph::{is_executable, Graph, Node};
use crate::provider::{hex, ChatRequest, ErrKind, ImageRequest, ProviderError};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "from", rename_all = "camelCase")]
pub enum RefSrc {
    #[serde(rename_all = "camelCase")]
    Asset { asset_id: String },
    #[serde(rename_all = "camelCase")]
    Node { node_id: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub node_id: String,
    pub title: String,
    /// "image" or "text"
    pub kind: String,
    pub model_key: String,
    pub model_label: String,
    pub model_id: String,
    /// "new" (a provider call), "reuse" (saved result still valid) or "blocked".
    pub action: String,
    pub reason: Option<String>,
    pub fingerprint: String,
    pub final_prompt: Option<String>,
    pub text_from: Option<String>,
    pub instructions: Option<String>,
    pub refs: Vec<RefSrc>,
    pub settings: Map<String, Value>,
    pub deps: Vec<String>,
    pub reuse_asset_id: Option<String>,
    pub reuse_text: Option<String>,
    /// Explicit pixels that will be sent when an aspect ratio is chosen.
    #[serde(default)]
    pub pixel_size: Option<String>,
    /// Camera text copied from another image; added when the prompt arrives from a text node at run time.
    #[serde(default)]
    pub camera: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub project_id: String,
    pub revision: i64,
    pub targets: Vec<String>,
    pub force_new: bool,
    pub provider: String,
    pub region: String,
    pub items: Vec<PlanItem>,
    pub new_images: usize,
    pub new_texts: usize,
    pub reused: usize,
    pub blocked: usize,
    pub by_model: Vec<Value>,
    pub shared_refs: bool,
}

impl Plan {
    fn signature(&self) -> Vec<(String, String, String)> {
        self.items.iter().map(|i| (i.node_id.clone(), i.action.clone(), i.fingerprint.clone())).collect()
    }
}

fn title_of(n: &Node) -> String {
    n.data.get("title").and_then(|v| v.as_str()).unwrap_or(&n.id).to_string()
}

fn has_alpha(bytes: &[u8]) -> bool {
    image::load_from_memory(bytes).map(|i| i.color().has_alpha()).unwrap_or(false)
}

/// Executable nodes needed for the targets, dependencies first.
fn execution_order(g: &Graph, targets: &[String]) -> Vec<String> {
    let mut need: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![];
    for t in targets {
        match g.node(t) {
            Some(n) if n.kind == "preview" => stack.extend(g.incoming(t, "image").iter().map(|e| e.source.clone())),
            Some(_) => stack.push(t.clone()),
            None => {}
        }
    }
    while let Some(id) = stack.pop() {
        let Some(n) = g.node(&id) else { continue };
        if !is_executable(&n.kind) || !need.insert(id.clone()) {
            continue;
        }
        stack.extend(g.edges.iter().filter(|e| e.target == id).map(|e| e.source.clone()));
    }
    let mut order = vec![];
    let mut done: HashSet<String> = HashSet::new();
    // Graph is acyclic (validator), so this terminates.
    while order.len() < need.len() {
        for n in &g.nodes {
            if need.contains(&n.id) && !done.contains(&n.id) {
                let ready = g.edges.iter().filter(|e| e.target == n.id && need.contains(&e.source)).all(|e| done.contains(&e.source));
                if ready {
                    done.insert(n.id.clone());
                    order.push(n.id.clone());
                }
            }
        }
    }
    order
}

/// Pure planning. Takes the DB lock only for short single queries (the lock is not reentrant).
fn plan_in(core: &Core, project_id: &str, targets: &[String], force_new: bool) -> Res<Plan> {
    let row = core.db(|c| Core::load_canvas(c, project_id))?;
    let g = &row.graph;
    let mode = core.provider_mode();
    let (provider, region) = if mode == "mock" { ("mock".to_string(), "-".to_string()) } else { ("modelark".to_string(), core.region()) };
    let force_set: HashSet<String> = if force_new {
        targets.iter().flat_map(|t| match g.node(t) {
            Some(n) if n.kind == "preview" => g.incoming(t, "image").iter().map(|e| e.source.clone()).collect::<Vec<_>>(),
            _ => vec![t.clone()],
        }).collect()
    } else {
        HashSet::new()
    };
    let mut items: Vec<PlanItem> = vec![];
    let mut fps: HashMap<String, String> = HashMap::new();
    let mut fresh: HashSet<String> = HashSet::new();

    for id in execution_order(g, targets) {
        let node = g.node(&id).unwrap();
        let is_image = node.kind == "seedream";
        // Describe: an image in, text out. Its instruction is the prompt; the image rides as a reference.
        let is_describe = node.kind == "describe";
        let model_key = node.data.get("modelKey").and_then(|v| v.as_str()).map(String::from)
            .unwrap_or_else(|| if is_image { core.image_model_key() } else { core.chat_model_key() });
        let mut reason: Option<String> = None;
        let mut block = |r: String| { if reason.is_none() { reason = Some(r); } };
        let model_label = core.catalog.model(&model_key).map(|m| m.label.clone()).unwrap_or_else(|| model_key.clone());
        let model_id = core.resolve_model_id(&model_key).unwrap_or_else(|e| { block(e.message); String::new() });
        let settings = node.data.get("settings").and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let mut deps = vec![];

        // prompt input
        let (mut final_prompt, mut text_from, text_component) = (None, None, match g.incoming(&id, "prompt").first() {
            None if is_describe => {
                let t = node.data.get("instructions").and_then(|v| v.as_str()).map(str::trim).filter(|t| !t.is_empty()).unwrap_or(DESCRIBE_DEFAULT);
                format!("lit:{t}")
            }
            None => {
                // The node's own prompt, written in its panel.
                let own = node.data.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                if own.is_empty() { block("Write a prompt for this node (double-click it), or connect a Prompt node".into()); }
                format!("lit:{own}")
            }
            Some(e) => {
                let src = g.node(&e.source).unwrap();
                if src.kind == "prompt" {
                    let t = src.data.get("text").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                    if t.is_empty() { block(format!("The prompt node “{}” is empty", title_of(src))); }
                    format!("lit:{t}")
                } else {
                    deps.push(src.id.clone());
                    format!("node:{}", fps.get(&src.id).cloned().unwrap_or_default())
                }
            }
        });
        if let Some(t) = text_component.strip_prefix("lit:") {
            final_prompt = Some(t.to_string());
        } else if let Some(d) = deps.first() {
            text_from = Some(d.clone());
        }

        // A camera copied from another image: text only, added to whatever the prompt is.
        let camera = if is_image { node.data.get("camera").and_then(|v| v.as_str()).map(str::trim).filter(|c| !c.is_empty()).map(crate::look::camera_suffix) } else { None };
        let text_component = match &camera { Some(c) => format!("{text_component}|camera:{c}"), None => text_component };

        // ordered references
        let mut refs = vec![];
        let mut ref_components = vec![];
        if is_image || is_describe {
            if is_image { if let Err(e) = core.catalog.validate_settings(&model_key, &settings) { block(e); } }
            for e in g.incoming(&id, if is_describe { "image" } else { "refs" }) {
                let src = g.node(&e.source).unwrap();
                if src.kind == "imageAsset" {
                    match src.data.get("assetId").and_then(|v| v.as_str()) {
                        None => block(format!("The reference image “{}” has no file", title_of(src))),
                        Some(aid) => {
                            let found: Option<(String, String)> = core.db(|c| Ok(c.query_row(
                                "SELECT sha256, rel_path FROM assets WHERE id=?1 AND project_id=?2", params![aid, project_id],
                                |r| Ok((r.get(0)?, r.get(1)?))).optional()?))?;
                            match found {
                                Some((sha, rel)) if core.data_dir.join(&rel).exists() => {
                                    ref_components.push(format!("asset:{sha}"));
                                    refs.push(RefSrc::Asset { asset_id: aid.to_string() });
                                }
                                _ => block(format!("The file of reference image “{}” is missing", title_of(src))),
                            }
                        }
                    }
                } else {
                    deps.push(src.id.clone());
                    ref_components.push(format!("node:{}", fps.get(&src.id).cloned().unwrap_or_default()));
                    refs.push(RefSrc::Node { node_id: src.id.clone() });
                }
            }
            // Elements called as @Name in a written prompt: their images join the references and
            // the prompt names each one by its position, followed by its locked description.
            if is_image {
                if let Some(text) = final_prompt.clone() {
                    let used = crate::elements::mentions(core, project_id, &text)?;
                    if !used.is_empty() {
                        let base = refs.len();
                        let mut places = vec![];
                        for (_, e) in &used {
                            let sha = match &e.asset_id { Some(a) => crate::elements::image_sha(core, project_id, a)?, None => None };
                            match (&e.asset_id, sha) {
                                (Some(a), Some(sha)) => {
                                    ref_components.push(format!("element:{sha}"));
                                    refs.push(RefSrc::Asset { asset_id: a.clone() });
                                    places.push(Some(refs.len()));
                                }
                                _ => places.push(None),
                            }
                            ref_components.push(format!("element-text:{}:{}", e.name, e.description));
                        }
                        let _ = base;
                        final_prompt = Some(crate::elements::apply(&text, &used, |k| places[k].map(|n| format!("reference image {n}"))));
                    }
                }
            }
            if let (Some(c), Some(p)) = (&camera, final_prompt.clone()) { final_prompt = Some(format!("{p}\n\n{c}")); }
            if is_describe && refs.is_empty() { block("Connect an image to the “image” port".into()); }
            if is_image && refs.len() > core.catalog.max_refs(&model_key) {
                block(format!("More references than {model_label} accepts"));
            }
            if is_image && settings.get("background").and_then(|v| v.as_str()) == Some("transparent") && core.catalog.transparent_needs_alpha_ref(&model_key) {
                let ok = match refs.as_slice() {
                    [RefSrc::Asset { asset_id }] => core.asset_bytes(asset_id).map(|(_, mime, _, b)| mime == "image/png" && has_alpha(&b)).unwrap_or(false),
                    _ => false,
                };
                if !ok {
                    block(format!("A transparent background in {model_label} needs exactly one PNG reference with an alpha channel"));
                }
            }
        }
        let instructions = (!is_image && !is_describe).then(|| node.data.get("instructions").and_then(|v| v.as_str()).unwrap_or("").to_string());

        let fingerprint = hex(&Sha256::digest(serde_json::to_vec(&json!({
            "kind": node.kind, "provider": provider, "region": region, "model": model_id,
            "text": text_component, "settings": settings, "refs": ref_components, "instructions": instructions,
        }))?));
        fps.insert(id.clone(), fingerprint.clone());

        let mut action = "new".to_string();
        let (mut reuse_asset_id, mut reuse_text) = (None, None);
        if reason.is_some() {
            action = "blocked".into();
        } else if !force_set.contains(&id) && !deps.iter().any(|d| fresh.contains(d)) {
            let hit: Option<(Option<String>, Option<String>)> = core.db(|c| Ok(c.query_row(
                "SELECT asset_id, text_output FROM node_runs WHERE project_id=?1 AND node_id=?2 AND fingerprint=?3 AND status='succeeded' ORDER BY finished_at DESC LIMIT 1",
                params![project_id, id, fingerprint], |r| Ok((r.get(0)?, r.get(1)?))).optional()?))?;
            if let Some((asset, text)) = hit {
                let file_ok = match &asset {
                    Some(a) => core.db(|c| Ok(c.query_row("SELECT rel_path FROM assets WHERE id=?1", [a], |r| r.get::<_, String>(0)).optional()?))?
                        .map(|rel| core.data_dir.join(rel).exists()).unwrap_or(false),
                    None => text.is_some(),
                };
                if file_ok {
                    action = "reuse".into();
                    reuse_asset_id = asset;
                    reuse_text = text;
                }
            }
        }
        if action == "new" {
            fresh.insert(id.clone());
        }
        let model_key_for_size = model_key.clone();
        let late_camera = if final_prompt.is_none() { camera } else { None };
        items.push(PlanItem {
            node_id: id.clone(), title: title_of(node), kind: if is_image { "image".into() } else { "text".into() },
            model_key, model_label, model_id, action, reason, fingerprint, final_prompt, text_from, instructions,
            camera: late_camera,
            pixel_size: if is_image { core.catalog.pixel_size(&model_key_for_size, &settings).map(|(w, h)| format!("{w}×{h}")) } else { None },
            refs, settings, deps, reuse_asset_id, reuse_text,
        });
    }

    let news: Vec<&PlanItem> = items.iter().filter(|i| i.action == "new").collect();
    let mut by_model: Vec<(String, String, usize)> = vec![];
    for i in news.iter().filter(|i| i.kind == "image") {
        match by_model.iter_mut().find(|(l, id, _)| *l == i.model_label && *id == i.model_id) {
            Some(m) => m.2 += 1,
            None => by_model.push((i.model_label.clone(), i.model_id.clone(), 1)),
        }
    }
    let mut seen_refs: HashMap<&str, usize> = HashMap::new();
    for i in &news {
        for r in &i.refs {
            if let RefSrc::Asset { asset_id } = r { *seen_refs.entry(asset_id.as_str()).or_default() += 1; }
        }
    }
    Ok(Plan {
        id: new_id(), project_id: project_id.into(), revision: row.revision, targets: targets.to_vec(), force_new,
        provider, region,
        new_images: news.iter().filter(|i| i.kind == "image").count(),
        new_texts: news.iter().filter(|i| i.kind == "text").count(),
        reused: items.iter().filter(|i| i.action == "reuse").count(),
        blocked: items.iter().filter(|i| i.action == "blocked").count(),
        by_model: by_model.into_iter().map(|(label, id, count)| json!({"label": label, "modelId": id, "count": count})).collect(),
        shared_refs: seen_refs.values().any(|n| *n > 1),
        items,
    })
}

/// Compute and store a plan. Nothing is sent anywhere.
pub fn build_plan(core: &Core, project_id: &str, targets: &[String], force_new: bool) -> Res<Plan> {
    let plan = plan_in(core, project_id, targets, force_new)?;
    core.db(|c| {
        c.execute("INSERT INTO runs(id,project_id,status,plan_json,created_at) VALUES(?1,?2,'planned',?3,?4)",
            params![plan.id, project_id, serde_json::to_string(&plan)?, now_ms()])?;
        Ok(plan)
    })
}

struct Out {
    asset: Option<String>,
    text: Option<String>,
}

fn set_run_status(core: &Core, run_id: &str, status: &str) {
    let _ = core.db(|c| { c.execute("UPDATE runs SET status=?1 WHERE id=?2", params![status, run_id])?; Ok(()) });
}

/// What a Describe node asks when it has no instruction of its own.
pub const DESCRIBE_DEFAULT: &str = "Describe this image as one detailed image-generation prompt: subject, clothing, pose and action, setting, framing and camera angle, lighting, colours and style. Write only the prompt, in English, with no preface.";

fn finish_node(core: &Core, node_run: &str, status: &str, error: Option<&str>) {
    let _ = core.db(|c| {
        c.execute("UPDATE node_runs SET status=?1, error=?2, finished_at=?3 WHERE id=?4", params![status, error, now_ms(), node_run])?;
        Ok(())
    });
}

async fn run_item(core: &Core, set: &ProviderSet, plan: &Plan, item: &PlanItem, node_run: &str, prompt: &str,
                  images: Vec<(String, Vec<u8>)>, parents: &[String]) -> Result<Out, ProviderError> {
    if item.kind == "text" {
        let mut messages = vec![];
        if let Some(i) = item.instructions.as_ref().filter(|s| !s.trim().is_empty()) {
            messages.push(json!({"role": "system", "content": i}));
        }
        if images.is_empty() {
            messages.push(json!({"role": "user", "content": prompt}));
        } else {
            // Describe: the image goes to the chat model with the instruction.
            let mut parts: Vec<Value> = images.iter().map(|(mime, bytes)| json!({"type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes))}})).collect();
            parts.push(json!({"type": "text", "text": prompt}));
            messages.push(json!({"role": "user", "content": parts}));
        }
        let req = ChatRequest { model: item.model_id.clone(), messages, tools: None, thinking: None, cancel: Default::default() };
        let out = set.chat.chat_stream(req, &mut |_| {}).await?;
        if !out.complete || out.text.trim().is_empty() {
            return Err(ProviderError::new(ErrKind::Other, "The rewrite reply was incomplete"));
        }
        let text = out.text.trim().to_string();
        core.db(|c| {
            c.execute("UPDATE node_runs SET status='succeeded', text_output=?1, usage_json=?2, finished_at=?3 WHERE id=?4",
                params![text, out.usage.map(|u| u.to_string()), now_ms(), node_run])?;
            Ok(())
        }).map_err(|e| ProviderError::new(ErrKind::Other, e.message))?;
        return Ok(Out { asset: None, text: Some(text) });
    }
    let req = ImageRequest { model: item.model_id.clone(), prompt: prompt.to_string(), settings: core.catalog.wire_settings(&item.model_key, &item.settings), images };
    let result = set.image.generate(req).await?;
    // Paid call succeeded: from here on, failures are local and must not trigger a resend.
    let local = |e: AppError| ProviderError::new(ErrKind::Other, format!("The result arrived but could not be saved: {}", e.message));
    let file = core.write_asset_file(&plan.project_id, &result.bytes).map_err(local)?;
    core.db(|c| {
        let tx = c.transaction()?;
        tx.execute("INSERT INTO assets(id,project_id,kind,rel_path,mime,width,height,sha256,title,provider,region,model_id,prompt,settings_json,node_id,node_run_id,created_at)
                    VALUES(?1,?2,'generated',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![file.id, plan.project_id, file.rel_path, file.mime, file.width, file.height, file.sha, item.title,
                    set.name, set.region, result.model, prompt, Value::Object(item.settings.clone()).to_string(), item.node_id, node_run, now_ms()])?;
        for (i, p) in parents.iter().enumerate() {
            tx.execute("INSERT INTO asset_inputs(asset_id,input_asset_id,ord) VALUES(?1,?2,?3)", params![file.id, p, i as i64])?;
        }
        tx.execute("UPDATE node_runs SET status='succeeded', asset_id=?1, usage_json=?2, finished_at=?3 WHERE id=?4",
            params![file.id, result.usage.map(|u| u.to_string()), now_ms(), node_run])?;
        tx.commit()?;
        Ok(())
    }).map_err(local)?;
    Ok(Out { asset: Some(file.id), text: None })
}

/// Execute a reviewed plan. A plan can be claimed exactly once, so a double
/// click cannot send a second request. If inputs changed since the plan was
/// shown, nothing runs and the refreshed plan is returned for review.
pub async fn execute(core: &Core, plan_id: &str, thread_id: Option<String>) -> Res<Value> {
    let stored: Plan = core.db(|c| {
        let json: String = c.query_row("SELECT plan_json FROM runs WHERE id=?1", [plan_id], |r| r.get(0)).optional()?
            .ok_or_else(|| AppError::new("not_found", "Plan not found"))?;
        let claimed = c.execute("UPDATE runs SET status='running' WHERE id=?1 AND status='planned'", [plan_id])?;
        if claimed == 0 {
            return Err(AppError::new("run_already_started", "This plan already started or finished; no second request was sent."));
        }
        Ok(serde_json::from_str(&json)?)
    })?;
    let project_id = stored.project_id.clone();
    let fresh = plan_in(core, &project_id, &stored.targets, stored.force_new)?;
    if fresh.signature() != stored.signature() {
        set_run_status(core, plan_id, "stale");
        let fresh = core.db(|c| {
            c.execute("INSERT INTO runs(id,project_id,status,plan_json,created_at) VALUES(?1,?2,'planned',?3,?4)",
                params![fresh.id, project_id, serde_json::to_string(&fresh)?, now_ms()])?;
            Ok(fresh)
        })?;
        return Ok(json!({"stale": true, "plan": fresh}));
    }
    if stored.blocked > 0 {
        set_run_status(core, plan_id, "blocked");
        let first = stored.items.iter().find(|i| i.action == "blocked").unwrap();
        return Err(AppError::new("plan_blocked", format!("«{}»: {}", first.title, first.reason.clone().unwrap_or_default()))
            .with(json!({"nodeId": first.node_id})));
    }
    let set = match core.providers() {
        Ok(s) => s,
        Err(e) => { set_run_status(core, plan_id, "failed"); return Err(e); }
    };
    let cancel = core.cancel_flag(plan_id);
    let mut outputs: HashMap<String, Out> = HashMap::new();
    let mut bad: HashSet<String> = HashSet::new();
    let mut results: Vec<Value> = vec![];
    let mut cancelled = false;

    for item in &stored.items {
        if item.action == "reuse" {
            outputs.insert(item.node_id.clone(), Out { asset: item.reuse_asset_id.clone(), text: item.reuse_text.clone() });
            results.push(json!({"nodeId": item.node_id, "status": "reused", "assetId": item.reuse_asset_id}));
            continue;
        }
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            results.push(json!({"nodeId": item.node_id, "status": "cancelled"}));
            continue;
        }
        let node_run = new_id();
        let insert = |status: &str, prompt: &str, error: Option<&str>| core.db(|c| {
            c.execute("INSERT INTO node_runs(id,run_id,project_id,node_id,kind,status,fingerprint,provider,region,model_id,final_prompt,settings_json,error,started_at,finished_at)
                       VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![node_run, plan_id, project_id, item.node_id, item.kind, status, item.fingerprint, set.name, set.region, item.model_id,
                        prompt, Value::Object(item.settings.clone()).to_string(), error, now_ms(), error.map(|_| now_ms())])?;
            Ok(())
        });
        if item.deps.iter().any(|d| bad.contains(d)) {
            bad.insert(item.node_id.clone());
            let msg = "Not run because one of its inputs did not succeed";
            insert("skipped", "", Some(msg))?;
            results.push(json!({"nodeId": item.node_id, "status": "skipped", "error": msg}));
            continue;
        }
        // Gather the exact inputs: prompt text and the bytes of the chosen versions.
        let gathered: Res<(String, Vec<(String, Vec<u8>)>, Vec<String>)> = (|| {
            let prompt = match (&item.final_prompt, &item.text_from) {
                (Some(p), _) => p.clone(),
                (None, Some(n)) => {
                    let t = outputs.get(n).and_then(|o| o.text.clone()).ok_or_else(|| AppError::new("missing_input", "The rewritten text is not available"))?;
                    match &item.camera { Some(c) => format!("{t}\n\n{c}"), None => t }
                }
                _ => return Err(AppError::new("missing_input", "Missing prompt")),
            };
            let mut images = vec![];
            let mut parents = vec![];
            for r in &item.refs {
                let aid = match r {
                    RefSrc::Asset { asset_id } => asset_id.clone(),
                    RefSrc::Node { node_id } => outputs.get(node_id).and_then(|o| o.asset.clone()).ok_or_else(|| AppError::new("missing_input", "The input image is not available"))?,
                };
                let (owner, mime, _, bytes) = core.asset_bytes(&aid)?;
                if owner != project_id {
                    return Err(AppError::new("forbidden", "Image from another project"));
                }
                images.push((mime, bytes));
                parents.push(aid);
            }
            Ok((prompt, images, parents))
        })();
        let (prompt, images, parents) = match gathered {
            Ok(x) => x,
            Err(e) => {
                bad.insert(item.node_id.clone());
                insert("failed", "", Some(&e.message))?;
                results.push(json!({"nodeId": item.node_id, "status": "failed", "error": e.message}));
                continue;
            }
        };
        // Record the attempt before anything is sent.
        insert("sending", &prompt, None)?;
        core.emit("run-update", json!({"projectId": project_id, "runId": plan_id, "nodeId": item.node_id, "status": "sending"}));
        match run_item(core, &set, &stored, item, &node_run, &prompt, images, &parents).await {
            Ok(out) => {
                core.mark_tried(&set, &item.model_id);
                results.push(json!({"nodeId": item.node_id, "status": "succeeded", "assetId": out.asset}));
                outputs.insert(item.node_id.clone(), out);
            }
            Err(e) => {
                bad.insert(item.node_id.clone());
                let status = if e.kind == ErrKind::Uncertain { "uncertain" } else { "failed" };
                if e.kind == ErrKind::Activation {
                    core.mark_needs_activation(&set, &item.model_id);
                }
                let msg = e.display(&set.region, &item.model_id);
                finish_node(core, &node_run, status, Some(&msg));
                results.push(json!({"nodeId": item.node_id, "status": status, "error": msg}));
            }
        }
        core.emit("run-update", json!({"projectId": project_id, "runId": plan_id, "nodeId": item.node_id, "status": "done"}));
    }
    core.cancels.lock().unwrap().remove(plan_id);
    let status = if cancelled { "cancelled" } else if bad.is_empty() { "completed" } else { "partial" };
    set_run_status(core, plan_id, status);

    if let Some(thread) = thread_id {
        let assets: Vec<&Value> = results.iter().filter(|r| r["status"] == "succeeded" || r["status"] == "reused")
            .filter(|r| stored.targets.iter().any(|t| r["nodeId"] == t.as_str()) || stored.targets.is_empty())
            .filter_map(|r| r.get("assetId")).filter(|a| a.is_string()).collect();
        let errors: Vec<&Value> = results.iter().filter_map(|r| r.get("error")).filter(|e| e.is_string()).collect();
        let content = if errors.is_empty() { String::new() } else { errors.iter().filter_map(|e| e.as_str()).collect::<Vec<_>>().join("\n") };
        let model = stored.items.iter().rev().find(|i| i.kind == "image").map(|i| i.model_id.clone());
        core.db(|c| {
            c.execute("INSERT INTO messages(id,thread_id,role,content,status,model_id,provider,meta_json,created_at) VALUES(?1,?2,'assistant',?3,?4,?5,?6,?7,?8)",
                params![new_id(), thread, content, if errors.is_empty() { "complete" } else { "error" }, model, set.name,
                        json!({"kind": "run_result", "runId": plan_id, "assetIds": assets, "nodeIds": stored.targets}).to_string(), now_ms()])?;
            Ok(())
        })?;
        core.emit("messages-changed", json!({"threadId": thread}));
    }
    core.emit("run-update", json!({"projectId": project_id, "runId": plan_id, "status": status}));
    Ok(json!({"runId": plan_id, "status": status, "results": results}))
}
