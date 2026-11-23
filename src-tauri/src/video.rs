//! Seedance video: plan → (user confirms) → start one task → poll until the file
//! is saved locally. The task id is stored as soon as the provider returns it, so
//! polling resumes after the app is reopened. Creating a task is never retried.
use crate::core::{new_id, now_ms, Core};
use crate::error::{AppError, Res};
use crate::provider::{ErrKind, VideoImage, VideoRequest};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};

pub const VIDEO_MODEL: &str = "seedance-2.5";
const COLS: &str = "id, project_id, thread_id, kind, draft_of, status, task_id, provider, region, model_key, model_id, prompt, params_json, inputs_json, rel_path, error, created_at, updated_at, node_id, shot_id, usage_json";

fn row_json(r: &Row) -> rusqlite::Result<Value> {
    let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
    Ok(json!({
        "id": r.get::<_, String>(0)?, "projectId": r.get::<_, String>(1)?, "threadId": r.get::<_, Option<String>>(2)?,
        "kind": r.get::<_, String>(3)?, "draftOf": r.get::<_, Option<String>>(4)?, "status": r.get::<_, String>(5)?,
        "taskId": r.get::<_, Option<String>>(6)?, "provider": r.get::<_, String>(7)?, "region": r.get::<_, String>(8)?,
        "modelKey": r.get::<_, String>(9)?, "modelId": r.get::<_, String>(10)?, "prompt": r.get::<_, String>(11)?,
        "params": parse(r.get(12)?), "inputs": parse(r.get(13)?),
        "hasFile": r.get::<_, Option<String>>(14)?.is_some(), "error": r.get::<_, Option<String>>(15)?,
        "createdAt": r.get::<_, i64>(16)?, "updatedAt": r.get::<_, i64>(17)?, "nodeId": r.get::<_, Option<String>>(18)?, "shotId": r.get::<_, Option<String>>(19)?,
        "usage": r.get::<_, Option<String>>(20)?.map(parse),
    }))
}

pub fn get(c: &Connection, id: &str) -> Res<Value> {
    c.query_row(&format!("SELECT {COLS} FROM videos WHERE id=?1"), [id], row_json).optional()?
        .ok_or_else(|| AppError::new("not_found", "Video not found"))
}

pub fn list(core: &Core, project_id: &str) -> Res<Value> {
    core.db(|c| {
        let mut st = c.prepare(&format!("SELECT {COLS} FROM videos WHERE project_id=?1 AND status!='planned' ORDER BY created_at DESC"))?;
        let rows = st.query_map([project_id], row_json)?.collect::<Result<Vec<_>, _>>()?;
        Ok(json!(rows))
    })
}

/// Seedance's reference mode (the only one that takes a voice clip or a library image) treats an
/// image as a loose reference unless the prompt names it as the opening frame by its tag.
fn frame_prefix(last: bool) -> String {
    let ends = if last { " and @Image 2 is its last frame" } else { "" };
    format!("@Image 1 is the first frame of the video{ends}: open on exactly this image, with the same framing, camera angle, pose, people, faces, clothing, set, props and lighting, and continue the action from it without recomposing or cutting away. ")
}
fn voice_prefix(with_image: bool) -> &'static str {
    if with_image { "@Audio 1 is the voice of the person in @Image 1: they say it on camera with lips matched to @Audio 1, and no other voice is heard. " }
    else { "@Audio 1 is the speaking voice: the character says it on camera with lips matched to @Audio 1, and no other voice is heard. " }
}

/// Seedance only links words to an attachment through its tag, so "the connected image" or
/// "the supplied audio" in a prompt points at nothing. Those phrases become @Image 1 / @Audio 1.
fn point_at(prompt: &str, image: bool, audio: bool) -> (String, bool) {
    let mut out = prompt.to_string();
    let mut swap = |phrases: &[&str], tag: &str| {
        for ph in phrases {
            loop {
                let Some(at) = out.to_lowercase().find(ph) else { break };
                // Lowercasing keeps byte offsets for these ASCII phrases only when the text before is unchanged in length.
                if !out.is_char_boundary(at) || !out.is_char_boundary(at + ph.len()) || out[at..at + ph.len()].to_lowercase() != *ph { break; }
                out.replace_range(at..at + ph.len(), tag);
            }
        }
    };
    if audio { swap(&["the connected audio clip", "the connected audio", "the attached audio clip", "the attached audio", "the supplied audio clip", "the supplied audio", "the supplied voice"], "@Audio 1"); }
    if image { swap(&["the connected image", "the attached image", "the supplied image", "the supplied frame", "the start frame", "the starting frame"], "@Image 1"); }
    let changed = out != prompt;
    (out, changed)
}

/// A prompt pasted as a JSON object (a "shot card") is turned into plain sentences: the model
/// reads the prompt as text, and fields such as model, duration or aspect ratio are settings here, not words to film.
fn flatten_json_prompt(prompt: &str) -> Option<String> {
    let t = prompt.trim();
    if !t.starts_with('{') { return None; }
    let v: Value = serde_json::from_str(t).ok()?;
    let obj = v.as_object()?;
    const SKIP: [&str; 12] = ["shot", "title", "name", "model", "duration", "aspect_ratio", "aspectratio", "ratio", "resolution", "fps", "id", "number"];
    fn walk(key: &str, v: &Value, out: &mut Vec<String>) {
        let k = key.to_lowercase();
        if SKIP.contains(&k.as_str()) { return; }
        match v {
            Value::String(s) if !s.trim().is_empty() => {
                let s = s.trim();
                let end = if s.ends_with(['.', '!', '?', '"', '”']) { "" } else { "." };
                out.push(match k.as_str() {
                    "prompt" | "input" | "description" | "action" | "scene" | "notes" => format!("{s}{end}"),
                    "spoken_transcript" | "transcript" | "dialogue" | "line" => format!("The spoken line is: \"{s}\"."),
                    "negative" | "negative_prompt" | "avoid" => format!("Avoid: {s}{end}"),
                    _ => format!("{}: {s}{end}", key.replace('_', " ")),
                });
            }
            Value::Object(m) => {
                let mut inner = vec![];
                for (ik, iv) in m { walk(ik, iv, &mut inner); }
                if !inner.is_empty() { out.push(format!("{}: {}", capital(&key.replace('_', " ")), inner.join(" "))); }
            }
            Value::Array(a) => {
                let items: Vec<&str> = a.iter().filter_map(|x| x.as_str()).collect();
                if !items.is_empty() { out.push(format!("{}: {}.", capital(&key.replace('_', " ")), items.join(", "))); }
            }
            _ => {}
        }
    }
    fn capital(s: &str) -> String { let mut c = s.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }
    let mut out = vec![];
    for (k, v) in obj { walk(k, v, &mut out); }
    (!out.is_empty()).then(|| out.join(" "))
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Validate a request against the documented Seedance 2.5 rules and store it as
/// a plan. Nothing is sent. Returns the plan row plus notes about adjusted options.
pub fn plan(core: &Core, args: &Value) -> Res<Value> {
    let bad = |m: String| AppError::new("bad_video_request", m);
    let project_id = args["projectId"].as_str().ok_or_else(|| bad("projectId is required".into()))?.to_string();
    let thread_id = args["threadId"].as_str().map(String::from);
    let draft_source = args["draftOf"].as_str().map(|id| core.db(|c| get(c, id))).transpose()?;
    let model_key = draft_source.as_ref().and_then(|v| v["modelKey"].as_str())
        .or_else(|| args["modelKey"].as_str()).unwrap_or(VIDEO_MODEL);
    let model = core.catalog.model(model_key).filter(|m| m.kind == "video")
        .ok_or_else(|| bad("Choose a video model from the catalog".into()))?;
    let controls = model.controls.clone();
    let model_id = match &draft_source {
        Some(draft) => draft["modelId"].as_str().ok_or_else(|| bad("The draft has no model ID".into()))?.to_string(),
        None => core.resolve_model_id(model_key)?,
    };
    let (provider, region) = if core.provider_mode() == "mock" { ("mock".to_string(), "-".to_string()) } else { ("modelark".to_string(), core.region()) };
    let mut notes: Vec<String> = vec![];
    let mut wire = Map::new();

    // Final video from an approved draft: only the draft id and the output resolution are sent.
    if let Some(draft_id) = args["draftOf"].as_str() {
        let draft = draft_source.as_ref().unwrap();
        if args["modelKey"].as_str().is_some_and(|key| key != model_key) {
            return Err(bad("A final video must use the same model as its draft".into()));
        }
        if controls["draftMode"] != true { return Err(bad(format!("{} does not support draft videos", model.label))); }
        if draft["region"] != region.as_str() { return Err(bad("Switch back to the draft’s connection before upscaling it".into())); }
        if draft["projectId"] != project_id.as_str() { return Err(bad("That draft belongs to another project".into())); }
        if draft["kind"] != "draft" || draft["status"] != "succeeded" { return Err(bad("Only a finished draft can be upscaled".into())); }
        if draft["provider"] != provider.as_str() { return Err(bad(format!("This draft was made with “{}”; switch the provider back to upscale it", draft["provider"].as_str().unwrap_or("")))); }
        let resolution = args["resolution"].as_str().unwrap_or("1080p");
        let allowed = strs(&controls["finalFromDraft"]);
        if !allowed.iter().any(|r| r == resolution) {
            return Err(bad(format!("A final video made from a draft can only be {}", allowed.join(" or "))));
        }
        wire.insert("resolution".into(), json!(resolution));
        let id = new_id();
        core.db(|c| {
            c.execute("INSERT INTO videos(id,project_id,thread_id,kind,draft_of,status,provider,region,model_key,model_id,prompt,params_json,inputs_json,created_at,updated_at,node_id,shot_id)
                       VALUES(?1,?2,?3,'final',?4,'planned',?5,?6,?7,?8,?9,?10,?11,?12,?12,?13,?14)",
                params![id, project_id, thread_id, draft_id, provider, region, model_key, model_id, draft["prompt"].as_str().unwrap_or(""),
                        Value::Object(wire.clone()).to_string(), draft["inputs"].to_string(), now_ms(), draft["nodeId"].as_str(), draft["shotId"].as_str()])?;
            Ok(())
        })?;
        let mut row = core.db(|c| get(c, &id))?;
        row["notes"] = json!(["The final video reuses the draft’s prompt, frames and shape; only the resolution is sent with the draft id."]);
        row["modelLabel"] = json!(model.label);
        return Ok(row);
    }

    let mut prompt = args["prompt"].as_str().unwrap_or("").trim().to_string();
    let supports_draft = controls["draftMode"] == true;
    let is_draft = args["draft"].as_bool().unwrap_or(supports_draft);
    if is_draft && !supports_draft { return Err(bad(format!("{} does not support draft videos", model.label))); }
    let assets = core.db(|c| Core::asset_ids(c, &project_id))?;
    let mut inputs: Vec<Value> = vec![];
    let (mut first, mut last, mut refs) = (0, 0, 0);
    // Images the gateway holds in its character library go by reference (`asset://`), not as files.
    let library = if provider == "mock" { vec![] } else { crate::library::cached(core) };
    let mut from_library: Vec<String> = vec![];
    let uploads = provider != "mock" && crate::library::uploads_on(core);
    let mut to_upload: Vec<String> = vec![];
    for i in args["inputs"].as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
        let role = i["role"].as_str().unwrap_or("");
        let aid = i["assetId"].as_str().unwrap_or("");
        if !assets.contains(aid) { return Err(bad("An attached image does not belong to this project".into())); }
        match role {
            "first_frame" => first += 1,
            "last_frame" => last += 1,
            "reference_image" => refs += 1,
            _ => return Err(bad(format!("Unknown image role: {role}"))),
        }
        let mut input = json!({"role": role, "assetId": aid});
        if !library.is_empty() || uploads {
            let title: String = core.db(|c| Ok(c.query_row("SELECT title FROM assets WHERE id=?1", [aid], |r| r.get(0)).unwrap_or_default()))?;
            let label = i["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&title).to_string();
            if let Some(a) = crate::library::resolve(core, &library, aid, i["library"].as_str(), &[&title, i["name"].as_str().unwrap_or("")]).map_err(&bad)? {
                from_library.push(a["name"].as_str().unwrap_or("").to_string());
                input["library"] = json!({"id": a["id"], "name": a["name"]});
            } else if uploads && i["library"].as_str() != Some("off") {
                // Not in the library yet: it is added first, when the request is sent.
                to_upload.push(label.clone());
                input["library"] = json!({"pending": true, "name": label});
            }
        }
        inputs.push(input);
    }
    inputs.sort_by_key(|i| match i["role"].as_str() { Some("first_frame") => 0, Some("last_frame") => 1, _ => 2 });
    if first > 1 || last > 1 { return Err(bad("Use at most one first frame and one last frame".into())); }
    if last == 1 && first == 0 { return Err(bad("A last frame needs a first frame".into())); }
    if (first + last) > 0 && refs > 0 {
        return Err(bad("First/last frames and reference images are separate modes in Seedance and cannot be mixed. Use frames, or references.".into()));
    }
    if !to_upload.is_empty() {
        notes.push(format!("Before the video is requested, {} added to the gateway’s character library, declared AI-generated with no real person (your setting): {}. Each uses one place in the library.",
            if to_upload.len() == 1 { "1 image is".to_string() } else { format!("{} images are", to_upload.len()) }, to_upload.join(", ")));
    }
    if !from_library.is_empty() {
        notes.push(format!("Sent from the gateway’s character library instead of the file, so the face is accepted: {}.", from_library.join(", ")));
    }
    if !from_library.is_empty() || !to_upload.is_empty() {
        // The library is only accepted as reference images, so frames become references the prompt points at.
        if first + last > 0 {
            for i in inputs.iter_mut() { i["role"] = json!("reference_image"); }
            prompt = format!("{}{}", frame_prefix(last == 1), point_at(&prompt, last == 0, false).0);
            notes.push("Library images go in as reference images, so the frame is sent as @Image 1 and the prompt asks to open on it.".into());
            refs += first + last;
            first = 0;
            last = 0;
        }
    }
    // A voice clip (reference audio) puts the request in Seedance's omni-reference mode.
    if let Some(a) = args.get("audio").filter(|a| a.is_object()) {
        let (start, end) = (a["startMs"].as_i64().unwrap_or(0), a["endMs"].as_i64().unwrap_or(0));
        let owner: Option<String> = core.db(|c| Ok(c.query_row("SELECT project_id FROM audio WHERE id=?1", [a["audioId"].as_str().unwrap_or("")], |r| r.get(0)).optional()?))?;
        if owner.as_deref() != Some(project_id.as_str()) { return Err(bad("The voice track does not belong to this project".into())); }
        let max = controls["audio"]["maxClipSeconds"].as_i64().unwrap_or(30) * 1000;
        if end <= start || end - start > max { return Err(bad(format!("A voice clip must be longer than 0 and at most {} seconds", max / 1000))); }
        if first + last > 0 { return Err(bad("A voice clip needs the image as a reference image, not as a first or last frame (Seedance treats them as separate modes)".into())); }
        wire.insert("_audio".into(), json!({"audioId": a["audioId"], "startMs": start, "endMs": end}));
    }
    if let Some(g) = args["generateAudio"].as_bool() { wire.insert("generate_audio".into(), json!(g)); }
    if prompt.is_empty() && inputs.is_empty() { return Err(bad("Describe the video, or attach a first frame or reference images".into())); }
    let max_refs = controls["maxReferenceImages"].as_u64().unwrap_or(30) as usize;
    if refs > max_refs { return Err(bad(format!("At most {max_refs} reference images"))); }

    let draft_res = controls["draftResolution"].as_str().unwrap_or("480p").to_string();
    let mut resolution = args["resolution"].as_str().unwrap_or("720p").to_string();
    if is_draft {
        if resolution != draft_res { notes.push(format!("Drafts are always {draft_res}.")); }
        resolution = draft_res;
    } else if !strs(&controls["resolutions"]).contains(&resolution) {
        return Err(bad(format!("Resolution {resolution} is not offered by {}", model.label)));
    }
    let mut ratio = args["ratio"].as_str().unwrap_or("adaptive").to_string();
    if !strs(&controls["ratios"]).contains(&ratio) { return Err(bad(format!("Aspect ratio {ratio} is not offered by {}", model.label))); }
    if (first + last) > 0 && ratio != "adaptive" {
        if args["quietRatio"].as_bool() != Some(true) { notes.push("With a first or last frame, Seedance 2.5 only accepts the adaptive ratio (the shape follows the first frame).".into()); }
        ratio = "adaptive".into();
    }
    let mut duration = args["duration"].as_i64().unwrap_or(-1);
    if let (-1, false, Some(d)) = (duration, provider == "mock", core.gateway()["defaultVideoDuration"].as_i64()) {
        notes.push(format!("This gateway holds budget for the longest possible video when the length is Auto, so {d}s is sent instead. Pick a length to change it."));
        duration = d;
    }
    let (dmin, dmax) = (controls["duration"]["min"].as_i64().unwrap_or(4), controls["duration"]["max"].as_i64().unwrap_or(30));
    if duration != -1 && !(dmin..=dmax).contains(&duration) { return Err(bad(format!("Duration must be Auto or {dmin}–{dmax} seconds"))); }
    wire.insert("resolution".into(), json!(resolution));
    wire.insert("ratio".into(), json!(ratio));
    wire.insert("duration".into(), json!(duration));
    if is_draft { wire.insert("draft".into(), json!(true)); }

    let id = new_id();
    core.db(|c| {
        c.execute("INSERT INTO videos(id,project_id,thread_id,kind,status,provider,region,model_key,model_id,prompt,params_json,inputs_json,created_at,updated_at,node_id,shot_id)
                   VALUES(?1,?2,?3,?4,'planned',?5,?6,?7,?8,?9,?10,?11,?12,?12,?13,?14)",
            params![id, project_id, thread_id, if is_draft { "draft" } else { "final" }, provider, region, model_key, model_id, prompt,
                    Value::Object(wire).to_string(), json!(inputs).to_string(), now_ms(), args["nodeId"].as_str(), args["shotId"].as_str()])?;
        Ok(())
    })?;
    let mut row = core.db(|c| get(c, &id))?;
    row["notes"] = json!(notes);
    row["modelLabel"] = json!(model.label);
    Ok(row)
}

fn set_status(core: &Core, id: &str, status: &str, error: Option<&str>) -> Res<()> {
    core.db(|c| {
        c.execute("UPDATE videos SET status=?1, error=?2, updated_at=?3 WHERE id=?4", params![status, error, now_ms(), id])?;
        Ok(())
    })
}

/// Send the reviewed plan. A plan can be claimed once, so a double click cannot
/// create two paid tasks.
pub async fn start(core: &Core, video_id: &str) -> Res<Value> {
    let row = core.db(|c| {
        let claimed = c.execute("UPDATE videos SET status='submitting', updated_at=?1 WHERE id=?2 AND status='planned'", params![now_ms(), video_id])?;
        if claimed == 0 {
            return Err(AppError::new("video_already_started", "This video request was already sent; no second task was created."));
        }
        get(c, video_id)
    })?;
    let project_id = row["projectId"].as_str().unwrap_or("").to_string();
    let fail = |e: AppError| { let _ = set_status(core, video_id, "failed", Some(&e.message)); e };
    let set = core.providers().map_err(&fail)?;
    if set.name != row["provider"].as_str().unwrap_or("") {
        return Err(fail(AppError::new("bad_video_request", "The provider changed after this plan was shown. Review a new plan.")));
    }
    if set.region != row["region"].as_str().unwrap_or("") {
        return Err(fail(AppError::new("bad_video_request", "The connection changed after this plan was shown. Review a new plan.")));
    }
    let model_id = row["modelId"].as_str().unwrap_or("").to_string();
    let mut req = VideoRequest {
        model: model_id.clone(), prompt: row["prompt"].as_str().unwrap_or("").to_string(), images: vec![], audio: vec![], draft_task_id: None,
        params: row["params"].as_object().cloned().unwrap_or_default(),
    };
    if let Some(draft_id) = row["draftOf"].as_str() {
        let task: Option<String> = core.db(|c| Ok(c.query_row("SELECT task_id FROM videos WHERE id=?1", [draft_id], |r| r.get(0)).optional()?.flatten())).map_err(&fail)?;
        req.draft_task_id = Some(task.ok_or_else(|| fail(AppError::new("bad_video_request", "The draft has no task id to build on")))?);
    } else {
        // Images marked for the library are added first; the request then names them by reference.
        let mut inputs = row["inputs"].as_array().cloned().unwrap_or_default();
        let mut added = false;
        for i in inputs.iter_mut().filter(|i| i["library"]["pending"] == true) {
            let a = crate::library::upload(core, &set, i["assetId"].as_str().unwrap_or("")).await.map_err(&fail)?;
            i["library"] = a;
            added = true;
        }
        if added {
            core.db(|c| { c.execute("UPDATE videos SET inputs_json=?1 WHERE id=?2", params![json!(inputs).to_string(), video_id])?; Ok(()) }).map_err(&fail)?;
            let _ = crate::library::refresh(core).await;
        }
        for i in &inputs {
            if let Some(id) = i["library"]["id"].as_str() {
                if !crate::library::available(core) {
                    return Err(fail(AppError::new("bad_video_request", "The connection changed after this plan was shown. Review a new plan.")));
                }
                req.images.push(VideoImage { role: i["role"].as_str().unwrap_or("").to_string(), mime: String::new(), bytes: vec![], library: Some(id.to_string()) });
                continue;
            }
            let (owner, mime, _, bytes) = core.asset_bytes(i["assetId"].as_str().unwrap_or("")).map_err(&fail)?;
            if owner != project_id {
                return Err(fail(AppError::new("forbidden", "Image from another project")));
            }
            req.images.push(VideoImage { role: i["role"].as_str().unwrap_or("").to_string(), mime, bytes, library: None });
        }
        let a = &row["params"]["_audio"];
        if a.is_object() {
            let min = core.catalog.model(row["modelKey"].as_str().unwrap_or(VIDEO_MODEL)).and_then(|m| m.controls["audio"]["minClipSeconds"].as_i64()).unwrap_or(2) * 1000;
            let wav = crate::voice::clip_wav(core, &project_id, a["audioId"].as_str().unwrap_or(""), a["startMs"].as_i64().unwrap_or(0), a["endMs"].as_i64().unwrap_or(0), min).map_err(&fail)?;
            req.audio.push(("audio/wav".into(), wav));
        }
    }
    // Record the request in the conversation before anything is sent.
    if let Some(thread) = row["threadId"].as_str() {
        core.db(|c| {
            let kind = if row["draftOf"].is_string() { "video_upscale" } else { "video_request" };
            c.execute("INSERT INTO messages(id,thread_id,role,content,status,meta_json,created_at) VALUES(?1,?2,'user',?3,'complete',?4,?5)",
                params![new_id(), thread, row["prompt"].as_str().unwrap_or(""), json!({"kind": kind, "videoId": video_id}).to_string(), now_ms()])?;
            c.execute("INSERT INTO messages(id,thread_id,role,content,status,model_id,provider,meta_json,created_at) VALUES(?1,?2,'assistant','','complete',?3,?4,?5,?6)",
                params![new_id(), thread, model_id, set.name, json!({"kind": "video", "videoId": video_id}).to_string(), now_ms() + 1])?;
            c.execute("UPDATE threads SET updated_at=?1, title=CASE WHEN title='New chat' THEN substr(?3,1,40) ELSE title END WHERE id=?2",
                params![now_ms(), thread, if req.prompt.is_empty() { "Video" } else { req.prompt.as_str() }])?;
            Ok(())
        })?;
        core.emit("messages-changed", json!({"threadId": thread}));
    }
    match set.video.create_video(&req).await {
        Ok(task_id) => {
            core.db(|c| {
                c.execute("UPDATE videos SET task_id=?1, status='queued', updated_at=?2 WHERE id=?3", params![task_id, now_ms(), video_id])?;
                Ok(())
            })?;
        }
        Err(e) => {
            if e.kind == ErrKind::Activation { core.mark_needs_activation(&set, &model_id); }
            let status = if e.kind == ErrKind::Uncertain { "uncertain" } else { "failed" };
            set_status(core, video_id, status, Some(&e.display(&set.region, &model_id)))?;
        }
    }
    core.emit("video-update", json!({"projectId": project_id, "videoId": video_id}));
    core.db(|c| get(c, video_id))
}

/// Ask the provider about a running task (a GET, safe to repeat) and save the
/// file when it is ready. Also the way an interrupted session resumes.
pub async fn poll(core: &Core, video_id: &str) -> Res<Value> {
    let row = core.db(|c| get(c, video_id))?;
    let status = row["status"].as_str().unwrap_or("");
    let Some(task_id) = row["taskId"].as_str() else { return Ok(row) };
    if status != "queued" && status != "running" { return Ok(row) }
    let set = core.providers()?;
    if set.name != row["provider"].as_str().unwrap_or("") {
        let mut row = row;
        row["pollNote"] = json!("Switch the provider back to check on this video.");
        return Ok(row);
    }
    if set.region != row["region"].as_str().unwrap_or("") {
        let mut row = row;
        row["pollNote"] = json!("Switch back to this video’s connection to check its progress.");
        return Ok(row);
    }
    let model_id = row["modelId"].as_str().unwrap_or("").to_string();
    let project_id = row["projectId"].as_str().unwrap_or("").to_string();
    let note = |row: Value, text: String| { let mut row = row; row["pollNote"] = json!(text); Ok(row) };
    let task = match set.video.get_video(task_id).await {
        Ok(t) => t,
        Err(e) => return note(row, format!("Could not check the task yet ({}). It will be checked again.", e.message)),
    };
    match task.status.as_str() {
        "succeeded" => {
            let Some(url) = task.video_url else { return note(row, "The task finished but returned no video link yet.".into()) };
            let bytes = match set.video.fetch_video(&url).await {
                Ok(b) => b,
                Err(e) => return note(row, e.message),
            };
            let dir = core.data_dir.join("assets").join(&project_id);
            std::fs::create_dir_all(&dir)?;
            let name = format!("{video_id}.mp4");
            let tmp = dir.join(format!("{name}.tmp"));
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, dir.join(&name))?;
            core.db(|c| {
                c.execute("UPDATE videos SET status='succeeded', rel_path=?1, usage_json=?2, error=NULL, updated_at=?3 WHERE id=?4",
                    params![format!("assets/{project_id}/{name}"), task.usage.map(|u| u.to_string()), now_ms(), video_id])?;
                Ok(())
            })?;
            core.mark_tried(&set, &model_id);
        }
        "failed" | "expired" | "cancelled" => {
            let msg = task.error.unwrap_or_else(|| format!("The task ended as “{}”.", task.status));
            let msg = match crate::provider::content_hint(&msg) { Some(h) => format!("{h} {msg}"), None => msg };
            set_status(core, video_id, "failed", Some(&msg))?;
        }
        other => {
            let s = if other == "queued" { "queued" } else { "running" };
            core.db(|c| { c.execute("UPDATE videos SET status=?1, updated_at=?2 WHERE id=?3", params![s, now_ms(), video_id])?; Ok(()) })?;
        }
    }
    core.emit("video-update", json!({"projectId": project_id, "videoId": video_id}));
    core.db(|c| get(c, video_id))
}

/// Copy a finished video to Downloads.
pub fn export(core: &Core, video_id: &str) -> Res<Value> {
    let (_, _, _, bytes) = core.asset_bytes(video_id)?;
    let dir = std::env::var("SEED_STUDIO_EXPORT_DIR").map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads")))
        .map_err(|_| AppError::new("io", "Could not find the Downloads folder"))?;
    std::fs::create_dir_all(&dir)?;
    let mut path = dir.join("Seed Studio video.mp4");
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("Seed Studio video {n}.mp4"));
        n += 1;
    }
    std::fs::write(&path, bytes)?;
    if std::env::var("SEED_STUDIO_EXPORT_DIR").is_err() && cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
    }
    Ok(json!({"path": path.to_string_lossy()}))
}

/// Build a video plan from a canvas video node: the connected prompt text and
/// the images on its `first`, `last` and `refs` ports, with the node's settings.
pub fn plan_from_node(core: &Core, project_id: &str, node_id: &str) -> Res<Value> {
    let bad = |m: String| AppError::new("bad_video_request", m).with(json!({"nodeId": node_id}));
    let row = core.db(|c| Core::load_canvas(c, project_id))?;
    let g = &row.graph;
    let node = g.node(node_id).filter(|n| n.kind == "seedance").ok_or_else(|| bad("Select a video node".into()))?;
    let title = |id: &str| g.node(id).and_then(|n| n.data.get("title").and_then(|v| v.as_str())).unwrap_or(id).to_string();
    // Latest saved output of an upstream generation node.
    let latest = |source: &str, column: &str| -> Res<Option<String>> {
        core.db(|c| Ok(c.query_row(
            &format!("SELECT {column} FROM node_runs WHERE project_id=?1 AND node_id=?2 AND status='succeeded' AND {column} IS NOT NULL ORDER BY finished_at DESC LIMIT 1"),
            params![project_id, source], |r| r.get::<_, String>(0)).optional()?))
    };
    let prompt = match g.incoming(node_id, "prompt").first() {
        None => node.data.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
        Some(e) => {
            let src = g.node(&e.source).unwrap();
            if src.kind == "prompt" {
                src.data.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string()
            } else {
                latest(&src.id, "text_output")?.ok_or_else(|| bad(format!("Run “{}” first; the video uses its text", title(&src.id))))?
            }
        }
    };
    let mut inputs = vec![];
    for (handle, role) in [("first", "first_frame"), ("last", "last_frame"), ("refs", "reference_image")] {
        for e in g.incoming(node_id, handle) {
            let src = g.node(&e.source).unwrap();
            let asset = if src.kind == "imageAsset" {
                src.data.get("assetId").and_then(|v| v.as_str()).map(String::from).ok_or_else(|| bad(format!("The image node “{}” has no file", title(&src.id))))?
            } else {
                latest(&src.id, "asset_id")?.ok_or_else(|| bad(format!("Run “{}” first; the video uses its image", title(&src.id))))?
            };
            // An image node may name its library entry; its title (the file name) is matched otherwise.
            let own = |k: &str| src.data.get(k).and_then(|v| v.as_str()).filter(|_| src.kind == "imageAsset");
            inputs.push(json!({"role": role, "assetId": asset, "library": own("library"), "name": own("title")}));
        }
    }
    // Elements called as @Name: each image goes in as a reference image.
    let used = crate::elements::mentions(core, project_id, &prompt)?;
    let before_elements = inputs.len();
    let mut element_places: Vec<Option<usize>> = vec![];
    for (_, e) in &used {
        match &e.asset_id {
            Some(a) => { inputs.push(json!({"role": "reference_image", "assetId": a, "name": e.name})); element_places.push(Some(inputs.len())); }
            None => element_places.push(None),
        }
    }
    let s = node.data.get("settings").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let text = |k: &str, d: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string();
    let mut prompt = prompt;
    let mut prompt_notes: Vec<&str> = vec![];
    if let Some(plain) = flatten_json_prompt(&prompt) {
        prompt = plain;
        prompt_notes.push("The prompt was a JSON card, so it was sent as plain sentences; its model, duration and aspect-ratio fields were left out because those are the node’s own settings.");
    }
    let model_key = node.data.get("modelKey").and_then(|v| v.as_str()).unwrap_or(VIDEO_MODEL);
    let supports_draft = core.catalog.model(model_key).is_some_and(|m| m.controls["draftMode"] == true);
    let mut args = json!({
        "projectId": project_id, "nodeId": node_id, "modelKey": model_key,
        "draft": text("pass", if supports_draft { "draft" } else { "final" }) == "draft", "resolution": text("resolution", if supports_draft { "1080p" } else { "720p" }), "ratio": text("ratio", "adaptive"),
        "duration": text("duration", "-1").parse::<i64>().unwrap_or(-1),
        "quietRatio": true,
        "generateAudio": text("sound", "on") == "on" || text("voice", "off") == "on" || !g.incoming(node_id, "audio").is_empty(),
    });
    let mut extra_note = None;
    // An audio node on the `audio` port is the voice clip; otherwise the node's own Voice setting.
    let wired = match g.incoming(node_id, "audio").first() {
        None => None,
        Some(e) => {
            let src = g.node(&e.source).unwrap();
            let audio = crate::voice::audio_row_by(core, project_id, src.data.get("audioId").and_then(|v| v.as_str()))?;
            if !audio.is_object() { return Err(bad(format!("The audio node “{}” has no audio file yet. Select it and import one.", title(&src.id)))); }
            let secs = |k: &str| src.data.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
            let a = (secs("start").unwrap_or(0.0) * 1000.0).round() as i64;
            let whole = audio["durationMs"].as_i64().unwrap_or(0);
            let b = secs("end").filter(|e| *e > 0.0).map(|e| (e * 1000.0).round() as i64).unwrap_or(whole).min(whole);
            Some((audio, a, b))
        }
    };
    // Voice clip for lip sync: Seedance takes it only in reference mode, so the
    // frame is sent as a reference image and the prompt names it as the opening frame.
    if wired.is_some() || text("voice", "off") == "on" {
        let (audio, a, b) = match wired {
            Some(w) => w,
            None => {
                let audio = crate::voice::audio_row(core, project_id)?;
                if !audio.is_object() { return Err(bad("Voice is on, but this project has no voice track. Import one from the node’s Voice section.".into())); }
                let ms = |k: &str| (text(k, "").parse::<f64>().unwrap_or(-1.0) * 1000.0).round() as i64;
                (audio, ms("voiceStart"), ms("voiceEnd"))
            }
        };
        if a < 0 || b <= a { return Err(bad("Voice is on: set where this shot’s clip starts and ends in the voice track.".into())); }
        if inputs.iter().any(|i| i["role"] == "last_frame") { return Err(bad("A voice clip cannot be combined with a last frame. Disconnect the last frame or turn Voice off.".into())); }
        let mut framed = false;
        for i in inputs.iter_mut().filter(|i| i["role"] == "first_frame") { i["role"] = json!("reference_image"); framed = true; }
        let one_image = inputs.len() == 1;
        let (pointed, changed) = point_at(&prompt, one_image, true);
        if changed { prompt_notes.push("Phrases such as “the connected image” or “the connected audio” were sent as @Image 1 / @Audio 1, the only names Seedance links to the attachments."); }
        prompt = format!("{}{}{pointed}", if framed { frame_prefix(false) } else { String::new() }, voice_prefix(!inputs.is_empty()));
        if framed {
            extra_note = Some("With a voice clip, Seedance takes the frame as a reference image, so it is sent as @Image 1 and the prompt asks to open on it. The clip is sent as @Audio 1.");
        }
        args["audio"] = json!({"audioId": audio["id"], "startMs": a, "endMs": b});
    }
    if !used.is_empty() {
        // References cannot be mixed with real first/last frames, so a frame becomes @Image 1 and the prompt opens on it.
        if inputs.len() > before_elements {
            let (has_first, has_last) = (inputs.iter().any(|i| i["role"] == "first_frame"), inputs.iter().any(|i| i["role"] == "last_frame"));
            if has_first {
                for i in inputs.iter_mut() { i["role"] = json!("reference_image"); }
                prompt = format!("{}{prompt}", frame_prefix(has_last));
            }
        }
        prompt = crate::elements::apply(&prompt, &used, |k| element_places[k].map(|n| format!("@Image {n}")));
        prompt_notes.push("Elements called with @ were added as reference images, and their locked descriptions were added to the prompt.");
    }
    args["prompt"] = json!(prompt);
    args["inputs"] = json!(inputs);
    let mut row = plan(core, &args).map_err(|e| if e.detail.is_none() { e.with(json!({"nodeId": node_id})) } else { e })?;
    if let Some(list) = row["notes"].as_array_mut() {
        if let Some(n) = extra_note { list.push(json!(n)); }
        for n in prompt_notes { list.push(json!(n)); }
    }
    Ok(row)
}
