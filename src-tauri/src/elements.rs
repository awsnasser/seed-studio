//! Project elements: characters, environments and props the owner locks once (a name,
//! one image, a description) and calls into any prompt as `@Name`. A mention adds the
//! element's image as a reference and puts its description into the prompt.
use crate::core::{new_id, now_ms, Core};
use crate::error::{AppError, Res};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub const KINDS: [&str; 3] = ["character", "environment", "prop"];

/// The name as it is typed after `@`: letters and digits only, so `Red Tent` is `@RedTent`.
pub fn handle(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect()
}

#[derive(Clone, Debug)]
pub struct Element {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub handle: String,
    pub asset_id: Option<String>,
    pub description: String,
}

fn rows(core: &Core, project_id: &str) -> Res<Vec<Element>> {
    core.db(|c| {
        let mut st = c.prepare("SELECT id, kind, name, asset_id, description FROM elements WHERE project_id=?1 ORDER BY kind, name COLLATE NOCASE")?;
        let list = st.query_map([project_id], |r| {
            let name: String = r.get(2)?;
            Ok(Element { id: r.get(0)?, kind: r.get(1)?, handle: handle(&name), name, asset_id: r.get(3)?, description: r.get(4)? })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(list)
    })
}

pub fn list(core: &Core, project_id: &str) -> Res<Value> {
    Ok(json!(rows(core, project_id)?.iter().map(|e| json!({
        "id": e.id, "kind": e.kind, "name": e.name, "handle": e.handle, "assetId": e.asset_id, "description": e.description,
    })).collect::<Vec<_>>()))
}

pub fn save(core: &Core, args: &Value) -> Res<Value> {
    let bad = |m: &str| AppError::new("bad_element", m);
    let project_id = args["projectId"].as_str().ok_or_else(|| bad("projectId is required"))?;
    let kind = args["kind"].as_str().filter(|k| KINDS.contains(k)).ok_or_else(|| bad("Choose character, environment or prop"))?;
    let name = args["name"].as_str().unwrap_or("").trim().to_string();
    let h = handle(&name);
    if h.is_empty() || name.chars().count() > 40 { return Err(bad("Give the element a name of up to 40 characters")); }
    // Seedance's own tags must stay unambiguous.
    if ["image", "audio", "video"].iter().any(|t| h.to_lowercase().starts_with(t)) { return Err(bad("A name cannot start with Image, Audio or Video: Seedance uses @Image 1, @Audio 1 and @Video 1 itself")); }
    let description = args["description"].as_str().unwrap_or("").trim().to_string();
    if description.chars().count() > 1200 { return Err(bad("Keep the description under 1200 characters")); }
    let asset_id = args["assetId"].as_str().filter(|a| !a.is_empty());
    if let Some(a) = asset_id {
        if !core.db(|c| Core::asset_ids(c, project_id))?.contains(a) { return Err(bad("That image does not belong to this project")); }
    }
    let id = args["id"].as_str().map(String::from);
    let clash = rows(core, project_id)?.into_iter().any(|e| e.handle.to_lowercase() == h.to_lowercase() && Some(&e.id) != id.as_ref());
    if clash { return Err(AppError::new("bad_element", format!("This project already has an element called @{h}"))); }
    core.db(|c| {
        match &id {
            Some(id) => {
                let n = c.execute("UPDATE elements SET kind=?1, name=?2, asset_id=?3, description=?4 WHERE id=?5 AND project_id=?6", params![kind, name, asset_id, description, id, project_id])?;
                if n == 0 { return Err(AppError::new("not_found", "Element not found")); }
            }
            None => {
                c.execute("INSERT INTO elements(id, project_id, kind, name, asset_id, description, created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![new_id(), project_id, kind, name, asset_id, description, now_ms()])?;
            }
        }
        Ok(())
    })?;
    list(core, project_id)
}

pub fn delete(core: &Core, project_id: &str, id: &str) -> Res<Value> {
    core.db(|c| { c.execute("DELETE FROM elements WHERE id=?1 AND project_id=?2", params![id, project_id])?; Ok(()) })?;
    list(core, project_id)
}

/// The elements a prompt calls, in the order they first appear, with the exact text of each mention.
pub fn mentions(core: &Core, project_id: &str, prompt: &str) -> Res<Vec<(String, Element)>> {
    if !prompt.contains('@') { return Ok(vec![]); }
    let all = rows(core, project_id)?;
    let mut out: Vec<(String, Element)> = vec![];
    let chars: Vec<char> = prompt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') { j += 1; }
            let word: String = chars[i + 1..j].iter().collect();
            if let Some(e) = all.iter().find(|e| e.handle.to_lowercase() == word.to_lowercase()) {
                if !out.iter().any(|(_, x)| x.id == e.id) { out.push((format!("@{word}"), e.clone())); }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    Ok(out)
}

/// Replace each mention with the element's name and what the model calls its image, and
/// add the locked descriptions at the end. `tag(k)` names the k-th element's image.
pub fn apply(prompt: &str, used: &[(String, Element)], tag: impl Fn(usize) -> Option<String>) -> String {
    let mut out = prompt.to_string();
    let mut locked = vec![];
    for (k, (_, e)) in used.iter().enumerate() {
        let with = match tag(k) { Some(t) => format!("{} ({t})", e.name), None => e.name.clone() };
        // Every spelling of the mention, longest handle first so @Tent2 is not cut by @Tent.
        // Case is ignored only when lowercasing keeps every byte position (true for almost all text).
        let same_len = out.to_lowercase().len() == out.len() && e.handle.to_lowercase().len() == e.handle.len();
        let lower = if same_len { out.to_lowercase() } else { out.clone() };
        let needle = if same_len { format!("@{}", e.handle.to_lowercase()) } else { format!("@{}", e.handle) };
        let mut rebuilt = String::new();
        let mut at = 0;
        while let Some(pos) = lower[at..].find(&needle) {
            let start = at + pos;
            let end = start + needle.len();
            let boundary = lower[end..].chars().next().map(|c| !(c.is_alphanumeric() || c == '_')).unwrap_or(true);
            rebuilt.push_str(&out[at..start]);
            rebuilt.push_str(if boundary { &with } else { &out[start..end] });
            at = end;
        }
        rebuilt.push_str(&out[at..]);
        out = rebuilt;
        if !e.description.is_empty() {
            let d = e.description.trim_end_matches(['.', ' ']);
            locked.push(format!("{}: {d}.", e.name));
        }
    }
    if !locked.is_empty() { out = format!("{} {}", out.trim_end(), locked.join(" ")); }
    out
}

/// The image file identity of an element, for the plan fingerprint.
pub fn image_sha(core: &Core, project_id: &str, asset_id: &str) -> Res<Option<String>> {
    core.db(|c| Ok(c.query_row("SELECT sha256 FROM assets WHERE id=?1 AND project_id=?2", params![asset_id, project_id], |r| r.get(0)).optional()?))
}
