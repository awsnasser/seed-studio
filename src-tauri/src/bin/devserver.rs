//! Development-only HTTP bridge (feature `devserver`): lets the web UI run in a
//! normal browser against the real Rust core. Binds to 127.0.0.1, keeps secrets
//! in memory only, and uses its own data folder. Not part of the app bundle.
use seed_studio_lib::core::Emitter;
use seed_studio_lib::secrets::MemoryStore;
use seed_studio_lib::{api, Core};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Response, Server};

#[derive(Default)]
struct Buffer(Mutex<(u64, Vec<(u64, Value)>)>);
impl Emitter for Buffer {
    fn emit(&self, event: &str, payload: Value) {
        let mut g = self.0.lock().unwrap();
        g.0 += 1;
        let n = g.0;
        g.1.push((n, json!({"event": event, "payload": payload})));
        let len = g.1.len();
        if len > 2000 {
            g.1.drain(..len - 2000);
        }
    }
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn allowed_request(host: &str, origin: Option<&str>) -> bool {
    matches!(host, "127.0.0.1:1421" | "localhost:1421")
        && matches!(origin, None | Some("http://127.0.0.1:1420") | Some("http://localhost:1420"))
}

fn main() {
    let dir = std::env::var("SEED_STUDIO_DATA").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../.devdata").into());
    let events = Arc::new(Buffer::default());
    let core = Arc::new(Core::open(dir.clone().into(), events.clone(), Arc::new(MemoryStore::default()), 25).expect("open core"));
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap());
    let server = Server::http("127.0.0.1:1421").expect("bind 127.0.0.1:1421");
    println!("Seed Studio dev bridge on http://127.0.0.1:1421 (data: {dir})");
    for mut req in server.incoming_requests() {
        let host = req.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str()).unwrap_or("");
        let origin = req.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str());
        if !allowed_request(host, origin) {
            let _ = req.respond(Response::empty(403));
            continue;
        }
        let origin = origin.unwrap_or("http://127.0.0.1:1420").to_string();
        let (core, rt, events) = (core.clone(), rt.clone(), events.clone());
        std::thread::spawn(move || {
            let cors = [header("Access-Control-Allow-Origin", &origin), header("Vary", "Origin"),
                header("Access-Control-Allow-Headers", "content-type"), header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")];
            let url = req.url().to_string();
            let reply = |body: Vec<u8>, ctype: &str, status: u16| {
                let mut r = Response::from_data(body).with_status_code(status).with_header(header("Content-Type", ctype));
                for h in cors.iter().cloned() {
                    r = r.with_header(h);
                }
                r
            };
            let resp = if *req.method() == Method::Options {
                reply(vec![], "text/plain", 204)
            } else if url == "/rpc" {
                if *req.method() != Method::Post {
                    let _ = req.respond(reply(vec![], "text/plain", 405));
                    return;
                }
                let json_body = req.headers().iter().any(|h| h.field.equiv("Content-Type")
                    && h.value.as_str().split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"));
                if !json_body {
                    let _ = req.respond(reply(vec![], "text/plain", 415));
                    return;
                }
                let mut body = String::new();
                // Supports reference media uploads while bounding local request memory.
                const MAX_BODY: u64 = 128 * 1024 * 1024;
                if req.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).is_err() {
                    let _ = req.respond(reply(vec![], "text/plain", 400));
                    return;
                }
                if body.len() as u64 > MAX_BODY {
                    let _ = req.respond(reply(vec![], "text/plain", 413));
                    return;
                }
                let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let out = match rt.block_on(api::dispatch(&core, v["cmd"].as_str().unwrap_or(""), v["args"].clone())) {
                    Ok(r) => json!({"ok": true, "result": r}),
                    Err(e) => json!({"ok": false, "error": e}),
                };
                reply(out.to_string().into_bytes(), "application/json", 200)
            } else if let Some(since) = url.strip_prefix("/events?since=") {
                let since: u64 = since.parse().unwrap_or(0);
                let g = events.0.lock().unwrap();
                let list: Vec<&Value> = g.1.iter().filter(|(n, _)| *n > since).map(|(_, v)| v).collect();
                reply(json!({"next": g.0, "events": list}).to_string().into_bytes(), "application/json", 200)
            } else if let Some(id) = url.strip_prefix("/asset/") {
                match core.asset_bytes(id) {
                    Ok((_, mime, _, bytes)) => reply(bytes, &mime, 200),
                    Err(_) => reply(vec![], "text/plain", 404),
                }
            } else {
                reply(vec![], "text/plain", 404)
            };
            let _ = req.respond(resp);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::allowed_request;

    #[test]
    fn restricts_browser_origins_and_dns_rebinding_hosts() {
        assert!(allowed_request("127.0.0.1:1421", Some("http://127.0.0.1:1420")));
        assert!(allowed_request("localhost:1421", Some("http://localhost:1420")));
        assert!(allowed_request("127.0.0.1:1421", None));
        for origin in ["https://example.com", "null", "http://127.0.0.1:9999", "http://localhost:1420.evil.test"] {
            assert!(!allowed_request("127.0.0.1:1421", Some(origin)));
        }
        assert!(!allowed_request("evil.test:1421", None));
    }
}
