//! Canvas graph model and the single validator used for every change,
//! whether it comes from the user's hand or from the agent's tool call.
use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub position: Pos,
    #[serde(default)]
    pub data: Map<String, Value>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Default)]
pub struct EdgeData {
    #[serde(default)]
    pub order: i64,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub source: String,
    pub source_handle: String,
    pub target: String,
    pub target_handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<EdgeData>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Graph {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum Op {
    #[serde(rename_all = "camelCase")]
    AddNode {
        #[serde(default)]
        id: Option<String>,
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        position: Option<Pos>,
        #[serde(default)]
        data: Map<String, Value>,
    },
    #[serde(rename_all = "camelCase")]
    SetNodeData { node_id: String, changes: Map<String, Value> },
    #[serde(rename_all = "camelCase")]
    Connect {
        source: String,
        #[serde(default)]
        source_handle: Option<String>,
        target: String,
        target_handle: String,
        #[serde(default)]
        order: Option<i64>,
        #[serde(default)]
        id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Disconnect {
        #[serde(default)]
        edge_id: Option<String>,
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        target_handle: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    MoveNode { node_id: String, position: Pos },
    #[serde(rename_all = "camelCase")]
    RemoveNode { node_id: String },
}

pub struct Ctx<'a> {
    pub catalog: &'a Catalog,
    /// Asset ids that belong to the open project. Anything else is rejected.
    pub asset_ids: &'a HashSet<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub created_ids: Vec<String>,
    pub removed_ids: Vec<String>,
    pub notes: Vec<String>,
    pub added: usize,
    pub connected: usize,
    pub disconnected: usize,
    pub edited: usize,
    pub moved: usize,
    pub removed: usize,
}

impl Outcome {
    /// Arabic one-line summary of what was actually saved.
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        let n = |c: usize, one: &str, two: &str, many: &str| match c {
            1 => one.to_string(),
            2 => two.to_string(),
            c => format!("{c} {many}"),
        };
        if self.added > 0 { parts.push(format!("added {}", n(self.added, "1 node", "2 nodes", "nodes"))); }
        if self.connected > 0 { parts.push(format!("connected {}", n(self.connected, "1 link", "2 links", "links"))); }
        if self.edited > 0 { parts.push(format!("edited {}", n(self.edited, "1 node", "2 nodes", "nodes"))); }
        if self.disconnected > 0 { parts.push(format!("disconnected {}", n(self.disconnected, "1 link", "2 links", "links"))); }
        if self.moved > 0 { parts.push(format!("moved {}", n(self.moved, "1 node", "2 nodes", "nodes"))); }
        if self.removed > 0 { parts.push(format!("removed {}", n(self.removed, "1 node", "2 nodes", "nodes"))); }
        if parts.is_empty() { "No change".into() } else {
            let s = parts.join(", ");
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or(s)
        }
    }
}

#[derive(Debug, Clone)]
pub struct PatchError {
    pub op_index: usize,
    pub message: String,
}

pub const NODE_KINDS: [&str; 10] = ["prompt", "imageAsset", "seedText", "seedream", "preview", "seedance", "describe", "audio", "note", "group"];
/// Colours a note or a group frame can have (organisation only; nothing is sent).
pub const COLORS: [&str; 6] = ["gray", "green", "amber", "red", "blue", "purple"];

pub fn is_executable(kind: &str) -> bool {
    kind == "seedream" || kind == "seedText" || kind == "describe"
}

/// (handle, port type) a node kind emits.
pub fn out_port(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "prompt" | "seedText" | "describe" => Some(("text", "text")),
        "imageAsset" | "seedream" => Some(("image", "image")),
        "audio" => Some(("audio", "audio")),
        _ => None,
    }
}

/// (port type, max incoming) for an input handle.
pub fn in_port(node: &Node, handle: &str, catalog: &Catalog) -> Option<(&'static str, usize)> {
    match (node.kind.as_str(), handle) {
        ("seedText", "prompt") | ("seedream", "prompt") => Some(("text", 1)),
        ("seedream", "refs") => {
            let key = node.data.get("modelKey").and_then(|v| v.as_str()).unwrap_or("");
            Some(("image", catalog.max_refs(key)))
        }
        ("preview", "image") | ("describe", "image") => Some(("image", 1)),
        // Video node: one prompt, optional first/last frame, or ordered reference images.
        ("seedance", "prompt") => Some(("text", 1)),
        ("seedance", "audio") => Some(("audio", 1)),
        ("seedance", "first") | ("seedance", "last") => Some(("image", 1)),
        ("seedance", "refs") => Some(("image", catalog.model(node.data.get("modelKey").and_then(|v| v.as_str()).unwrap_or(crate::video::VIDEO_MODEL)).and_then(|m| m.controls["maxReferenceImages"].as_u64()).unwrap_or(30) as usize)),
        _ => None,
    }
}

impl Graph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }
    pub fn incoming<'a>(&'a self, target: &'a str, handle: &'a str) -> Vec<&'a Edge> {
        let mut v: Vec<&Edge> = self.edges.iter().filter(|e| e.target == target && e.target_handle == handle).collect();
        v.sort_by_key(|e| e.data.as_ref().map(|d| d.order).unwrap_or(0));
        v
    }
    fn reaches(&self, from: &str, to: &str) -> bool {
        let mut stack = vec![from.to_string()];
        let mut seen = HashSet::new();
        while let Some(n) = stack.pop() {
            if n == to { return true; }
            if !seen.insert(n.clone()) { continue; }
            for e in self.edges.iter().filter(|e| e.source == n) {
                stack.push(e.target.clone());
            }
        }
        false
    }
}

fn check_str(data: &Map<String, Value>, key: &str, max: usize) -> Result<(), String> {
    match data.get(key) {
        None => Ok(()),
        Some(Value::String(s)) if s.chars().count() <= max => Ok(()),
        Some(Value::String(_)) => Err(format!("Field {key} is longer than {max} characters")),
        Some(_) => Err(format!("Field {key} must be text")),
    }
}

/// Closed schema for each node kind's business data.
fn validate_data(kind: &str, data: &Map<String, Value>, ctx: &Ctx) -> Result<(), String> {
    let allowed: &[&str] = match kind {
        "prompt" => &["title", "text"],
        "imageAsset" => &["title", "assetId", "library"],
        "seedText" | "describe" => &["title", "instructions", "modelKey"],
        "seedream" => &["title", "modelKey", "settings", "prompt", "camera"],
        "audio" => &["title", "audioId", "start", "end"],
        "preview" => &["title"],
        "seedance" => &["title", "modelKey", "settings", "prompt"],
        // Organisation only: a coloured sticky note, and a coloured frame drawn behind other nodes.
        "note" => &["title", "text", "color"],
        "group" => &["title", "color", "width", "height"],
        _ => return Err(format!("Unknown node type: {kind}")),
    };
    // A colour tag is allowed on every node: it only marks the node for its owner and is never sent.
    if let Some(k) = data.keys().find(|k| !allowed.contains(&k.as_str()) && k.as_str() != "color") {
        return Err(format!("Field {k} is not allowed on a {kind} node"));
    }
    check_str(data, "title", 120)?;
    check_str(data, "library", 120)?;
    check_str(data, "camera", 2000)?;
    check_str(data, "text", 8000)?;
    check_str(data, "instructions", 4000)?;
    if let Some(c) = data.get("color") {
        if !c.as_str().map(|c| COLORS.contains(&c)).unwrap_or(false) { return Err(format!("color must be one of: {}", COLORS.join(", "))); }
    }
    for k in ["width", "height"] {
        if let Some(v) = data.get(k) {
            if !v.as_f64().map(|n| (120.0..=40_000.0).contains(&n)).unwrap_or(false) { return Err(format!("{k} must be a number between 120 and 40000")); }
        }
    }
    match kind {
        "imageAsset" => match data.get("assetId") {
            None | Some(Value::Null) => {}
            Some(Value::String(id)) if ctx.asset_ids.contains(id) => {}
            Some(Value::String(id)) => return Err(format!("Image {id} does not belong to this project")),
            Some(_) => return Err("assetId must be an image id".into()),
        },
        "audio" => {
            for k in ["start", "end"] {
                if let Some(v) = data.get(k) {
                    let ok = v.as_str().map(|s| s.is_empty() || s.parse::<f64>().map(|x| (0.0..=36_000.0).contains(&x)).unwrap_or(false)).unwrap_or(false);
                    if !ok { return Err(format!("{k} must be a number of seconds")); }
                }
            }
            if let Some(v) = data.get("audioId") {
                if !v.is_string() && !v.is_null() { return Err("audioId must be an audio id".into()); }
            }
        }
        "seedText" | "describe" => {
            if let Some(v) = data.get("modelKey") {
                let ok = v.as_str().and_then(|k| ctx.catalog.model(k)).map(|m| m.kind == "chat").unwrap_or(false);
                if !ok { return Err("modelKey must be a chat model from the catalog".into()); }
            }
        }
        "seedance" => {
            let key = match data.get("modelKey") {
                None => crate::video::VIDEO_MODEL,
                Some(v) => v.as_str().ok_or_else(|| "modelKey must be a video model from the catalog".to_string())?,
            };
            let model = ctx.catalog.model(key).filter(|m| m.kind == "video")
                .ok_or_else(|| "modelKey must be a video model from the catalog".to_string())?;
            let controls = &model.controls;
            let list = |k: &str| -> Vec<String> { controls[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
            if let Some(v) = data.get("settings") {
                let s = v.as_object().ok_or_else(|| "settings must be an object".to_string())?;
                for (k, v) in s {
                    let val = v.as_str().ok_or_else(|| format!("The value of {k} must be a string"))?;
                    let ok = match k.as_str() {
                        "pass" => val == "final" || (val == "draft" && controls["draftMode"] == true),
                        "resolution" => list("resolutions").iter().any(|x| x == val),
                        "ratio" => list("ratios").iter().any(|x| x == val),
                        "duration" => val.parse::<i64>().map(|d| d == -1 || (controls["duration"]["min"].as_i64().unwrap_or(4)..=controls["duration"]["max"].as_i64().unwrap_or(30)).contains(&d)).unwrap_or(false),
                        "voice" | "sound" => val == "on" || val == "off",
                        "voiceStart" | "voiceEnd" => val.is_empty() || val.parse::<f64>().map(|x| (0.0..=36_000.0).contains(&x)).unwrap_or(false),
                        _ => return Err(format!("Field {k} is not known for the video node")),
                    };
                    if !ok {
                        return Err(format!("{val} is not allowed for {k} on the video node"));
                    }
                }
            }
        }
        "seedream" => {
            let key = data.get("modelKey").and_then(|v| v.as_str())
                .ok_or_else(|| "A seedream node needs a modelKey".to_string())?;
            let empty = Map::new();
            let settings = match data.get("settings") {
                None => &empty,
                Some(Value::Object(m)) => m,
                Some(_) => return Err("settings must be an object".into()),
            };
            ctx.catalog.validate_settings(key, settings)?;
        }
        _ => {}
    }
    Ok(())
}

fn short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

fn apply_one(g: &mut Graph, op: &Op, ctx: &Ctx, out: &mut Outcome) -> Result<(), String> {
    match op {
        Op::AddNode { id, kind, position, data } => {
            if !NODE_KINDS.contains(&kind.as_str()) {
                return Err(format!("Unknown node type: {kind}"));
            }
            let id = id.clone().unwrap_or_else(|| format!("{kind}-{}", short()));
            if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(format!("Invalid node id: {id}"));
            }
            if g.node(&id).is_some() {
                return Err(format!("The id {id} is already used"));
            }
            let mut data = data.clone();
            if kind == "seedream" {
                if !data.contains_key("modelKey") {
                    data.insert("modelKey".into(), json!(ctx.catalog.suggested("image")));
                }
                let key = data["modelKey"].as_str().unwrap_or("").to_string();
                if ctx.catalog.model(&key).is_some() {
                    let mut s = ctx.catalog.default_settings(&key);
                    if let Some(Value::Object(given)) = data.get("settings") {
                        for (k, v) in given { s.insert(k.clone(), v.clone()); }
                    }
                    data.insert("settings".into(), Value::Object(s));
                }
            }
            if kind == "seedance" {
                let key = data.get("modelKey").and_then(|v| v.as_str()).unwrap_or(crate::video::VIDEO_MODEL);
                let supports_draft = ctx.catalog.model(key).is_some_and(|m| m.controls["draftMode"] == true);
                let mut s: Map<String, Value> = [("pass", if supports_draft { "draft" } else { "final" }), ("resolution", if supports_draft { "1080p" } else { "720p" }), ("ratio", "16:9"), ("duration", "6"), ("voice", "off"), ("sound", "on")]
                    .iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
                if let Some(Value::Object(given)) = data.get("settings") {
                    for (k, v) in given { s.insert(k.clone(), v.clone()); }
                }
                data.insert("settings".into(), Value::Object(s));
            }
            validate_data(kind, &data, ctx)?;
            let position = position.unwrap_or_else(|| {
                let y = g.nodes.iter().map(|n| n.position.y).fold(-180.0, f64::max) + 180.0;
                Pos { x: 0.0, y }
            });
            if !position.x.is_finite() || !position.y.is_finite() {
                return Err("Invalid position".into());
            }
            g.nodes.push(Node { id: id.clone(), kind: kind.clone(), position, data });
            out.created_ids.push(id);
            out.added += 1;
        }
        Op::SetNodeData { node_id, changes } => {
            let node = g.node(node_id).ok_or_else(|| format!("Node {node_id} does not exist"))?.clone();
            let mut data = node.data.clone();
            let model_changed = changes.get("modelKey").map(|v| Some(v) != data.get("modelKey")).unwrap_or(false);
            for (k, v) in changes {
                if k == "settings" {
                    let patch = v.as_object().ok_or_else(|| "settings must be an object".to_string())?;
                    let mut s = data.get("settings").and_then(|x| x.as_object()).cloned().unwrap_or_default();
                    for (sk, sv) in patch {
                        if sv.is_null() { s.remove(sk); } else { s.insert(sk.clone(), sv.clone()); }
                    }
                    data.insert("settings".into(), Value::Object(s));
                } else if v.is_null() {
                    data.remove(k);
                } else {
                    data.insert(k.clone(), v.clone());
                }
            }
            if node.kind == "seedream" && model_changed && !changes.contains_key("settings") {
                let key = data.get("modelKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if ctx.catalog.model(&key).map(|m| m.kind == "image").unwrap_or(false) {
                    let old = data.get("settings").and_then(|x| x.as_object()).cloned().unwrap_or_default();
                    let (s, notes) = ctx.catalog.normalize_settings(&key, &old);
                    data.insert("settings".into(), Value::Object(s));
                    out.notes.extend(notes);
                    let refs = g.incoming(node_id, "refs").len();
                    if refs > ctx.catalog.max_refs(&key) {
                        return Err(format!("The new model accepts only {} references and this node has {refs}", ctx.catalog.max_refs(&key)));
                    }
                }
            }
            if node.kind == "seedance" && model_changed {
                let key = data.get("modelKey").and_then(|v| v.as_str()).unwrap_or(crate::video::VIDEO_MODEL);
                if let Some(model) = ctx.catalog.model(key).filter(|m| m.kind == "video") {
                    let max_refs = model.controls["maxReferenceImages"].as_u64().unwrap_or(30) as usize;
                    if g.incoming(node_id, "refs").len() > max_refs {
                        return Err(format!("{} accepts at most {max_refs} reference images. Disconnect extra references before switching models.", model.label));
                    }
                    if !changes.contains_key("settings") {
                        let mut settings = data.get("settings").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                        if model.controls["draftMode"] != true { settings.insert("pass".into(), json!("final")); }
                        let resolutions = model.controls["resolutions"].as_array().cloned().unwrap_or_default();
                        if !resolutions.contains(settings.get("resolution").unwrap_or(&Value::Null)) {
                            settings.insert("resolution".into(), json!("720p"));
                        }
                        data.insert("settings".into(), json!(settings));
                    }
                }
            }
            validate_data(&node.kind, &data, ctx)?;
            g.node_mut(node_id).unwrap().data = data;
            out.edited += 1;
        }
        Op::Connect { source, source_handle, target, target_handle, order, id } => {
            if source == target {
                return Err("A node cannot connect to itself".into());
            }
            let s = g.node(source).ok_or_else(|| format!("Node {source} does not exist"))?;
            let t = g.node(target).ok_or_else(|| format!("Node {target} does not exist"))?;
            let (sh, sty) = out_port(&s.kind).ok_or_else(|| format!("Node {source} has no output port"))?;
            if let Some(h) = source_handle {
                if h != sh { return Err(format!("Output port {h} does not exist on {source}")); }
            }
            let (tty, max) = in_port(t, target_handle, ctx.catalog)
                .ok_or_else(|| format!("Input port {target_handle} does not exist on {target}"))?;
            if sty != tty {
                let name = |x: &str| if x == "text" { "a text" } else { "an image" };
                return Err(format!("Cannot connect {} output to {} input", name(sty), name(tty)));
            }
            if g.edges.iter().any(|e| e.source == *source && e.target == *target && e.target_handle == *target_handle) {
                return Err("This link already exists".into());
            }
            let existing = g.incoming(target, target_handle).len();
            if existing >= max {
                return Err(if max == 1 {
                    format!("The {target_handle} port on {target} accepts a single input")
                } else {
                    format!("The {target_handle} port accepts at most {max} references")
                });
            }
            if g.reaches(target, source) {
                return Err("This link would create a loop".into());
            }
            let eid = id.clone().unwrap_or_else(|| format!("e-{}", short()));
            if g.edges.iter().any(|e| e.id == eid) {
                return Err(format!("The link id {eid} is already used"));
            }
            let data = if target_handle == "refs" {
                Some(EdgeData { order: order.unwrap_or(existing as i64) })
            } else {
                None
            };
            g.edges.push(Edge { id: eid, source: source.clone(), source_handle: sh.into(), target: target.clone(), target_handle: target_handle.clone(), data });
            out.connected += 1;
        }
        Op::Disconnect { edge_id, source, target, target_handle } => {
            let before = g.edges.len();
            if let Some(id) = edge_id {
                g.edges.retain(|e| e.id != *id);
            } else if let (Some(s), Some(t)) = (source, target) {
                g.edges.retain(|e| !(e.source == *s && e.target == *t && target_handle.as_ref().map(|h| *h == e.target_handle).unwrap_or(true)));
            } else {
                return Err("disconnect needs edgeId, or source and target".into());
            }
            if g.edges.len() == before {
                return Err("The link does not exist".into());
            }
            out.disconnected += before - g.edges.len();
        }
        Op::MoveNode { node_id, position } => {
            if !position.x.is_finite() || !position.y.is_finite() {
                return Err("Invalid position".into());
            }
            g.node_mut(node_id).ok_or_else(|| format!("Node {node_id} does not exist"))?.position = *position;
            out.moved += 1;
        }
        Op::RemoveNode { node_id } => {
            if g.node(node_id).is_none() {
                return Err(format!("Node {node_id} does not exist"));
            }
            g.nodes.retain(|n| n.id != *node_id);
            g.edges.retain(|e| e.source != *node_id && e.target != *node_id);
            out.removed_ids.push(node_id.clone());
            out.removed += 1;
        }
    }
    Ok(())
}

/// All-or-nothing: returns a new graph, or the first failing operation.
pub fn apply_ops(graph: &Graph, ops: &[Op], ctx: &Ctx) -> Result<(Graph, Outcome), PatchError> {
    if ops.is_empty() {
        return Err(PatchError { op_index: 0, message: "No operations".into() });
    }
    if ops.len() > 200 {
        return Err(PatchError { op_index: 0, message: "Too many operations in one batch".into() });
    }
    let mut g = graph.clone();
    let mut out = Outcome::default();
    for (i, op) in ops.iter().enumerate() {
        apply_one(&mut g, op, ctx, &mut out).map_err(|message| PatchError { op_index: i, message })?;
    }
    if g.nodes.len() > 500 {
        return Err(PatchError { op_index: 0, message: "The canvas exceeds 500 nodes".into() });
    }
    Ok((g, out))
}

/// Rebuild a graph through the validator (used by import).
pub fn validated(graph: &Graph, ctx: &Ctx) -> Result<Graph, PatchError> {
    let mut ops: Vec<Op> = graph.nodes.iter().map(|n| Op::AddNode {
        id: Some(n.id.clone()), kind: n.kind.clone(), position: Some(n.position), data: n.data.clone(),
    }).collect();
    ops.extend(graph.edges.iter().map(|e| Op::Connect {
        source: e.source.clone(), source_handle: Some(e.source_handle.clone()), target: e.target.clone(),
        target_handle: e.target_handle.clone(), order: e.data.as_ref().map(|d| d.order), id: Some(e.id.clone()),
    }));
    if ops.is_empty() {
        return Ok(Graph::default());
    }
    apply_ops(&Graph::default(), &ops, ctx).map(|(g, _)| g)
}

/// Compact description for the agent's context and `read_canvas`.
pub fn summarize(graph: &Graph, only: Option<&[String]>) -> Value {
    let keep = |id: &str| only.map(|o| o.iter().any(|x| x == id)).unwrap_or(true);
    let nodes: Vec<Value> = graph.nodes.iter().filter(|n| keep(&n.id)).map(|n| {
        let mut data = n.data.clone();
        if let Some(Value::String(t)) = data.get("text") {
            if t.chars().count() > 400 {
                let cut: String = t.chars().take(400).collect();
                data.insert("text".into(), json!(format!("{cut}…")));
            }
        }
        json!({"id": n.id, "type": n.kind, "position": n.position, "data": data})
    }).collect();
    let edges: Vec<Value> = graph.edges.iter().filter(|e| keep(&e.source) || keep(&e.target)).map(|e| {
        json!({"id": e.id, "source": e.source, "target": e.target, "targetHandle": e.target_handle,
               "order": e.data.as_ref().map(|d| d.order)})
    }).collect();
    json!({"nodes": nodes, "edges": edges})
}
