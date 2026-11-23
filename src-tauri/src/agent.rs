//! Canvas assistant: the Seed chat model picks a tool, the app validates and
//! applies it. The model never gets a generation tool — only `plan_run`.
use crate::core::{new_id, now_ms, Actor, Core};
use crate::engine;
use crate::error::{AppError, Res};
use crate::graph::{self, Op};
use crate::provider::{ChatRequest, ErrKind, ToolCall};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashSet;

pub const MAX_ROUNDS: usize = 4;

const SYSTEM: &str = "You are the Seed Studio canvas assistant. You change the node graph of the open project only through tools, and you reply briefly in the user's language.\n\
Node types: prompt (data.text, outputs text) · imageAsset (data.assetId, outputs image) · seedText (text input `prompt`, outputs text) · \
seedance (video node: inputs `prompt`, `first`, `last`, `refs`; data.settings pass/resolution/ratio/duration as strings; only the user runs it) · seedream (one text input `prompt`, ordered image inputs `refs`, data.modelKey and data.settings, outputs image) · preview (input `image`) · audio (data.audioId, start, end; outputs an audio clip for the video node’s `audio` input) · seedream and seedance may carry their own data.prompt instead of a connected prompt node · describe (one image input `image`, data.instructions, outputs text that describes the image).\n\
Rules: use apply_canvas_patch with the current expectedRevision and apply everything requested in one batch; give new nodes short latin ids so you can connect them in the same batch, \
and positions that do not overlap (about 380 apart horizontally, 260 vertically). Each generation branch is a prompt node, a seedream node and a preview node, with the selected reference image connected to `refs`. \
Do not change settings you were not asked to change. Call get_model_options before setting an option whose values you do not know. You cannot generate images: use plan_run to propose a plan; only the user presses Run. \
If a batch is rejected, read the reason and correct it; if the reason is a revision conflict, call read_canvas first.";

pub fn tool_defs(model_keys: &[String]) -> Vec<Value> {
    let pos = json!({"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"], "additionalProperties": false});
    let f = |name: &str, description: &str, parameters: Value| json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}});
    vec![
        f("read_canvas", "Reads the nodes and links of the open canvas and its current revision.",
          json!({"type": "object", "properties": {"nodeIds": {"type": "array", "items": {"type": "string"}, "description": "Optional: specific nodes"}}, "additionalProperties": false})),
        f("get_model_options", "Returns the allowed fields and values for a catalog model.",
          json!({"type": "object", "properties": {"modelKey": {"type": "string", "enum": model_keys}}, "required": ["modelKey"], "additionalProperties": false})),
        f("apply_canvas_patch", "Applies a validated batch of edits to the graph. Either all of it is applied or none.",
          json!({"type": "object", "required": ["expectedRevision", "operations"], "additionalProperties": false, "properties": {
              "expectedRevision": {"type": "integer"},
              "operations": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["op"], "additionalProperties": false, "properties": {
                  "op": {"type": "string", "enum": ["addNode", "setNodeData", "connect", "disconnect", "moveNode", "removeNode"]},
                  "id": {"type": "string", "description": "addNode: id of the new node"},
                  "type": {"type": "string", "enum": graph::NODE_KINDS},
                  "position": pos,
                  "data": {"type": "object", "description": "addNode: title, text, assetId, modelKey, settings"},
                  "nodeId": {"type": "string"},
                  "changes": {"type": "object", "description": "setNodeData: changed fields only; settings are merged"},
                  "source": {"type": "string"}, "sourceHandle": {"type": "string"},
                  "target": {"type": "string"}, "targetHandle": {"type": "string", "enum": ["prompt", "refs", "image", "first", "last", "audio"]},
                  "order": {"type": "integer"}, "edgeId": {"type": "string"}
              }}}
          }})),
        f("plan_run", "Computes a local run plan for the given nodes and shows it to the user. Generates nothing.",
          json!({"type": "object", "properties": {"nodeIds": {"type": "array", "items": {"type": "string"}, "minItems": 1}}, "required": ["nodeIds"], "additionalProperties": false})),
    ]
}

struct Turn<'a> {
    core: &'a Core,
    project_id: &'a str,
    thread_id: &'a str,
    turn_id: String,
    actions: Vec<String>,
    notes: Vec<String>,
    plan: Option<Value>,
    pending: Option<Value>,
    changed: bool,
}

impl Turn<'_> {
    fn exec(&mut self, tc: &ToolCall) -> Value {
        let args: Value = match serde_json::from_str(&tc.arguments) {
            Ok(v @ Value::Object(_)) => v,
            _ => return json!({"ok": false, "error": "Tool arguments are not a valid JSON object"}),
        };
        match tc.name.as_str() {
            "read_canvas" => {
                let only: Option<Vec<String>> = args.get("nodeIds").and_then(|v| serde_json::from_value(v.clone()).ok());
                match self.core.db(|c| Core::load_canvas(c, self.project_id)) {
                    Ok(row) => json!({"ok": true, "revision": row.revision, "canvas": graph::summarize(&row.graph, only.as_deref())}),
                    Err(e) => json!({"ok": false, "error": e.message}),
                }
            }
            "get_model_options" => match args["modelKey"].as_str().and_then(|k| self.core.catalog.options(k)) {
                Some(o) => json!({"ok": true, "options": o}),
                None => json!({"ok": false, "error": "Model key is not in the catalog"}),
            },
            "apply_canvas_patch" => {
                if args.as_object().unwrap().keys().any(|k| k != "expectedRevision" && k != "operations") {
                    return json!({"ok": false, "error": "Unknown fields; only expectedRevision and operations are allowed"});
                }
                let Some(rev) = args["expectedRevision"].as_i64() else {
                    return json!({"ok": false, "error": "expectedRevision is required"});
                };
                let ops: Vec<Op> = match serde_json::from_value(args["operations"].clone()) {
                    Ok(o) => o,
                    Err(e) => return json!({"ok": false, "error": format!("Invalid operation: {e}")}),
                };
                // Removing nodes is shown to the user first; it is applied only from the UI.
                let removals: Vec<&str> = ops.iter().filter_map(|o| if let Op::RemoveNode { node_id } = o { Some(node_id.as_str()) } else { None }).collect();
                if !removals.is_empty() {
                    let row = match self.core.db(|c| Core::load_canvas(c, self.project_id)) {
                        Ok(r) => r,
                        Err(e) => return json!({"ok": false, "error": e.message}),
                    };
                    let titles: Vec<String> = removals.iter().filter_map(|id| row.graph.node(id))
                        .map(|n| n.data.get("title").and_then(|v| v.as_str()).unwrap_or(&n.id).to_string()).collect();
                    let edges = row.graph.edges.iter().filter(|e| removals.contains(&e.source.as_str()) || removals.contains(&e.target.as_str())).count();
                    self.pending = Some(json!({"expectedRevision": rev, "operations": args["operations"], "titles": titles, "edges": edges}));
                    return json!({"ok": false, "pendingUserConfirmation": true,
                        "note": "The batch removes existing nodes; its effect was shown to the user and it is applied only if they approve in the UI. Do not send it again."});
                }
                let actor = Actor::Agent { turn_id: self.turn_id.clone(), tool_call_id: tc.id.clone(), tool: tc.name.clone(), thread_id: Some(self.thread_id.to_string()) };
                match self.core.apply_patch(self.project_id, Some(rev), &ops, &actor, "Assistant edit") {
                    Ok(r) => {
                        self.changed = true;
                        self.actions.push(r.outcome.summary());
                        self.notes.extend(r.outcome.notes.clone());
                        json!({"ok": true, "revision": r.revision, "createdIds": r.outcome.created_ids, "notes": r.outcome.notes})
                    }
                    Err(e) if e.code == "duplicate_tool_call" => json!({"ok": true, "duplicate": true, "note": e.message}),
                    Err(e) => json!({"ok": false, "error": e.message, "detail": e.detail, "applied": false}),
                }
            }
            "plan_run" => {
                let ids: Vec<String> = serde_json::from_value(args["nodeIds"].clone()).unwrap_or_default();
                if ids.is_empty() {
                    return json!({"ok": false, "error": "nodeIds is required"});
                }
                match engine::build_plan(self.core, self.project_id, &ids, false) {
                    Ok(plan) => {
                        let brief = json!({"ok": true, "planId": plan.id, "newImages": plan.new_images, "newTextCalls": plan.new_texts,
                            "reusedFromDisk": plan.reused, "blocked": plan.items.iter().filter(|i| i.action == "blocked")
                                .map(|i| json!({"nodeId": i.node_id, "reason": i.reason})).collect::<Vec<_>>(),
                            "note": "The plan was shown to the user; only their Run button starts it."});
                        self.plan = serde_json::to_value(&plan).ok();
                        brief
                    }
                    Err(e) => json!({"ok": false, "error": e.message}),
                }
            }
            other => json!({"ok": false, "error": format!("Unknown tool: {other}")}),
        }
    }
}

fn save_message(core: &Core, thread_id: &str, role: &str, content: &str, status: &str, model: Option<&str>, provider: Option<&str>, meta: Value) -> Res<String> {
    let id = new_id();
    core.db(|c| {
        c.execute("INSERT INTO messages(id,thread_id,role,content,status,model_id,provider,meta_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![id, thread_id, role, content, status, model, provider, meta.to_string(), now_ms()])?;
        c.execute("UPDATE threads SET updated_at=?1 WHERE id=?2", params![now_ms(), thread_id])?;
        Ok(())
    })?;
    Ok(id)
}

/// One user command to the canvas assistant: up to four tool rounds.
pub async fn send(core: &Core, project_id: &str, thread_id: &str, text: &str, selection: &[String]) -> Res<Value> {
    let (instructions, history): (String, Vec<(String, String)>) = core.db(|c| {
        let owner: String = c.query_row("SELECT project_id FROM threads WHERE id=?1", [thread_id], |r| r.get(0))
            .map_err(|_| AppError::new("not_found", "Conversation not found"))?;
        if owner != project_id {
            return Err(AppError::new("forbidden", "The conversation does not belong to this project"));
        }
        let ins: String = c.query_row("SELECT instructions FROM projects WHERE id=?1", [project_id], |r| r.get(0))?;
        let mut st = c.prepare("SELECT role, content FROM messages WHERE thread_id=?1 AND content!='' AND role IN ('user','assistant') ORDER BY created_at DESC, rowid DESC LIMIT 8")?;
        let mut rows: Vec<(String, String)> = st.query_map([thread_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        rows.reverse();
        Ok((ins, rows))
    })?;
    save_message(core, thread_id, "user", text, "complete", None, None, json!({"selection": selection}))?;
    core.emit("messages-changed", json!({"threadId": thread_id}));

    let fail = |e: AppError| -> AppError {
        let _ = save_message(core, thread_id, "assistant", &e.message, "error", None, None, json!({"kind": "agent"}));
        core.emit("messages-changed", json!({"threadId": thread_id}));
        e
    };
    let set = core.providers().map_err(&fail)?;
    let model_key = core.chat_model_key();
    let model_id = core.resolve_model_id(&model_key).map_err(&fail)?;
    let row = core.db(|c| Core::load_canvas(c, project_id)).map_err(&fail)?;
    let image_keys: Vec<String> = core.catalog.models.iter().filter(|m| m.kind == "image").map(|m| m.key.clone()).collect();
    let all_keys: Vec<String> = core.catalog.models.iter().map(|m| m.key.clone()).collect();
    let context = json!({
        "revision": row.revision,
        "selection": selection,
        "canvas": graph::summarize(&row.graph, None),
        "imageModels": image_keys,
        "defaultImageModel": core.image_model_key(),
    });
    let mut messages = vec![json!({"role": "system", "content": SYSTEM})];
    if !instructions.trim().is_empty() {
        messages.push(json!({"role": "system", "content": format!("Project instructions:\n{instructions}")}));
    }
    messages.push(json!({"role": "system", "content": format!("CANVAS_CONTEXT\n{context}")}));
    for (role, content) in history {
        messages.push(json!({"role": role, "content": content}));
    }
    messages.push(json!({"role": "user", "content": text}));

    let mut turn = Turn { core, project_id, thread_id, turn_id: new_id(), actions: vec![], notes: vec![], plan: None, pending: None, changed: false };
    let tools = tool_defs(&all_keys);
    let mut seen: HashSet<String> = HashSet::new();
    let mut final_text = String::new();
    let mut answered_by = model_id.clone();
    let mut exhausted = true;

    for _ in 0..MAX_ROUNDS {
        let req = ChatRequest { model: model_id.clone(), messages: messages.clone(), tools: Some(tools.clone()), thinking: Some("disabled".into()), cancel: Default::default() };
        let reply = match set.chat.chat_tools(req).await {
            Ok(r) => r,
            Err(e) => {
                if e.kind == ErrKind::Activation {
                    core.mark_needs_activation(&set, &model_id);
                }
                if turn.changed {
                    core.emit("canvas-changed", json!({"projectId": project_id}));
                }
                let mut msg = e.display(&set.region, &model_id);
                if e.kind == ErrKind::BadRequest {
                    msg.push_str(" If this model does not support tool calling, the canvas assistant cannot work with it; normal chat and manual editing still do.");
                }
                if !turn.actions.is_empty() {
                    msg.push_str(&format!(" Done before the error: {}.", turn.actions.join("; ")));
                }
                return Err(fail(AppError::new("agent_failed", msg)));
            }
        };
        answered_by = reply.model.clone();
        core.mark_tried(&set, &model_id);
        if reply.tool_calls.is_empty() {
            final_text = reply.content;
            exhausted = false;
            break;
        }
        final_text = reply.content.clone();
        messages.push(reply.raw_message.clone());
        for tc in &reply.tool_calls {
            let result = if seen.insert(tc.id.clone()) {
                turn.exec(tc)
            } else {
                json!({"ok": true, "duplicate": true, "note": "Duplicate call; ignored"})
            };
            messages.push(json!({"role": "tool", "tool_call_id": tc.id, "content": result.to_string()}));
        }
    }
    if exhausted {
        let done = if turn.actions.is_empty() { "no edit was applied".to_string() } else { turn.actions.join("; ") };
        final_text = format!("The assistant reached the limit of {MAX_ROUNDS} tool rounds before finishing. Actually done: {done}.");
    }
    if final_text.trim().is_empty() {
        final_text = if turn.actions.is_empty() { "The assistant made no edit.".into() } else { "Done.".into() };
    }
    let meta = json!({
        "kind": "agent", "turnId": turn.turn_id, "actions": turn.actions, "notes": turn.notes,
        "plan": turn.plan, "pendingPatch": turn.pending, "changed": turn.changed,
    });
    let id = save_message(core, thread_id, "assistant", &final_text, "complete", Some(&answered_by), Some(&set.name), meta.clone())?;
    if turn.changed {
        core.emit("canvas-changed", json!({"projectId": project_id}));
    }
    core.emit("messages-changed", json!({"threadId": thread_id}));
    Ok(json!({"messageId": id, "text": final_text, "model": answered_by, "provider": set.name, "meta": meta}))
}
