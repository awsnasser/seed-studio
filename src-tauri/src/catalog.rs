//! Model catalog: documented candidates (config/seed-catalog.json), used to build
//! the properties UI and to validate requests. Not account entitlements.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Aspect ratios offered in the app. Seedream has no aspect field: a chosen ratio
/// is sent as explicit `WIDTHxHEIGHT` pixels inside the model's documented range.
pub const ASPECTS: [&str; 10] = ["auto", "1:1", "4:5", "3:4", "2:3", "9:16", "4:3", "3:2", "16:9", "21:9"];

pub const CATALOG_JSON: &str = include_str!("../../config/seed-catalog.json");

#[derive(Clone, Debug)]
pub struct Model {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub candidate_ids: Vec<String>,
    pub controls: Value,
}

#[derive(Clone, Debug)]
pub struct Catalog {
    pub raw: Value,
    pub models: Vec<Model>,
    pub regions: BTreeMap<String, String>,
}

fn field_name(k: &str) -> &str {
    match k {
        "aspect" => "Aspect ratio",
        "size" => "Size",
        "output_format" => "Format",
        "background" => "Background",
        "optimize_prompt" => "Prompt optimization",
        other => other,
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

impl Catalog {
    pub fn load() -> Catalog {
        let raw: Value = serde_json::from_str(CATALOG_JSON).expect("seed-catalog.json is valid");
        let models = raw["models"].as_array().unwrap().iter().map(|m| Model {
            key: m["key"].as_str().unwrap().into(),
            label: m["label"].as_str().unwrap().into(),
            kind: m["kind"].as_str().unwrap().into(),
            candidate_ids: strs(&m["candidateIds"]),
            controls: m["controls"].clone(),
        }).collect();
        let regions = raw["regions"].as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        Catalog { raw, models, regions }
    }

    pub fn model(&self, key: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.key == key)
    }
    pub fn suggested(&self, kind: &str) -> String {
        self.raw["suggestedFirstUse"][kind].as_str().unwrap_or_default().to_string()
    }
    pub fn max_refs(&self, key: &str) -> usize {
        self.model(key).and_then(|m| m.controls["maxReferencesInApp"].as_u64()).unwrap_or(10) as usize
    }
    pub fn background_modes(&self, key: &str) -> Vec<String> {
        self.model(key).map(|m| strs(&m.controls["background"]["modes"])).unwrap_or_default()
    }
    /// True when transparency needs exactly one PNG reference with alpha (5.0 Pro / Flash).
    pub fn transparent_needs_alpha_ref(&self, key: &str) -> bool {
        self.model(key).and_then(|m| m.controls["background"]["transparentRequires"].as_str())
            .map(|s| s.contains("one_png_reference")).unwrap_or(false)
    }

    /// Fields the properties panel (and the agent) may set for this model.
    pub fn options(&self, key: &str) -> Option<Value> {
        let m = self.model(key)?;
        if m.kind == "video" {
            return Some(json!({"key": m.key, "label": m.label, "kind": m.kind, "candidateIds": m.candidate_ids, "controls": m.controls}));
        }
        if m.kind != "image" {
            return Some(json!({"key": m.key, "label": m.label, "kind": m.kind,
                "thinking": m.controls["thinkingModes"], "candidateIds": m.candidate_ids}));
        }
        let opt = strs(&m.controls["promptOptimization"]);
        Some(json!({
            "key": m.key, "label": m.label, "kind": m.kind, "candidateIds": m.candidate_ids,
            "fields": {
                "aspect": ASPECTS,
                "size": m.controls["sizePresets"],
                "output_format": m.controls["outputFormats"],
                "background": if m.controls["background"].is_null() { Value::Null } else { m.controls["background"]["modes"].clone() },
                "optimize_prompt": if opt.len() > 1 { json!(opt) } else { Value::Null },
            },
            "maxReferences": self.max_refs(key),
            "transparentRule": m.controls["background"]["transparentRequires"],
            "defaults": self.default_settings(key),
        }))
    }

    /// Pixel dimensions for a chosen aspect ratio at the chosen quality tier,
    /// kept inside the model's documented total-pixel range. None = send the tier.
    pub fn pixel_size(&self, key: &str, settings: &Map<String, Value>) -> Option<(u32, u32)> {
        let aspect = settings.get("aspect")?.as_str()?;
        let (a, b) = aspect.split_once(':')?;
        let ratio = a.parse::<f64>().ok()? / b.parse::<f64>().ok()?;
        let side: f64 = match settings.get("size").and_then(|v| v.as_str()).unwrap_or("2K") {
            "1K" => 1024.0, "1.5K" => 1536.0, "2K" => 2048.0, "3K" => 3072.0, "4K" => 4096.0,
            _ => return None,
        };
        let m = self.model(key)?;
        let (min, max) = (m.controls["pixelRange"][0].as_f64()?, m.controls["pixelRange"][1].as_f64()?);
        let budget = (side * side).clamp(min, max);
        let mut w = ((budget * ratio).sqrt() / 16.0).floor() * 16.0;
        let mut h = ((budget / ratio).sqrt() / 16.0).floor() * 16.0;
        while w * h < min {
            w += 16.0;
            h = (w / ratio / 16.0).ceil() * 16.0;
        }
        if w * h > max || !(1.0 / 16.0..=16.0).contains(&(w / h)) {
            return None;
        }
        Some((w as u32, h as u32))
    }

    /// Settings as they go on the wire: the app-only `aspect` is folded into `size`.
    pub fn wire_settings(&self, key: &str, settings: &Map<String, Value>) -> Map<String, Value> {
        let mut s = settings.clone();
        s.remove("aspect");
        if let Some((w, h)) = self.pixel_size(key, settings) {
            s.insert("size".into(), json!(format!("{w}x{h}")));
        }
        s
    }

    pub fn default_settings(&self, key: &str) -> Map<String, Value> {
        let mut s = Map::new();
        let Some(m) = self.model(key) else { return s };
        let sizes = strs(&m.controls["sizePresets"]);
        let size = if sizes.iter().any(|x| x == "1.5K") { "1.5K".to_string() } else { sizes.first().cloned().unwrap_or_default() };
        s.insert("size".into(), json!(size));
        s.insert("output_format".into(), json!("png"));
        if self.background_modes(key).iter().any(|b| b == "opaque") {
            s.insert("background".into(), json!("opaque"));
        }
        s
    }

    /// Closed validation of a seedream node's settings for one model.
    pub fn validate_settings(&self, key: &str, settings: &Map<String, Value>) -> Result<(), String> {
        let m = self.model(key).filter(|m| m.kind == "image")
            .ok_or_else(|| format!("Unknown image model: {key}"))?;
        for (k, v) in settings {
            let val = v.as_str().ok_or_else(|| format!("The value of {k} must be a string"))?;
            let allowed = match k.as_str() {
                "size" => strs(&m.controls["sizePresets"]),
                "aspect" => ASPECTS.iter().map(|s| s.to_string()).collect(),
                "output_format" => strs(&m.controls["outputFormats"]),
                "background" => self.background_modes(key),
                "optimize_prompt" => strs(&m.controls["promptOptimization"]),
                _ => return Err(format!("Field {k} is not known for this model")),
            };
            if allowed.is_empty() {
                return Err(format!("{} does not support the field {k}", m.label));
            }
            if !allowed.iter().any(|a| a == val) {
                return Err(format!("{val} is not allowed for {k} in {} (available: {})", m.label, allowed.join(" / ")));
            }
        }
        if settings.get("background").and_then(|v| v.as_str()) == Some("transparent")
            && settings.get("output_format").and_then(|v| v.as_str()) != Some("png") {
            return Err("A transparent background requires PNG output".into());
        }
        Ok(())
    }

    /// Carry settings over to another model; report every change so the UI can explain it.
    pub fn normalize_settings(&self, key: &str, old: &Map<String, Value>) -> (Map<String, Value>, Vec<String>) {
        let defaults = self.default_settings(key);
        let label = self.model(key).map(|m| m.label.clone()).unwrap_or_default();
        let mut out = Map::new();
        let mut notes = vec![];
        for (k, v) in old {
            let mut one = Map::new();
            one.insert(k.clone(), v.clone());
            if self.validate_settings(key, &one).is_ok() {
                out.insert(k.clone(), v.clone());
            } else if let Some(d) = defaults.get(k) {
                notes.push(format!("{}: was {}, now {} because {label} does not offer it", field_name(k), v.as_str().unwrap_or(""), d.as_str().unwrap_or("")));
                out.insert(k.clone(), d.clone());
            } else {
                notes.push(format!("{}: removed because {label} does not support it", field_name(k)));
            }
        }
        for (k, d) in &defaults {
            out.entry(k.clone()).or_insert_with(|| d.clone());
        }
        if self.validate_settings(key, &out).is_err() {
            // Only the transparent/PNG pairing can still fail here.
            if let Some(d) = defaults.get("background") {
                out.insert("background".into(), d.clone());
                notes.push("Background: reset to the default".into());
            }
        }
        (out, notes)
    }
}
