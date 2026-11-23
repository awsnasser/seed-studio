#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Desktop shell: one IPC command, one image protocol. All logic is in the lib.
use seed_studio_lib::core::Emitter;
use seed_studio_lib::secrets::KeychainStore;
use seed_studio_lib::{api, Core};
use serde_json::Value;
use std::sync::Arc;
use tauri::{Emitter as _, Manager};

struct TauriEmitter(tauri::AppHandle);
impl Emitter for TauriEmitter {
    fn emit(&self, event: &str, payload: Value) {
        let _ = self.0.emit(event, payload);
    }
}

#[tauri::command]
async fn rpc(core: tauri::State<'_, Arc<Core>>, cmd: String, args: Value) -> Result<Value, Value> {
    let core = core.inner().clone();
    api::dispatch(&core, &cmd, args).await.map_err(|e| serde_json::to_value(e).unwrap_or(Value::Null))
}

fn main() {
    tauri::Builder::default()
        // seedasset://localhost/<asset id> → bytes of a project image, looked up by id only.
        .register_uri_scheme_protocol("seedasset", |ctx, request| {
            let id = request.uri().path().trim_start_matches('/').to_string();
            let found = ctx.app_handle().try_state::<Arc<Core>>().and_then(|core| core.asset_bytes(&id).ok());
            // Video elements ask for byte ranges; answer them so playback and seeking work.
            let range = request.headers().get("range").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes="))
                .and_then(|v| v.split_once('-')).map(|(a, b)| (a.parse::<usize>().ok(), b.parse::<usize>().ok()));
            match found {
                Some((_, mime, _, bytes)) => match range {
                    Some((Some(start), end)) if start < bytes.len() => {
                        let end = end.unwrap_or(bytes.len() - 1).min(bytes.len() - 1);
                        tauri::http::Response::builder().status(206)
                            .header("Content-Type", mime)
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Range", format!("bytes {start}-{end}/{}", bytes.len()))
                            .body(bytes[start..=end].to_vec()).unwrap()
                    }
                    _ => tauri::http::Response::builder()
                        .header("Content-Type", mime)
                        .header("Accept-Ranges", "bytes")
                        .header("Cache-Control", "max-age=31536000, immutable")
                        .body(bytes).unwrap(),
                },
                None => tauri::http::Response::builder().status(404).body(Vec::new()).unwrap(),
            }
        })
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let core = Core::open(dir, Arc::new(TauriEmitter(app.handle().clone())), Arc::new(KeychainStore), 25)?;
            app.manage(Arc::new(core));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![rpc])
        .run(tauri::generate_context!())
        .expect("error while running Seed Studio");
}
