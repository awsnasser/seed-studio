//! Copy the *look* of one image to another without copying what is in it.
//! - Camera: the chat model reads one image and writes only its camera and composition;
//!   the text is kept on the image node and added to its prompt. The image itself is never
//!   sent with the generation, so none of its people, objects or colours can leak in.
//! - Colour: done on this Mac with arithmetic (no request): the colour statistics of the
//!   approved image are applied to the other one, pixel positions untouched.
//! - Palette: a strip of the approved image's main colours, for a Seedream re-grade.
use crate::core::{new_id, now_ms, Core};
use crate::error::{AppError, Res};
use crate::provider::ChatRequest;
use image::{DynamicImage, RgbaImage};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub const CAMERA_ASK: &str = "Describe only the camera and composition of this image, so that a different picture can be framed identically. Cover: shot size; camera height and angle (eye level, low, high, tilt); lens feel (wide, normal, telephoto) and depth of field; camera distance; where the main subject sits in the frame, how much of the frame it fills and which way it faces relative to the camera; the horizon line; the foreground, midground and background as abstract masses (for example “a large mass in the left third”); leading lines. Do not name or describe the people, objects, clothing, place, colours, lighting or style: say “the subject”, “a secondary element”, “the background”. Write one compact paragraph in English, under 120 words, with no preface.";

/// What is added to an image prompt for a stored camera description.
pub fn camera_suffix(camera: &str) -> String {
    format!("Camera and composition — frame the picture exactly like this, and take nothing else from it: {}", camera.trim())
}

fn own_image(core: &Core, project_id: &str, asset_id: &str, small: bool) -> Res<(String, Vec<u8>)> {
    let (owner, mime, _, bytes) = core.asset_bytes(&if small { format!("{asset_id}~t") } else { asset_id.to_string() })?;
    if owner != project_id { return Err(AppError::new("forbidden", "Image from another project")); }
    if !mime.starts_with("image/") { return Err(AppError::new("bad_image", "That file is not an image")); }
    Ok((mime, bytes))
}

/// One text request: the chat model writes the camera of one image. Never repeated on its own.
pub async fn camera(core: &Core, project_id: &str, asset_id: &str) -> Res<Value> {
    let (mime, bytes) = own_image(core, project_id, asset_id, true)?;
    let key = core.chat_model_key();
    let model_id = core.resolve_model_id(&key)?;
    let set = core.providers()?;
    let url = format!("data:{mime};base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes));
    let messages = vec![json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": url}}, {"type": "text", "text": CAMERA_ASK}]})];
    let req = ChatRequest { model: model_id.clone(), messages, tools: None, thinking: None, cancel: Default::default() };
    let out = set.chat.chat_stream(req, &mut |_| {}).await.map_err(|e| e.to_app(&set.region, &model_id))?;
    let text: String = out.text.trim().chars().take(1800).collect();
    if !out.complete || text.is_empty() { return Err(AppError::new("incomplete", "The camera description came back incomplete. Nothing was changed; try again.")); }
    core.mark_tried(&set, &model_id);
    Ok(json!({"text": text, "provider": set.name, "modelId": model_id}))
}

// ───────────── colour ─────────────

fn decode(bytes: &[u8]) -> Res<DynamicImage> {
    image::load_from_memory(bytes).map_err(|_| AppError::new("bad_image", "This works with PNG and JPEG images. Convert the image to PNG or JPEG first."))
}

fn lin(v: u8) -> f32 { let c = v as f32 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }
fn unlin(c: f32) -> u8 { let c = c.clamp(0.0, 1.0); ((if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as u8 }

/// sRGB → Oklab: lightness and two colour axes that can be shifted independently.
fn to_lab(lut: &[f32; 256], p: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]);
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s, 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s, 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s]
}
fn from_lab(c: [f32; 3]) -> [u8; 3] {
    let l = (c[0] + 0.396_337_78 * c[1] + 0.215_803_76 * c[2]).powi(3);
    let m = (c[0] - 0.105_561_346 * c[1] - 0.063_854_17 * c[2]).powi(3);
    let s = (c[0] - 0.089_484_18 * c[1] - 1.291_485_5 * c[2]).powi(3);
    [unlin(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s), unlin(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s), unlin(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s)]
}
fn lut() -> [f32; 256] { let mut t = [0.0; 256]; for (i, v) in t.iter_mut().enumerate() { *v = lin(i as u8); } t }

/// Mean and spread of each axis over the visible pixels.
fn stats(img: &RgbaImage, lut: &[f32; 256]) -> ([f32; 3], [f32; 3]) {
    let (mut n, mut sum, mut sq) = (0f64, [0f64; 3], [0f64; 3]);
    for p in img.pixels().filter(|p| p[3] > 8) {
        let c = to_lab(lut, [p[0], p[1], p[2]]);
        for k in 0..3 { sum[k] += c[k] as f64; sq[k] += (c[k] * c[k]) as f64; }
        n += 1.0;
    }
    if n == 0.0 { return ([0.5, 0.0, 0.0], [0.1, 0.02, 0.02]); }
    let mean = [0, 1, 2].map(|k| (sum[k] / n) as f32);
    let dev = [0, 1, 2].map(|k| ((sq[k] / n - (sum[k] / n).powi(2)).max(0.0).sqrt() as f32).max(1e-4));
    (mean, dev)
}

/// The target image with the colour of `source`: same pixels in the same places, new grade.
/// `strength` 0–1 blends between the original and the full match. Returns (bytes, is_jpeg).
pub fn match_colour(target: &[u8], source: &[u8], strength: f32) -> Res<Vec<u8>> {
    let is_jpeg = matches!(image::guess_format(target), Ok(image::ImageFormat::Jpeg));
    let mut img = decode(target)?.to_rgba8();
    let src = decode(source)?.thumbnail(512, 512).to_rgba8();
    let small = DynamicImage::ImageRgba8(img.clone()).thumbnail(512, 512).to_rgba8();
    let t = lut();
    let ((sm, sd), (tm, td)) = (stats(&src, &t), stats(&small, &t));
    // The spread is matched too, within limits, so a flat image is not blown out.
    let gain = [0, 1, 2].map(|k| (sd[k] / td[k]).clamp(0.6, 1.7));
    let k = strength.clamp(0.0, 1.0);
    for p in img.pixels_mut() {
        let c = to_lab(&t, [p[0], p[1], p[2]]);
        let out = from_lab([0, 1, 2].map(|i| c[i] + k * ((c[i] - tm[i]) * gain[i] + sm[i] - c[i])));
        p.0 = [out[0], out[1], out[2], p[3]];
    }
    let mut buf = std::io::Cursor::new(vec![]);
    if is_jpeg {
        DynamicImage::ImageRgba8(img).to_rgb8().write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95))
    } else {
        img.write_to(&mut buf, image::ImageFormat::Png)
    }.map_err(|e| AppError::new("bad_image", format!("The matched image could not be written: {e}")))?;
    Ok(buf.into_inner())
}

/// The main colours of an image as a strip (dark to light, widths by how much of the image each covers).
pub fn palette(source: &[u8]) -> Res<Vec<u8>> {
    const K: usize = 6;
    let px: Vec<[f32; 3]> = decode(source)?.thumbnail(96, 96).to_rgba8().pixels().filter(|p| p[3] > 8).map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    if px.is_empty() { return Err(AppError::new("bad_image", "The image has no visible pixels")); }
    let luma = |c: &[f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    // Deterministic start: pixels taken at even steps through the image sorted by brightness.
    let mut sorted = px.clone();
    sorted.sort_by(|a, b| luma(a).total_cmp(&luma(b)));
    let mut centres: Vec<[f32; 3]> = (0..K).map(|i| sorted[(sorted.len() - 1) * (2 * i + 1) / (2 * K)]).collect();
    let mut counts = [0usize; K];
    for _ in 0..12 {
        let (mut sum, mut n) = ([[0f32; 3]; K], [0usize; K]);
        for p in &px {
            let best = (0..K).min_by(|&a, &b| dist(p, &centres[a]).total_cmp(&dist(p, &centres[b]))).unwrap();
            for k in 0..3 { sum[best][k] += p[k]; }
            n[best] += 1;
        }
        for i in 0..K { if n[i] > 0 { centres[i] = [sum[i][0] / n[i] as f32, sum[i][1] / n[i] as f32, sum[i][2] / n[i] as f32]; } }
        counts = n;
    }
    let mut blocks: Vec<([f32; 3], usize)> = centres.into_iter().zip(counts).filter(|(_, n)| *n > 0).collect();
    blocks.sort_by(|a, b| luma(&a.0).total_cmp(&luma(&b.0)));
    let (w, h, total) = (1024u32, 256u32, px.len() as f32);
    // No block thinner than 6% of the strip, so a small but important colour stays visible.
    let shares: Vec<f32> = blocks.iter().map(|(_, n)| (*n as f32 / total).max(0.06)).collect();
    let all: f32 = shares.iter().sum();
    let mut img = image::RgbImage::new(w, h);
    let mut x0 = 0f32;
    for (i, (c, _)) in blocks.iter().enumerate() {
        let x1 = if i + 1 == blocks.len() { w as f32 } else { x0 + shares[i] / all * w as f32 };
        for x in x0.round() as u32..(x1.round() as u32).min(w) {
            for y in 0..h { img.put_pixel(x, y, image::Rgb([c[0].round() as u8, c[1].round() as u8, c[2].round() as u8])); }
        }
        x0 = x1;
    }
    let mut buf = std::io::Cursor::new(vec![]);
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| AppError::new("bad_image", e.to_string()))?;
    Ok(buf.into_inner())
}
fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 { (0..3).map(|k| (a[k] - b[k]).powi(2)).sum() }

fn title_of(core: &Core, asset_id: &str) -> String {
    core.db(|c| Ok(c.query_row("SELECT title FROM assets WHERE id=?1", [asset_id], |r| r.get::<_, String>(0)).unwrap_or_default())).unwrap_or_default()
}

/// A palette strip of one project image, saved as a reference image. Nothing is sent.
pub fn palette_asset(core: &Core, project_id: &str, from: &str) -> Res<Value> {
    let (_, bytes) = own_image(core, project_id, from, false)?;
    let file = core.write_asset_file(project_id, &palette(&bytes)?)?;
    let name = title_of(core, from);
    let title = format!("Palette of {}", if name.is_empty() { "image" } else { &name });
    core.insert_reference_asset(project_id, &file, &title)?;
    Ok(json!({"assetId": file.id, "title": title}))
}

/// Give an image node a new version: its current image with the colour of `from`.
/// Nothing is sent. The earlier version stays under Versions and can be put back.
pub fn colour_version(core: &Core, project_id: &str, node_id: &str, from: &str, strength: f32) -> Res<Value> {
    let current: Option<(String, String)> = core.db(|c| Ok(c.query_row(
        "SELECT id, asset_id FROM node_runs WHERE project_id=?1 AND node_id=?2 AND status='succeeded' AND asset_id IS NOT NULL ORDER BY finished_at DESC LIMIT 1",
        params![project_id, node_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?))?;
    let (run, target) = current.ok_or_else(|| AppError::new("not_found", "This node has no image yet. Generate it first."))?;
    if target == from { return Err(AppError::new("same_image", "Choose a different image to take the colour from.")); }
    let (_, target_bytes) = own_image(core, project_id, &target, false)?;
    let (_, from_bytes) = own_image(core, project_id, from, false)?;
    let file = core.write_asset_file(project_id, &match_colour(&target_bytes, &from_bytes, strength)?)?;
    let (new_run, now) = (new_id(), now_ms());
    core.db(|c| {
        let tx = c.transaction()?;
        // Same record as the image it came from (model, prompt, simulation or not), with the new file.
        tx.execute("INSERT INTO assets(id,project_id,kind,rel_path,mime,width,height,sha256,title,provider,region,model_id,prompt,settings_json,node_id,node_run_id,created_at)
                    SELECT ?1,project_id,'generated',?2,?3,?4,?5,?6,title,provider,region,model_id,prompt,settings_json,node_id,?7,?8 FROM assets WHERE id=?9",
            params![file.id, file.rel_path, file.mime, file.width, file.height, file.sha, new_run, now, target])?;
        tx.execute("INSERT INTO asset_inputs(asset_id,input_asset_id,ord) VALUES(?1,?2,0),(?1,?3,1)", params![file.id, target, from])?;
        tx.execute("INSERT INTO node_runs(id,run_id,project_id,node_id,kind,status,fingerprint,provider,region,model_id,final_prompt,settings_json,asset_id,text_output,error,usage_json,started_at,finished_at)
                    SELECT ?1,run_id,project_id,node_id,kind,status,fingerprint,provider,region,model_id,final_prompt,settings_json,?2,NULL,NULL,NULL,?3,?3 FROM node_runs WHERE id=?4",
            params![new_run, file.id, now, run])?;
        tx.commit()?;
        Ok(())
    })?;
    core.emit("canvas-changed", json!({"projectId": project_id}));
    core.emit("run-update", json!({"projectId": project_id}));
    core.canvas_state(project_id)
}
