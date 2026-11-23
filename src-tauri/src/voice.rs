//! The project's voice track: imported once, stored locally as mono WAV, and cut
//! into the clip a video node sends as reference audio (for lip sync).
use crate::core::{new_id, now_ms, Core};
use crate::error::{AppError, Res};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

const PEAK_MS: usize = 50;

fn bad(m: impl Into<String>) -> AppError {
    AppError::new("bad_audio", m)
}

// ───────────────────────── voice track ─────────────────────────

/// Decode an audio file (mp3, wav, m4a) to mono 16-bit samples.
fn decode(bytes: Vec<u8>, name: &str) -> Res<(Vec<i16>, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let unreadable = || bad("This audio file could not be read. Use an mp3, wav or m4a file.");
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = name.rsplit('.').next() {
        hint.with_extension(&ext.to_lowercase());
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|_| unreadable())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or_else(unreadable)?.clone();
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|_| unreadable())?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut out: Vec<i16> = vec![];
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(_)) | Err(Error::ResetRequired) => break,
            Err(_) => return Err(unreadable()),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue, // a damaged frame: skip it
            Err(_) => break,
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<i16>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            out.push((frame.iter().map(|s| *s as i32).sum::<i32>() / channels as i32) as i16);
        }
    }
    if out.is_empty() {
        return Err(unreadable());
    }
    Ok((out, rate))
}

fn wav_bytes(samples: &[i16], rate: u32) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

pub fn audio_row(core: &Core, project_id: &str) -> Res<Value> {
    audio_row_by(core, project_id, None)
}

/// One audio file of the project by id, or its newest when no id is given.
pub fn audio_row_by(core: &Core, project_id: &str, id: Option<&str>) -> Res<Value> {
    core.db(|c| {
        Ok(c.query_row("SELECT id, name, sample_rate, duration_ms, peaks_json FROM audio WHERE project_id=?1 AND (?2 IS NULL OR id=?2) ORDER BY created_at DESC LIMIT 1", params![project_id, id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "sampleRate": r.get::<_, i64>(2)?, "durationMs": r.get::<_, i64>(3)?,
                "peakMs": PEAK_MS, "peaks": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
            }))
        }).optional()?.unwrap_or(Value::Null))
    })
}

/// Store the project's voice track as mono WAV, with a loudness envelope for the
/// waveform and pause detection. Nothing leaves the machine.
pub fn audio_import(core: &Core, project_id: &str, name: &str, bytes: Vec<u8>) -> Res<Value> {
    if bytes.len() > 60_000_000 {
        return Err(bad("The audio file is larger than 60 MB"));
    }
    let (samples, rate) = decode(bytes, name)?;
    let step = (rate as usize * PEAK_MS / 1000).max(1);
    let peaks: Vec<u32> = samples.chunks(step).map(|c| {
        let rms = (c.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt();
        (rms / 32768.0 * 1000.0).round() as u32
    }).collect();
    let id = new_id();
    let dir = core.data_dir.join("assets").join(project_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{id}.wav")), wav_bytes(&samples, rate))?;
    let duration = samples.len() as i64 * 1000 / rate as i64;
    core.db(|c| {
        c.execute("INSERT INTO audio(id,project_id,name,rel_path,sample_rate,duration_ms,peaks_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, project_id, name.chars().take(120).collect::<String>(), format!("assets/{project_id}/{id}.wav"), rate, duration, json!(peaks).to_string(), now_ms()])?;
        Ok(())
    })?;
    audio_row(core, project_id)
}

/// The WAV bytes of one clip of a voice track, padded with silence up to `min_ms`.
pub fn clip_wav(core: &Core, project_id: &str, audio_id: &str, start_ms: i64, end_ms: i64, min_ms: i64) -> Res<Vec<u8>> {
    let (owner, rel, rate): (String, String, u32) = core.db(|c| {
        c.query_row("SELECT project_id, rel_path, sample_rate FROM audio WHERE id=?1", [audio_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?
            .ok_or_else(|| AppError::new("not_found", "The voice track is missing"))
    })?;
    if owner != project_id {
        return Err(AppError::new("forbidden", "Voice track from another project"));
    }
    let file = std::fs::read(core.data_dir.join(rel)).map_err(|_| AppError::new("missing_file", "The voice file is missing from the project folder"))?;
    let pcm = &file[44.min(file.len())..];
    let at = |ms: i64| ((ms.max(0) as usize * rate as usize / 1000) * 2).min(pcm.len());
    let (a, b) = (at(start_ms), at(end_ms));
    if b <= a {
        return Err(bad("The voice clip is empty"));
    }
    let mut samples: Vec<i16> = pcm[a..b].chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]])).collect();
    let min = min_ms.max(0) as usize * rate as usize / 1000;
    if samples.len() < min {
        samples.resize(min, 0);
    }
    Ok(wav_bytes(&samples, rate))
}
