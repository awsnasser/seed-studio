//! Command surface. The desktop shell exposes exactly one IPC command (`rpc`)
//! that lands here; nothing in this file reads arbitrary paths from the UI.
use crate::agent;
use crate::core::{new_id, now_ms, Actor, Core, PROFILE};
use crate::engine;
use crate::error::{AppError, Res};
use crate::graph::{self, Ctx, Graph, Op};
use crate::provider::{ChatRequest, ErrKind};
use crate::secrets;
use crate::video;
use base64::Engine as _;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn s(args: &Value, key: &str) -> Res<String> {
    args[key].as_str().map(String::from).ok_or_else(|| AppError::new("bad_args", format!("Missing argument: {key}")))
}
fn so(args: &Value, key: &str) -> Option<String> {
    args[key].as_str().map(String::from)
}
fn strs(args: &Value, key: &str) -> Vec<String> {
    args[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}
fn b64(data: &str) -> Res<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|_| AppError::new("bad_args", "Invalid file data"))
}

fn thread_project(c: &Connection, thread_id: &str) -> Res<String> {
    c.query_row("SELECT project_id FROM threads WHERE id=?1", [thread_id], |r| r.get(0)).optional()?
        .ok_or_else(|| AppError::new("not_found", "Conversation not found"))
}

fn message_rows(c: &Connection, thread_id: &str) -> Res<Vec<Value>> {
    let mut st = c.prepare("SELECT id, role, content, status, model_id, provider, pinned, meta_json, created_at FROM messages WHERE thread_id=?1 ORDER BY created_at ASC, rowid ASC")?;
    let rows = st.query_map([thread_id], |r| {
        let meta: String = r.get(7)?;
        Ok(json!({
            "id": r.get::<_, String>(0)?, "role": r.get::<_, String>(1)?, "content": r.get::<_, String>(2)?,
            "status": r.get::<_, String>(3)?, "modelId": r.get::<_, Option<String>>(4)?, "provider": r.get::<_, Option<String>>(5)?,
            "pinned": r.get::<_, i64>(6)? != 0, "meta": serde_json::from_str::<Value>(&meta).unwrap_or(json!({})),
            "createdAt": r.get::<_, i64>(8)?,
        }))
    })?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn asset_rows(c: &Connection, project_id: &str) -> Res<Vec<Value>> {
    let mut st = c.prepare("SELECT id, kind, mime, width, height, title, provider, region, model_id, prompt, settings_json, node_id, created_at,
        (SELECT group_concat(input_asset_id) FROM (SELECT input_asset_id FROM asset_inputs WHERE asset_id=assets.id ORDER BY ord))
        FROM assets WHERE project_id=?1 ORDER BY created_at DESC, rowid DESC")?;
    let rows = st.query_map([project_id], |r| {
        let settings: Option<String> = r.get(10)?;
        let parents: Option<String> = r.get(13)?;
        Ok(json!({
            "id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "mime": r.get::<_, String>(2)?,
            "width": r.get::<_, i64>(3)?, "height": r.get::<_, i64>(4)?, "title": r.get::<_, String>(5)?,
            "provider": r.get::<_, Option<String>>(6)?, "region": r.get::<_, Option<String>>(7)?, "modelId": r.get::<_, Option<String>>(8)?,
            "prompt": r.get::<_, Option<String>>(9)?,
            "settings": settings.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
            "nodeId": r.get::<_, Option<String>>(11)?, "createdAt": r.get::<_, i64>(12)?,
            "parentAssetIds": parents.map(|p| p.split(',').map(String::from).collect::<Vec<_>>()).unwrap_or_default(),
        }))
    })?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn app_state(core: &Core) -> Res<Value> {
    let region = core.region();
    let statuses: HashMap<String, String> = core.db(|c| {
        let mut st = c.prepare("SELECT model_id, status FROM model_status WHERE region=?1")?;
        let rows = st.query_map([&region], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    })?;
    let models: Vec<Value> = core.catalog.models.iter().map(|m| {
        let resolved = core.resolve_model_id(&m.key).unwrap_or_default();
        json!({
            "key": m.key, "label": m.label, "kind": m.kind, "candidateIds": m.candidate_ids,
            "resolvedId": resolved,
            "endpoint": core.setting(&format!("endpoint:{}", m.key)).unwrap_or_default(),
            "status": statuses.get(&resolved).cloned().unwrap_or_else(|| "unchecked".into()),
            "options": core.catalog.options(&m.key),
        })
    }).collect();
    let keys: Map<String, Value> = core.catalog.regions.keys().map(|r| (r.clone(), json!(core.has_key(r)))).collect();
    Ok(json!({
        "theme": core.setting("theme").unwrap_or_else(|| "system".into()),
        "providerMode": core.provider_mode(),
        "region": region,
        "regions": core.catalog.regions,
        "gateway": core.gateway(),
        "prices": {
            "videoPerM": core.setting("price_video_per_m").and_then(|p| p.parse::<f64>().ok()),
            "imageEach": core.setting("price_image_each").and_then(|p| p.parse::<f64>().ok()),
        },
        "libraryUpload": crate::library::uploads_on(core),
        "gateways": core.catalog.raw["gateways"],
        "hasKey": keys,
        "chatModel": core.chat_model_key(),
        "imageModel": core.image_model_key(),
        "thinking": core.setting("thinking").unwrap_or_else(|| "default".into()),
        "models": models,
        "catalogCheckedOn": core.catalog.raw["checkedOn"],
        "lastProjectId": core.setting("last_project"),
    }))
}

fn set_setting(core: &Core, key: &str, value: &str) -> Res<()> {
    let ok = match key {
        "theme" => ["light", "dark", "system"].contains(&value),
        "provider_mode" => ["modelark", "mock"].contains(&value),
        "region" => core.catalog.regions.contains_key(value),
        "chat_model" => core.catalog.model(value).map(|m| m.kind == "chat").unwrap_or(false),
        "image_model" => core.catalog.model(value).map(|m| m.kind == "image").unwrap_or(false),
        "thinking" => ["default", "enabled", "disabled"].contains(&value),
        "last_project" => value.len() <= 64,
        // The owner's own prices, used only to show an estimate next to the token counts.
        "price_video_per_m" | "price_image_each" => value.is_empty() || value.parse::<f64>().map(|p| (0.0..=100_000.0).contains(&p)).unwrap_or(false),
        // The owner's declaration that reference images are AI-generated, per gateway connection.
        k if k.starts_with("library_upload:") => core.catalog.raw["gateways"][&k[15..]]["assetsUrl"].is_string() && ["0", "1"].contains(&value),
        k if k.starts_with("endpoint:") => core.catalog.model(&k[9..]).is_some() && value.len() <= 128
            && value.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c)),
        _ => false,
    };
    if !ok {
        return Err(AppError::new("bad_args", format!("Invalid setting: {key}")));
    }
    core.set_setting(key, value)
}

fn replace_graph(core: &Core, project_id: &str, g: &Graph, viewport: Option<&Value>, label: &str) -> Res<()> {
    core.db(|c| {
        let tx = c.transaction()?;
        let row = Core::load_canvas(&tx, project_id)?;
        tx.execute("INSERT INTO canvas_history(project_id,stack,graph_json,label,actor,group_id) VALUES(?1,'undo',?2,?3,'user',NULL)",
            params![project_id, serde_json::to_string(&row.graph)?, label])?;
        tx.execute("DELETE FROM canvas_history WHERE project_id=?1 AND stack='redo'", [project_id])?;
        tx.execute("UPDATE canvases SET graph_json=?1, revision=?2, viewport_json=?3, updated_at=?4 WHERE project_id=?5",
            params![serde_json::to_string(g)?, row.revision + 1, viewport.unwrap_or(&row.viewport).to_string(), now_ms(), project_id])?;
        tx.commit()?;
        Ok(())
    })
}

/// Import a canvas document. Asset paths are keys into `files`, and must stay
/// inside the document's own folder.
pub fn import_canvas(core: &Core, project_id: &str, doc: &Value, files: &HashMap<String, Vec<u8>>) -> Res<()> {
    if doc["schemaVersion"].as_i64() != Some(1) {
        return Err(AppError::new("bad_import", "Unsupported canvas file version"));
    }
    let mut id_map: HashMap<String, String> = HashMap::new();
    for a in doc["assets"].as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
        let rel = a["relativePath"].as_str().unwrap_or("");
        let escapes = rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains(':')
            || rel.split('/').any(|p| p == ".." || p.is_empty());
        if escapes {
            return Err(AppError::new("bad_import", format!("The file path “{rel}” escapes the imported canvas folder")));
        }
        let bytes = files.get(rel).ok_or_else(|| AppError::new("bad_import", format!("The attached file “{rel}” is missing")))?;
        let file = core.write_asset_file(project_id, bytes)?;
        core.insert_reference_asset(project_id, &file, a["description"].as_str().unwrap_or("Imported reference"))?;
        id_map.insert(a["id"].as_str().unwrap_or("").to_string(), file.id);
    }
    let mut g: Graph = serde_json::from_value(json!({"nodes": doc["nodes"], "edges": doc["edges"]}))
        .map_err(|e| AppError::new("bad_import", format!("Invalid canvas file: {e}")))?;
    for n in g.nodes.iter_mut().filter(|n| n.kind == "imageAsset") {
        if let Some(old) = n.data.get("assetId").and_then(|v| v.as_str()).map(String::from) {
            let new = id_map.get(&old).ok_or_else(|| AppError::new("bad_import", format!("Node {} points to an image that is not attached", n.id)))?;
            n.data.insert("assetId".into(), json!(new));
        }
    }
    let assets = core.db(|c| Core::asset_ids(c, project_id))?;
    let g = graph::validated(&g, &Ctx { catalog: &core.catalog, asset_ids: &assets })
        .map_err(|e| AppError::new("bad_import", format!("Canvas file rejected: {}", e.message)))?;
    replace_graph(core, project_id, &g, doc.get("viewport").filter(|v| v.is_object()), "Import canvas")
}

/// Text of attached documents, as it is appended to the message sent to the model.
fn doc_blocks(c: &Connection, project_id: &str, ids: &[String]) -> Res<String> {
    let mut out = String::new();
    for id in ids {
        let row: Option<(String, String)> = c.query_row("SELECT name, text FROM documents WHERE id=?1 AND project_id=?2", params![id, project_id],
            |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some((name, text)) = row {
            let cut: String = text.chars().take(40_000).collect();
            out.push_str(&format!("\n\n<file name=\"{}\">\n{}\n</file>", name.replace('"', "'"), cut));
        }
    }
    Ok(out)
}

/// Accept a dropped file: images become project images; PDFs and text files
/// have their text extracted locally. Nothing is uploaded here.
fn file_import(core: &Core, args: &Value) -> Res<Value> {
    let project_id = s(args, "projectId")?;
    let name: String = so(args, "name").unwrap_or_else(|| "file".into()).chars().take(120).collect();
    let bytes = b64(&s(args, "base64")?)?;
    core.db(|c| Core::load_canvas(c, &project_id).map(|_| ()))?;
    if bytes.len() > 25 * 1024 * 1024 {
        return Err(AppError::new("file_too_large", format!("“{name}” is larger than 25 MB")));
    }
    if image::guess_format(&bytes).is_ok() {
        let file = core.write_asset_file(&project_id, &bytes)?;
        let title = name.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or(name.clone());
        core.insert_reference_asset(&project_id, &file, &title)?;
        return Ok(json!({"kind": "image", "id": file.id, "name": name, "width": file.width, "height": file.height}));
    }
    let lower = name.to_lowercase();
    let (kind, text) = if bytes.starts_with(b"%PDF") || lower.ends_with(".pdf") {
        let data = bytes.clone();
        let text = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&data)).ok().and_then(|r| r.ok())
            .ok_or_else(|| AppError::new("unsupported_file", format!("Could not read text from “{name}”. Scanned or protected PDFs are not supported.")))?;
        ("pdf", text)
    } else {
        match String::from_utf8(bytes) {
            Ok(t) if !t.contains('\0') => ("text", t),
            _ => return Err(AppError::new("unsupported_file", format!("“{name}” is not a supported file. Drop images (PNG, JPEG, WebP), PDFs, or text files (txt, md, csv, json, code)."))),
        }
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::new("unsupported_file", format!("“{name}” contains no readable text.")));
    }
    let total = text.chars().count();
    let kept: String = text.chars().take(200_000).collect();
    let id = new_id();
    core.db(|c| {
        c.execute("INSERT INTO documents(id,project_id,name,kind,text,chars,truncated,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, project_id, name, kind, kept, total as i64, (total > 200_000) as i64, now_ms()])?;
        Ok(())
    })?;
    Ok(json!({"kind": "document", "id": id, "name": name, "chars": total, "truncated": total > 40_000}))
}

/// One tiny real chat request, started by the user from Settings, to learn
/// whether this account can actually call a chat model.
async fn model_test(core: &Core, args: &Value) -> Res<Value> {
    let key = s(args, "modelKey")?;
    if core.catalog.model(&key).map(|m| m.kind != "chat").unwrap_or(true) {
        return Err(AppError::new("bad_args", "Only chat models can be tested without generating an image"));
    }
    let set = core.providers()?;
    let model_id = core.resolve_model_id(&key)?;
    let req = ChatRequest { model: model_id.clone(), messages: vec![json!({"role": "user", "content": "Reply with the single word: OK"})], tools: None, thinking: None, cancel: Default::default() };
    match set.chat.chat_stream(req, &mut |_| {}).await {
        Ok(out) if out.complete => {
            core.mark_tried(&set, &model_id);
            Ok(json!({"ok": true, "model": out.model, "state": app_state(core)?}))
        }
        Ok(_) => Ok(json!({"ok": false, "message": "The reply was cut off before it finished.", "state": app_state(core)?})),
        Err(e) => {
            if e.kind == ErrKind::Activation { core.mark_needs_activation(&set, &model_id); }
            Ok(json!({"ok": false, "kind": format!("{:?}", e.kind), "message": e.display(&set.region, &model_id), "state": app_state(core)?}))
        }
    }
}

/// One chat request that turns a story into numbered storyboard prompts. It only writes
/// text: frames are generated later, from the run plan the user approves.
async fn storyboard_write(core: &Core, args: &Value) -> Res<Value> {
    let story = s(args, "story")?;
    if story.trim().is_empty() {
        return Err(AppError::new("bad_args", "Paste the story first"));
    }
    let frames = args["frames"].as_u64().unwrap_or(0).min(100);
    let set = core.providers()?;
    if set.name == "mock" {
        // Simulation: one frame per sentence, so the flow can be tried offline.
        let mut parts: Vec<String> = story.split(|c| ".!?؟\n".contains(c)).map(|p| p.trim().to_string()).filter(|p| p.chars().count() > 3).collect();
        if frames > 0 { parts.truncate(frames as usize); }
        let text = parts.iter().enumerate().map(|(i, p)| format!("{}. {p}", i + 1)).collect::<Vec<_>>().join("\n");
        return Ok(json!({"text": text, "complete": true, "provider": "mock", "model": "simulation"}));
    }
    let key = core.chat_model_key();
    let model_id = core.resolve_model_id(&key)?;
    let count = if frames > 0 { format!("exactly {frames}") } else { "as many as the story needs (one per distinct shot)".to_string() };
    let system = format!("You turn a story or script into image-generation prompts for a storyboard. Write {count} prompts, in story order. \
Output only the prompts, one per line, numbered like `1. `, each on a single line, with no titles, notes or blank commentary. \
Every frame is generated independently and the image model remembers nothing between frames, so each prompt must stand alone: \
repeat the same concrete description of every recurring character, product and location word for word in each prompt where it appears, \
and state the action, framing (shot size and angle), setting and lighting. Do not include a visual style: the user adds one shared style to all frames. \
Write the prompts in the language of the story. Keep each prompt under 70 words.");
    let req = ChatRequest {
        model: model_id.clone(),
        messages: vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": story})],
        tools: None, thinking: Some("disabled".into()), cancel: Default::default(),
    };
    let mut text = String::new();
    match set.chat.chat_stream(req, &mut |d| text.push_str(d)).await {
        Ok(out) => {
            core.mark_tried(&set, &model_id);
            Ok(json!({"text": if out.text.is_empty() { text } else { out.text }, "complete": out.complete, "provider": set.name, "model": out.model}))
        }
        Err(e) => {
            if e.kind == ErrKind::Activation { core.mark_needs_activation(&set, &model_id); }
            Err(e.to_app(&set.region, &model_id))
        }
    }
}

async fn chat_send(core: &Core, args: &Value) -> Res<Value> {
    let thread_id = s(args, "threadId")?;
    let text = s(args, "text")?;
    let attachments = strs(args, "attachmentIds");
    if text.trim().is_empty() {
        return Err(AppError::new("bad_args", "The message is empty"));
    }
    let (project_id, instructions, pinned, mut history) = core.db(|c| {
        let project_id = thread_project(c, &thread_id)?;
        let instructions: String = c.query_row("SELECT instructions FROM projects WHERE id=?1", [&project_id], |r| r.get(0))?;
        let mut st = c.prepare("SELECT m.content FROM messages m JOIN threads t ON t.id=m.thread_id WHERE t.project_id=?1 AND m.pinned=1 AND m.content!='' ORDER BY m.created_at")?;
        let pinned: Vec<String> = st.query_map([&project_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut st = c.prepare("SELECT role, content, meta_json FROM messages WHERE thread_id=?1 AND content!='' AND role IN ('user','assistant') AND status!='error' ORDER BY created_at DESC, rowid DESC")?;
        let mut history: Vec<(String, String)> = vec![];
        let mut budget = 120_000usize; // characters of prior conversation (incl. attached documents) sent with each request
        for row in st.query_map([&thread_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
            let (role, mut content, meta) = row?;
            let meta: Value = serde_json::from_str(&meta).unwrap_or(Value::Null);
            let ids: Vec<String> = meta["documents"].as_array().map(|a| a.iter().filter_map(|d| d["id"].as_str().map(String::from)).collect()).unwrap_or_default();
            content.push_str(&doc_blocks(c, &project_id, &ids)?);
            let len = content.chars().count();
            if len > budget { break; }
            budget -= len;
            history.push((role, content));
        }
        Ok((project_id, instructions, pinned, history))
    })?;
    history.reverse();

    let save = |role: &str, content: &str, status: &str, model: Option<&str>, provider: Option<&str>, meta: Value| -> Res<String> {
        let id = new_id();
        core.db(|c| {
            c.execute("INSERT INTO messages(id,thread_id,role,content,status,model_id,provider,meta_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, thread_id, role, content, status, model, provider, meta.to_string(), now_ms()])?;
            c.execute("UPDATE threads SET updated_at=?1, title=CASE WHEN title='New chat' AND ?3='user' THEN substr(?4,1,40) ELSE title END WHERE id=?2",
                params![now_ms(), thread_id, role, content])?;
            Ok(())
        })?;
        Ok(id)
    };
    let document_ids = strs(args, "documentIds");
    let (doc_text, doc_meta) = core.db(|c| {
        let mut meta = vec![];
        for id in &document_ids {
            let name: Option<String> = c.query_row("SELECT name FROM documents WHERE id=?1 AND project_id=?2", params![id, project_id], |r| r.get(0)).optional()?;
            let name = name.ok_or_else(|| AppError::new("not_found", "An attached file is no longer available"))?;
            meta.push(json!({"id": id, "name": name}));
        }
        Ok((doc_blocks(c, &project_id, &document_ids)?, meta))
    })?;
    let text_with_docs = format!("{text}{doc_text}");
    save("user", &text, "complete", None, None, json!({"attachments": attachments, "documents": doc_meta}))?;
    core.emit("messages-changed", json!({"threadId": thread_id}));

    // Only images the user picked for this message are attached.
    let prepared: Res<_> = (|| {
        let set = core.providers()?;
        let model_id = core.resolve_model_id(&core.chat_model_key())?;
        let mut parts = vec![json!({"type": "text", "text": text_with_docs})];
        for id in &attachments {
            let (owner, mime, _, bytes) = core.asset_bytes(id)?;
            if owner != project_id {
                return Err(AppError::new("forbidden", "Image from another project"));
            }
            parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))}}));
        }
        Ok((set, model_id, parts))
    })();
    let (set, model_id, parts) = match prepared {
        Ok(x) => x,
        Err(e) => {
            save("assistant", &e.message, "error", None, None, json!({"errorCode": e.code}))?;
            core.emit("messages-changed", json!({"threadId": thread_id}));
            return Err(e);
        }
    };
    let mut system = "You are Seed, a creative assistant inside the Seed Studio app. Reply clearly and concisely in the language the user writes in.".to_string();
    if !instructions.trim().is_empty() {
        system.push_str(&format!("\n\nProject instructions:\n{instructions}"));
    }
    if !pinned.is_empty() {
        system.push_str(&format!("\n\nPinned in this project:\n- {}", pinned.join("\n- ")));
    }
    let mut messages = vec![json!({"role": "system", "content": system})];
    for (role, content) in history {
        messages.push(json!({"role": role, "content": content}));
    }
    messages.push(if attachments.is_empty() { json!({"role": "user", "content": text_with_docs}) } else { json!({"role": "user", "content": parts}) });

    let message_id = save("assistant", "", "streaming", Some(&model_id), Some(&set.name), json!({}))?;
    core.emit("messages-changed", json!({"threadId": thread_id}));
    let thinking = core.setting("thinking").filter(|t| t == "enabled" || t == "disabled");
    let req = ChatRequest { model: model_id.clone(), messages, tools: None, thinking, cancel: core.cancel_flag(&thread_id) };
    let emitter = core.emitter.clone();
    let (tid, mid) = (thread_id.clone(), message_id.clone());
    let mut on_delta = move |d: &str| emitter.emit("chat-delta", json!({"threadId": tid, "messageId": mid, "delta": d}));
    let result = set.chat.chat_stream(req, &mut on_delta).await;
    core.cancels.lock().unwrap().remove(&thread_id);

    let (content, status, model, err) = match result {
        Ok(out) => {
            if out.complete { core.mark_tried(&set, &model_id); }
            (out.text, if out.complete { "complete" } else { "incomplete" }, out.model, None)
        }
        Err(e) => {
            if e.kind == ErrKind::Activation { core.mark_needs_activation(&set, &model_id); }
            (e.display(&set.region, &model_id), "error", model_id.clone(), Some(e.to_app(&set.region, &model_id)))
        }
    };
    core.db(|c| {
        let meta = json!({"errorCode": err.as_ref().map(|e| e.code.clone())});
        c.execute("UPDATE messages SET content=?1, status=?2, model_id=?3, meta_json=?5 WHERE id=?4", params![content, status, model, message_id, meta.to_string()])?;
        Ok(())
    })?;
    core.emit("messages-changed", json!({"threadId": thread_id}));
    match err {
        Some(e) => Err(e),
        None => Ok(json!({"messageId": message_id, "status": status, "model": model, "provider": set.name})),
    }
}

/// "Generate from chat": builds a small visible flow on the project canvas and
/// returns the plan. The same engine runs it after the user confirms.
fn chat_image_flow(core: &Core, args: &Value) -> Res<Value> {
    let project_id = s(args, "projectId")?;
    let thread_id = s(args, "threadId")?;
    let prompt = s(args, "prompt")?;
    let reference = so(args, "refAssetId");
    if prompt.trim().is_empty() {
        return Err(AppError::new("bad_args", "Describe the image or the edit"));
    }
    let row = core.db(|c| {
        if thread_project(c, &thread_id)? != project_id {
            return Err(AppError::new("forbidden", "The conversation does not belong to this project"));
        }
        Core::load_canvas(c, &project_id)
    })?;
    let y = row.graph.nodes.iter().map(|n| n.position.y).fold(f64::NEG_INFINITY, f64::max);
    let y = if y.is_finite() { y + 300.0 } else { 0.0 };
    let tag = &new_id()[..6];
    let (p, r, g, v) = (format!("chat-{tag}-p"), format!("chat-{tag}-r"), format!("chat-{tag}-g"), format!("chat-{tag}-v"));
    let short: String = prompt.chars().take(28).collect();
    let mut ops = vec![
        json!({"op": "addNode", "id": p, "type": "prompt", "position": {"x": 0, "y": y}, "data": {"title": "Prompt from chat", "text": prompt}}),
        json!({"op": "addNode", "id": g, "type": "seedream", "position": {"x": 380, "y": y}, "data": {"title": short, "modelKey": core.image_model_key(),
            "settings": if args["settings"].is_object() { args["settings"].clone() } else { json!({}) }}}),
        json!({"op": "addNode", "id": v, "type": "preview", "position": {"x": 760, "y": y}, "data": {"title": "Chat result"}}),
        json!({"op": "connect", "source": p, "target": g, "targetHandle": "prompt"}),
        json!({"op": "connect", "source": g, "target": v, "targetHandle": "image"}),
    ];
    if let Some(aid) = &reference {
        ops.push(json!({"op": "addNode", "id": r, "type": "imageAsset", "position": {"x": 0, "y": y + 170.0}, "data": {"title": "Chosen image", "assetId": aid}}));
        ops.push(json!({"op": "connect", "source": r, "target": g, "targetHandle": "refs"}));
    }
    let ops: Vec<Op> = serde_json::from_value(json!(ops))?;
    core.apply_patch(&project_id, Some(row.revision), &ops, &Actor::User, "Flow from chat")?;
    core.db(|c| {
        c.execute("INSERT INTO messages(id,thread_id,role,content,status,meta_json,created_at) VALUES(?1,?2,'user',?3,'complete',?4,?5)",
            params![new_id(), thread_id, prompt, json!({"kind": "image_request", "refAssetId": reference, "nodeId": g}).to_string(), now_ms()])?;
        c.execute("UPDATE threads SET updated_at=?1, title=CASE WHEN title='New chat' THEN substr(?3,1,40) ELSE title END WHERE id=?2", params![now_ms(), thread_id, prompt])?;
        Ok(())
    })?;
    core.emit("canvas-changed", json!({"projectId": project_id}));
    let plan = engine::build_plan(core, &project_id, &[g.clone()], false)?;
    Ok(json!({"plan": plan, "nodeId": g}))
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() { return None; }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// What past generations took and used, so a running one can show an expected time and a
/// new one an expected size. Read from this app's own records; nothing is requested.
fn gen_stats(core: &Core, project_id: &str) -> Res<Value> {
    let provider = if core.provider_mode() == "mock" { "mock" } else { "modelark" };
    let tokens = |u: Option<String>| u.and_then(|u| serde_json::from_str::<Value>(&u).ok()).and_then(|u| u["total_tokens"].as_f64());
    core.db(|c| {
        // Videos: time and tokens per second of footage, by pass and resolution (a final takes its length from its draft).
        let mut st = c.prepare("SELECT v.kind, json_extract(v.params_json,'$.resolution'),
                coalesce(json_extract(v.params_json,'$.duration'), json_extract(d.params_json,'$.duration')), v.usage_json, (v.updated_at - v.created_at) / 1000.0
            FROM videos v LEFT JOIN videos d ON d.id = v.draft_of WHERE v.status='succeeded' AND v.provider=?1")?;
        let mut groups: std::collections::BTreeMap<String, (Vec<f64>, Vec<f64>)> = Default::default();
        let mut rows = st.query([provider])?;
        while let Some(r) = rows.next()? {
            let key = format!("{}:{}", r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default());
            let g = groups.entry(key).or_default();
            let secs: f64 = r.get(4)?;
            if secs > 0.0 { g.0.push(secs); }
            if let (Some(t), Some(d)) = (tokens(r.get(3)?), r.get::<_, Option<f64>>(2)?.filter(|d| *d > 0.0)) { g.1.push(t / d); }
        }
        let video: Map<String, Value> = groups.into_iter().map(|(k, (secs, per))| {
            (k, json!({"n": secs.len(), "secs": median(secs), "tokensPerSec": median(per)}))
        }).collect();
        // Images: the time depends mostly on the model and on whether a reference image is sent, so each pair has its own figure.
        let mut st = c.prepare("SELECT (finished_at - started_at) / 1000.0, usage_json, model_id FROM node_runs WHERE kind='image' AND status='succeeded' AND provider=?1 AND usage_json IS NOT NULL AND finished_at > started_at")?;
        let (mut secs, mut toks) = (vec![], vec![]);
        let mut image_groups: std::collections::BTreeMap<String, Vec<f64>> = Default::default();
        let mut rows = st.query([provider])?;
        while let Some(r) = rows.next()? {
            let s: f64 = r.get(0)?;
            let usage: Option<String> = r.get(1)?;
            let with_refs = usage.as_deref().and_then(|u| serde_json::from_str::<Value>(u).ok()).and_then(|u| u["input_images"].as_i64()).unwrap_or(0) > 0;
            image_groups.entry(format!("{}:{}", r.get::<_, String>(2)?, if with_refs { "ref" } else { "text" })).or_default().push(s);
            secs.push(s);
            if let Some(t) = tokens(usage) { toks.push(t); }
        }
        let image_by: Map<String, Value> = image_groups.into_iter().map(|(k, v)| {
            let slowest = v.iter().cloned().fold(0.0, f64::max);
            (k, json!({"n": v.len(), "secs": median(v), "slowest": slowest}))
        }).collect();
        let slowest_image = secs.iter().cloned().fold(0.0, f64::max);
        // What this project has used so far.
        let sum = |sql: &str| -> Res<(f64, i64)> {
            let mut st = c.prepare(sql)?;
            let (mut total, mut n) = (0.0, 0);
            let mut rows = st.query([project_id])?;
            while let Some(r) = rows.next()? {
                n += 1;
                total += tokens(r.get(0)?).unwrap_or(0.0);
            }
            Ok((total, n))
        };
        let (vt, vn) = sum("SELECT usage_json FROM videos WHERE project_id=?1 AND status='succeeded' AND provider!='mock'")?;
        let (it, inn) = sum("SELECT usage_json FROM node_runs WHERE project_id=?1 AND kind='image' AND status='succeeded' AND provider!='mock' AND usage_json IS NOT NULL")?;
        Ok(json!({
            "video": video,
            "image": {"n": secs.len(), "secs": median(secs), "slowest": slowest_image, "tokens": median(toks), "by": image_by},
            "project": {"videoTokens": vt, "videos": vn, "imageTokens": it, "images": inn},
        }))
    })
}

async fn check_connection(core: &Core) -> Res<Value> {
    if core.provider_mode() == "mock" {
        return Err(AppError::new("mock_mode", "Simulation mode does not contact BytePlus. Choose ModelArk, then check the connection."));
    }
    let set = core.providers()?;
    if core.gateway()["listsModels"].as_bool() == Some(false) {
        // This gateway has no model list; its balance endpoint proves the key instead.
        let balance = set.chat.balance().await.map_err(|e| e.to_app(&set.region, "GET balance"))?;
        // The character library is read on the same occasion; the check does not depend on it.
        let library = crate::library::refresh(core).await.ok().map(|l| json!(l["assets"].as_array().map(|a| a.len()).unwrap_or(0)));
        return Ok(json!({"region": set.region, "balance": balance, "library": library, "state": app_state(core)?}));
    }
    let ids = set.chat.list_models().await.map_err(|e| e.to_app(&set.region, "GET /models"))?;
    core.db(|c| {
        for m in &core.catalog.models {
            for id in &m.candidate_ids {
                let status = if ids.contains(id) { "visible" } else { "not_visible" };
                c.execute("INSERT INTO model_status(region,model_id,status,checked_at) VALUES(?1,?2,?3,?4)
                           ON CONFLICT(region,model_id) DO UPDATE SET status=excluded.status, checked_at=excluded.checked_at
                           WHERE model_status.status!='tried' OR excluded.status='not_visible'",
                    params![set.region, id, status, now_ms()])?;
            }
        }
        Ok(())
    })?;
    Ok(json!({"region": set.region, "visibleCount": ids.len(), "state": app_state(core)?}))
}

fn export_asset(core: &Core, id: &str) -> Res<Value> {
    let (_, mime, _, bytes) = core.asset_bytes(id)?;
    let title: String = core.db(|c| Ok(c.query_row("SELECT title FROM assets WHERE id=?1", [id], |r| r.get(0))?))?;
    let ext = match mime.as_str() { "image/png" => "png", "image/jpeg" => "jpg", _ => "webp" };
    let dir = std::env::var("SEED_STUDIO_EXPORT_DIR").map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads")))
        .map_err(|_| AppError::new("io", "Could not find the Downloads folder"))?;
    std::fs::create_dir_all(&dir)?;
    let stem: String = title.chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').take(40).collect();
    let stem = if stem.trim().is_empty() { "seed-studio".to_string() } else { stem.trim().to_string() };
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} {n}.{ext}"));
        n += 1;
    }
    std::fs::write(&path, bytes)?;
    if std::env::var("SEED_STUDIO_EXPORT_DIR").is_err() && cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
    }
    Ok(json!({"path": path.to_string_lossy()}))
}

/// Writes the whole request log as CSV into Downloads (never the key or any content).
fn api_log_export(core: &Core) -> Res<Value> {
    let (text, rows) = core.db(|c| crate::apilog::csv(c))?;
    let dir = std::env::var("SEED_STUDIO_EXPORT_DIR").map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads")))
        .map_err(|_| AppError::new("io", "Could not find the Downloads folder"))?;
    std::fs::create_dir_all(&dir)?;
    let stamp = crate::apilog::iso(now_ms()).replace(':', "").replace('-', "");
    let path = dir.join(format!("seed-studio-api-log-{}.csv", stamp.trim_end_matches('Z')));
    std::fs::write(&path, text)?;
    if std::env::var("SEED_STUDIO_EXPORT_DIR").is_err() && cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
    }
    Ok(json!({"path": path.to_string_lossy(), "rows": rows}))
}

pub async fn dispatch(core: &Core, cmd: &str, args: Value) -> Res<Value> {
    match cmd {
        "api_log_list" => core.db(|c| crate::apilog::list(c, args["paidOnly"].as_bool().unwrap_or(false), args["limit"].as_i64().unwrap_or(200), args["offset"].as_i64().unwrap_or(0))),
        "api_log_export" => api_log_export(core),
        "app_state" => app_state(core),
        "setting_set" => {
            set_setting(core, &s(&args, "key")?, &s(&args, "value")?)?;
            app_state(core)
        }
        "key_set" => {
            let region = s(&args, "region")?;
            let key = s(&args, "key")?;
            if !core.catalog.regions.contains_key(&region) || key.trim().is_empty() {
                return Err(AppError::new("bad_args", "Invalid region or key"));
            }
            // People often paste the header form or a quoted value.
            let cleaned: String = key.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string();
            let cleaned = cleaned.strip_prefix("Bearer ").or_else(|| cleaned.strip_prefix("bearer ")).unwrap_or(&cleaned).trim().to_string();
            if cleaned.is_empty() || cleaned.chars().any(|c| c.is_whitespace()) {
                return Err(AppError::new("bad_args", "That does not look like an API key (it contains spaces or line breaks)."));
            }
            core.secrets.set(&secrets::account(PROFILE, &region), &cleaned)?;
            core.note_key(&region, true);
            app_state(core)
        }
        "key_delete" => {
            let region = s(&args, "region")?;
            core.secrets.delete(&secrets::account(PROFILE, &region))?;
            core.note_key(&region, false);
            app_state(core)
        }
        "check_connection" => check_connection(core).await,
        "storyboard_write" => storyboard_write(core, &args).await,
        "gateway_balance" => {
            let set = core.providers()?;
            set.chat.balance().await.map_err(|e| e.to_app(&set.region, "GET balance"))
        }
        "model_test" => model_test(core, &args).await,
        "audio_import" => crate::voice::audio_import(core, &s(&args, "projectId")?, &s(&args, "name")?, b64(&s(&args, "base64")?)?),
        "audio_get" => crate::voice::audio_row_by(core, &s(&args, "projectId")?, args["audioId"].as_str()),
        "video_plan" => { crate::library::ensure(core).await; video::plan(core, &args) }
        "video_plan_node" => { crate::library::ensure(core).await; video::plan_from_node(core, &s(&args, "projectId")?, &s(&args, "nodeId")?) }
        "library_add" => {
            // The owner's own click on one image node: add that image to the gateway library now.
            let (project_id, asset_id) = (s(&args, "projectId")?, s(&args, "assetId")?);
            if !core.db(|c| Core::asset_ids(c, &project_id))?.contains(&asset_id) {
                return Err(AppError::new("forbidden", "That image does not belong to this project"));
            }
            let lib = crate::library::cached(core);
            if crate::library::resolve(core, &lib, &asset_id, None, &[]).ok().flatten().is_none() {
                let set = core.providers()?;
                crate::library::upload(core, &set, &asset_id).await?;
                let _ = crate::library::refresh(core).await;
            }
            crate::library::matches(core, &project_id)
        }
        "elements_list" => crate::elements::list(core, &s(&args, "projectId")?),
        "element_save" => crate::elements::save(core, &args),
        "element_delete" => crate::elements::delete(core, &s(&args, "projectId")?, &s(&args, "id")?),
        "gen_stats" => gen_stats(core, &s(&args, "projectId")?),
        // Team (the owner's Supabase project). `cloud_state` reads nothing but settings.
        "cloud_state" => Ok(crate::cloud::state(core)),
        "cloud_configure" => crate::cloud::configure(core, &s(&args, "url")?, &s(&args, "key")?).await,
        "cloud_sign_in" => crate::cloud::sign_in(core, &s(&args, "email")?, &s(&args, "password")?).await,
        "cloud_sign_out" => crate::cloud::sign_out(core).await,
        "cloud_change_password" => crate::cloud::change_password(core, &s(&args, "password")?).await,
        "cloud_rename_me" => crate::cloud::rename_me(core, &s(&args, "name")?).await,
        "cloud_people" => crate::cloud::people(core).await,
        "cloud_sessions" => crate::cloud::sessions(core).await,
        "cloud_session_create" => crate::cloud::session_create(core, &s(&args, "name")?).await,
        "cloud_session_rename" => crate::cloud::session_rename(core, &s(&args, "sessionId")?, &s(&args, "name")?).await,
        "cloud_session_delete" => crate::cloud::session_delete(core, &s(&args, "sessionId")?).await,
        "cloud_member_add" => crate::cloud::member_add(core, &s(&args, "sessionId")?, &s(&args, "userId")?).await,
        "cloud_member_remove" => crate::cloud::member_remove(core, &s(&args, "sessionId")?, &s(&args, "userId")?).await,
        "cloud_admin" => crate::cloud::admin(core, args.clone()).await,
        // Look: camera is one text request; colour and palette are local arithmetic.
        "camera_describe" => crate::look::camera(core, &s(&args, "projectId")?, &s(&args, "assetId")?).await,
        "colour_match" => crate::look::colour_version(core, &s(&args, "projectId")?, &s(&args, "nodeId")?, &s(&args, "fromAssetId")?, args["strength"].as_f64().unwrap_or(1.0) as f32),
        "palette_asset" => crate::look::palette_asset(core, &s(&args, "projectId")?, &s(&args, "fromAssetId")?),
        "gateway_library" => {
            if args["refresh"].as_bool() == Some(true) { crate::library::refresh(core).await?; }
            crate::library::matches(core, &s(&args, "projectId")?)
        }
        "video_start" => video::start(core, &s(&args, "videoId")?).await,
        "video_poll" => video::poll(core, &s(&args, "videoId")?).await,
        "videos_list" => video::list(core, &s(&args, "projectId")?),
        "video_export" => video::export(core, &s(&args, "videoId")?),
        "file_import" => file_import(core, &args),
        // The UI reports that it booted and whether project images can be displayed.
        "ui_ready" => {
            core.set_setting("ui_ready_at", &now_ms().to_string())?;
            core.set_setting("ui_image_channel", if args["imageChannel"].as_bool() == Some(true) { "ok" } else { "failed" })?;
            Ok(json!({"ok": true}))
        }

        "projects_list" => core.db(|c| {
            let mut st = c.prepare("SELECT id, name, instructions, updated_at,
                (SELECT count(*) FROM assets WHERE project_id=projects.id), (SELECT count(*) FROM threads WHERE project_id=projects.id AND kind='chat')
                FROM projects ORDER BY updated_at DESC")?;
            let rows = st.query_map([], |r| Ok(json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?,
                "instructions": r.get::<_, String>(2)?, "updatedAt": r.get::<_, i64>(3)?,
                "assetCount": r.get::<_, i64>(4)?, "threadCount": r.get::<_, i64>(5)?})))?.collect::<Result<Vec<_>, _>>()?;
            Ok(json!(rows))
        }),
        "project_create" => {
            let name = s(&args, "name")?;
            let name: String = name.trim().chars().take(80).collect();
            if name.is_empty() { return Err(AppError::new("bad_args", "Enter a name for your project.")); }
            let id = new_id();
            core.db(|c| {
                c.execute("INSERT INTO projects(id,name,created_at,updated_at) VALUES(?1,?2,?3,?3)", params![id, name, now_ms()])?;
                c.execute("INSERT INTO threads(id,project_id,kind,title,created_at,updated_at) VALUES(?1,?2,'chat','New chat',?3,?3)", params![new_id(), id, now_ms()])?;
                Core::load_canvas(c, &id)?;
                Ok(json!({"id": id, "name": name}))
            })
        }
        "project_update" => {
            let id = s(&args, "projectId")?;
            core.db(|c| {
                if let Some(name) = so(&args, "name").filter(|n| !n.trim().is_empty()) {
                    c.execute("UPDATE projects SET name=?1, updated_at=?2 WHERE id=?3", params![name.trim().chars().take(80).collect::<String>(), now_ms(), id])?;
                }
                if let Some(ins) = so(&args, "instructions") {
                    c.execute("UPDATE projects SET instructions=?1, updated_at=?2 WHERE id=?3", params![ins.chars().take(8000).collect::<String>(), now_ms(), id])?;
                }
                Ok(json!({"ok": true}))
            })
        }
        "project_delete" => {
            let id = s(&args, "projectId")?;
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(AppError::new("bad_args", "Invalid project id"));
            }
            core.db(|c| {
                c.execute("DELETE FROM projects WHERE id=?1", [&id])?;
                c.execute("DELETE FROM settings WHERE key='last_project' AND value=?1", [&id])?;
                Ok(())
            })?;
            let _ = std::fs::remove_dir_all(core.data_dir.join("assets").join(&id));
            Ok(json!({"ok": true}))
        }

        "threads_list" => {
            let project_id = s(&args, "projectId")?;
            core.db(|c| {
                let mut st = c.prepare("SELECT id, title, updated_at, (SELECT count(*) FROM messages WHERE thread_id=threads.id) FROM threads WHERE project_id=?1 AND kind='chat' ORDER BY updated_at DESC")?;
                let rows = st.query_map([&project_id], |r| Ok(json!({"id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "updatedAt": r.get::<_, i64>(2)?, "messageCount": r.get::<_, i64>(3)?})))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!(rows))
            })
        }
        "thread_create" => {
            let project_id = s(&args, "projectId")?;
            let id = new_id();
            core.db(|c| {
                c.execute("INSERT INTO threads(id,project_id,kind,title,created_at,updated_at) VALUES(?1,?2,'chat','New chat',?3,?3)", params![id, project_id, now_ms()])?;
                Ok(json!({"id": id, "title": "New chat"}))
            })
        }
        "thread_rename" => core.db(|c| {
            c.execute("UPDATE threads SET title=?1 WHERE id=?2", params![s(&args, "title")?.chars().take(80).collect::<String>(), s(&args, "threadId")?])?;
            Ok(json!({"ok": true}))
        }),
        "thread_delete" => core.db(|c| {
            c.execute("DELETE FROM threads WHERE id=?1 AND kind='chat'", [s(&args, "threadId")?])?;
            Ok(json!({"ok": true}))
        }),
        "agent_thread" => {
            let project_id = s(&args, "projectId")?;
            core.db(|c| {
                let existing: Option<String> = c.query_row("SELECT id FROM threads WHERE project_id=?1 AND kind='agent' LIMIT 1", [&project_id], |r| r.get(0)).optional()?;
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let id = new_id();
                        c.execute("INSERT INTO threads(id,project_id,kind,title,created_at,updated_at) VALUES(?1,?2,'agent','Canvas assistant',?3,?3)", params![id, project_id, now_ms()])?;
                        id
                    }
                };
                Ok(json!({"id": id, "messages": message_rows(c, &id)?}))
            })
        }
        "messages_list" => core.db(|c| Ok(json!(message_rows(c, &s(&args, "threadId")?)?))),
        "message_pin" => core.db(|c| {
            c.execute("UPDATE messages SET pinned=?1 WHERE id=?2", params![args["pinned"].as_bool().unwrap_or(false) as i64, s(&args, "messageId")?])?;
            Ok(json!({"ok": true}))
        }),
        "chat_send" => chat_send(core, &args).await,
        "chat_cancel" => {
            core.cancel(&s(&args, "threadId")?);
            Ok(json!({"ok": true}))
        }
        "chat_image_flow" => chat_image_flow(core, &args),

        "canvas_get" => core.canvas_state(&s(&args, "projectId")?),
        "canvas_patch" => {
            let project_id = s(&args, "projectId")?;
            let ops: Vec<Op> = serde_json::from_value(args["operations"].clone())
                .map_err(|e| AppError::new("invalid_patch", format!("Invalid operation: {e}")))?;
            let label = so(&args, "label").unwrap_or_else(|| "Edit".into());
            let r = core.apply_patch(&project_id, args["expectedRevision"].as_i64(), &ops, &Actor::User, &label)?;
            let mut state = core.canvas_state(&project_id)?;
            state["createdIds"] = json!(r.outcome.created_ids);
            state["notes"] = json!(r.outcome.notes);
            Ok(state)
        }
        "canvas_viewport" => {
            let v = &args["viewport"];
            if !(v["x"].is_number() && v["y"].is_number() && v["zoom"].is_number()) {
                return Err(AppError::new("bad_args", "Invalid viewport"));
            }
            core.db(|c| {
                c.execute("UPDATE canvases SET viewport_json=?1 WHERE project_id=?2",
                    params![json!({"x": v["x"], "y": v["y"], "zoom": v["zoom"]}).to_string(), s(&args, "projectId")?])?;
                Ok(json!({"ok": true}))
            })
        }
        "canvas_undo" | "canvas_redo" => {
            let project_id = s(&args, "projectId")?;
            let label = core.undo_redo(&project_id, cmd == "canvas_undo")?;
            let mut state = core.canvas_state(&project_id)?;
            state["label"] = json!(label);
            Ok(state)
        }
        "canvas_import" => {
            let project_id = s(&args, "projectId")?;
            let doc: Value = serde_json::from_str(&s(&args, "json")?).map_err(|_| AppError::new("bad_import", "The file is not valid JSON"))?;
            let mut files = HashMap::new();
            if let Some(m) = args["files"].as_object() {
                for (k, v) in m {
                    files.insert(k.clone(), b64(v.as_str().unwrap_or(""))?);
                }
            }
            import_canvas(core, &project_id, &doc, &files)?;
            core.canvas_state(&project_id)
        }
        "canvas_options" => core.catalog.options(&s(&args, "modelKey")?).ok_or_else(|| AppError::new("not_found", "Unknown model")),

        "asset_import" => {
            let project_id = s(&args, "projectId")?;
            core.db(|c| Core::load_canvas(c, &project_id).map(|_| ()))?;
            let file = core.write_asset_file(&project_id, &b64(&s(&args, "base64")?)?)?;
            let title: String = so(&args, "name").unwrap_or_else(|| "Reference image".into()).chars().take(80).collect();
            core.insert_reference_asset(&project_id, &file, &title)?;
            Ok(json!({"id": file.id, "mime": file.mime, "width": file.width, "height": file.height, "title": title}))
        }
        // An image the user dragged or pasted from a web page: fetched once, stored locally like any reference.
        "asset_import_url" => {
            let project_id = s(&args, "projectId")?;
            let url = s(&args, "url")?;
            if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 4000 {
                return Err(AppError::new("bad_image", "Only http(s) image links can be imported"));
            }
            core.db(|c| Core::load_canvas(c, &project_id).map(|_| ()))?;
            let cannot = || AppError::new("bad_image", "That image could not be downloaded. Save it to this Mac and drop the file instead.");
            let client = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10)).timeout(std::time::Duration::from_secs(40)).build().map_err(|_| cannot())?;
            let resp = client.get(&url).header("Accept", "image/png,image/jpeg,image/webp,image/*;q=0.8").send().await.map_err(|_| cannot())?;
            if !resp.status().is_success() || resp.content_length().map(|n| n > 30_000_000).unwrap_or(false) { return Err(cannot()); }
            let bytes = resp.bytes().await.map_err(|_| cannot())?;
            if bytes.len() > 30_000_000 { return Err(cannot()); }
            let file = core.write_asset_file(&project_id, &bytes)?;
            let title: String = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().filter(|t| !t.is_empty()).unwrap_or("Web image")
                .rsplit_once('.').map(|(a, _)| a).unwrap_or("Web image").chars().take(80).collect();
            core.insert_reference_asset(&project_id, &file, &title)?;
            Ok(json!({"id": file.id, "mime": file.mime, "width": file.width, "height": file.height, "title": title}))
        }
        "bundle_ingest" => crate::bundle::ingest(core, &s(&args, "projectId")?, &s(&args, "name")?, b64(&s(&args, "base64")?)?),
        "bundle_write" => crate::bundle::write(core, &args).await,
        "assets_list" => core.db(|c| Ok(json!(asset_rows(c, &s(&args, "projectId")?)?))),
        "node_use_version" => {
            // Make an earlier result of a node its current one. Nothing is sent: the earlier
            // run is recorded again as the newest, so the canvas, later runs and videos all use it.
            let (project_id, node_id, asset_id) = (s(&args, "projectId")?, s(&args, "nodeId")?, s(&args, "assetId")?);
            let done = core.db(|c| Ok(c.execute(
                "INSERT INTO node_runs(id,run_id,project_id,node_id,kind,status,fingerprint,provider,region,model_id,final_prompt,settings_json,asset_id,text_output,error,usage_json,started_at,finished_at)
                 SELECT ?1,run_id,project_id,node_id,kind,status,fingerprint,provider,region,model_id,final_prompt,settings_json,asset_id,text_output,NULL,NULL,?2,?2
                 FROM node_runs WHERE project_id=?3 AND node_id=?4 AND asset_id=?5 AND status='succeeded' ORDER BY finished_at DESC LIMIT 1",
                params![new_id(), now_ms(), project_id, node_id, asset_id])?))?;
            if done == 0 { return Err(AppError::new("not_found", "That version does not belong to this node")); }
            core.emit("canvas-changed", json!({"projectId": project_id}));
            core.canvas_state(&project_id)
        }
        "asset_export" => export_asset(core, &s(&args, "assetId")?),

        "plan_run" => {
            let plan = engine::build_plan(core, &s(&args, "projectId")?, &strs(&args, "nodeIds"), args["forceNew"].as_bool().unwrap_or(false))?;
            Ok(serde_json::to_value(plan)?)
        }
        "run_execute" => engine::execute(core, &s(&args, "planId")?, so(&args, "threadId")).await,
        "run_cancel" => {
            core.cancel(&s(&args, "runId")?);
            Ok(json!({"ok": true}))
        }
        "agent_send" => agent::send(core, &s(&args, "projectId")?, &s(&args, "threadId")?, &s(&args, "text")?, &strs(&args, "selection")).await,
        _ => Err(AppError::new("unknown_command", format!("Unknown command: {cmd}"))),
    }
}
