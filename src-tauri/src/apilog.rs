//! A record of every HTTP request the app sends to the provider or its gateway:
//! when, where, which model, the non-content options, the HTTP status, the provider's
//! request id, the task id and the usage it reported.
//!
//! Never recorded: the API key, any header, prompts, messages, images, audio or
//! response bodies. A row is written before the request leaves and completed when the
//! answer arrives, so a request the app never heard back from stays visible as "unknown".
use crate::error::Res;
use crate::provider::{ErrKind, ProviderError};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct ApiLog {
    db: Arc<Mutex<Connection>>,
}

#[derive(Default)]
struct Meta {
    status: Option<u16>,
    request_id: Option<String>,
    usage: Option<Value>,
    task_id: Option<String>,
    partial: bool,
    /// A short fact from the answer that is not content, e.g. a video task's status.
    info: Option<String>,
}

/// One request in flight. Dropping it unfinished (the app stopped waiting) closes the row as unknown.
pub struct Call {
    log: Option<Arc<ApiLog>>,
    id: i64,
    post: bool,
    meta: Mutex<Meta>,
    done: AtomicBool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The options of a request body without its content: scalars are kept, and prompts,
/// messages, images, audio and tool definitions are reduced to counts.
pub fn safe_params(body: &Value) -> Value {
    let mut out = Map::new();
    let Some(obj) = body.as_object() else { return json!({}) };
    for (k, v) in obj {
        match k.as_str() {
            "model" => {}
            "prompt" => { out.insert("prompt_chars".into(), json!(v.as_str().map(|s| s.chars().count()).unwrap_or(0))); }
            "messages" | "tools" => { out.insert(format!("{k}_count"), json!(v.as_array().map(|a| a.len()).unwrap_or(0))); }
            "image" => { out.insert("input_images".into(), json!(v.as_array().map(|a| a.len()).unwrap_or(1))); }
            "content" => {
                for part in v.as_array().into_iter().flatten() {
                    let t = part["type"].as_str().unwrap_or("other");
                    match t {
                        "draft_task" => { out.insert("draft_task".into(), part["draft_task"]["id"].clone()); }
                        "text" => { out.insert("prompt_chars".into(), json!(part["text"].as_str().map(|s| s.chars().count()).unwrap_or(0))); }
                        _ => {
                            let key = format!("{}_count", t.trim_end_matches("_url"));
                            let n = out.get(&key).and_then(|n| n.as_u64()).unwrap_or(0) + 1;
                            out.insert(key, json!(n));
                        }
                    }
                }
            }
            _ => match v {
                Value::Bool(_) | Value::Number(_) => { out.insert(k.clone(), v.clone()); }
                Value::String(s) if s.len() <= 80 => { out.insert(k.clone(), v.clone()); }
                Value::Object(_) if v.to_string().len() <= 120 => { out.insert(k.clone(), v.clone()); }
                _ => {}
            },
        }
    }
    Value::Object(out)
}

impl ApiLog {
    pub fn new(db: Arc<Mutex<Connection>>) -> Arc<ApiLog> {
        Arc::new(ApiLog { db })
    }

    fn begin(&self, purpose: &str, method: &str, url: &str, connection: &str, model: Option<&str>, params: &Value, bytes_out: usize) -> i64 {
        // Host and path only: a query string can carry a name or a signed token.
        let (host, path) = match reqwest::Url::parse(url) {
            Ok(u) => (u.host_str().unwrap_or("").to_string(), if purpose == "download" { "(result file)".to_string() } else { u.path().to_string() }),
            Err(_) => (String::new(), String::new()),
        };
        let c = self.db.lock().unwrap();
        c.execute("INSERT INTO api_log(started_at,purpose,method,host,path,connection,model,params_json,bytes_out,outcome) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'sending')",
            params![now_ms(), purpose, method, host, path, connection, model, params.to_string(), bytes_out as i64])
            .map(|_| c.last_insert_rowid()).unwrap_or(0)
    }

    fn end(&self, id: i64, outcome: &str, m: &Meta, code: Option<&str>, message: Option<&str>) {
        if id == 0 { return; }
        let c = self.db.lock().unwrap();
        let _ = c.execute("UPDATE api_log SET finished_at=?1, outcome=?2, http_status=?3, error_code=?4, error_message=?5, request_id=?6, task_id=?7, usage_json=?8, info=?9 WHERE id=?10",
            params![now_ms(), outcome, m.status, code, message, m.request_id, m.task_id, m.usage.as_ref().map(|u| u.to_string()), m.info, id]);
    }
}

impl Call {
    pub fn begin(log: Option<Arc<ApiLog>>, purpose: &str, method: &str, url: &str, connection: &str, model: Option<&str>, params: Value, bytes_out: usize) -> Call {
        let id = log.as_ref().map(|l| l.begin(purpose, method, url, connection, model, &params, bytes_out)).unwrap_or(0);
        Call { log, id, post: method == "POST", meta: Mutex::new(Meta::default()), done: AtomicBool::new(false) }
    }
    /// The response headers arrived: keep the status and the provider's request id.
    pub fn seen(&self, resp: &reqwest::Response) {
        let mut m = self.meta.lock().unwrap();
        m.status = Some(resp.status().as_u16());
        m.request_id = ["x-request-id", "x-tt-logid", "x-client-request-id"].iter()
            .find_map(|h| resp.headers().get(*h).and_then(|v| v.to_str().ok()).map(String::from));
    }
    pub fn usage(&self, usage: Option<&Value>) {
        if let Some(u) = usage.filter(|u| u.is_object()) {
            self.meta.lock().unwrap().usage = Some(u.clone());
        }
    }
    pub fn task(&self, id: &str) {
        self.meta.lock().unwrap().task_id = Some(id.to_string());
    }
    pub fn info(&self, text: &str) {
        if !text.is_empty() { self.meta.lock().unwrap().info = Some(text.chars().take(60).collect()); }
    }
    /// A streamed answer that stopped before the provider signalled the end.
    pub fn partial(&self) {
        self.meta.lock().unwrap().partial = true;
    }
    pub fn finish<T>(&self, r: &Result<T, ProviderError>) {
        if self.done.swap(true, Ordering::SeqCst) { return; }
        let Some(log) = &self.log else { return };
        let mut m = self.meta.lock().unwrap();
        match r {
            Ok(_) => {
                let (outcome, note) = if m.partial { ("partial", Some("the stream ended before the provider signalled the end")) } else { ("ok", None) };
                log.end(self.id, outcome, &m, None, note);
            }
            Err(e) => {
                if e.status.is_some() { m.status = e.status; }
                if e.request_id.is_some() { m.request_id = e.request_id.clone(); }
                let outcome = match e.kind {
                    ErrKind::Network => "not_sent",
                    ErrKind::Uncertain => "unknown",
                    _ => "error",
                };
                log.end(self.id, outcome, &m, e.code.as_deref(), Some(&e.message));
            }
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if self.done.swap(true, Ordering::SeqCst) { return; }
        if let Some(log) = &self.log {
            let m = self.meta.lock().unwrap();
            log.end(self.id, if self.post { "unknown" } else { "error" }, &m, None, Some("the app stopped waiting for this request"));
        }
    }
}

/// Requests that were in flight when the app last closed have an unknown outcome.
pub fn close_open_rows(conn: &Connection) -> Res<()> {
    conn.execute("UPDATE api_log SET outcome='unknown', error_message='The app closed during the request; the outcome is unknown', finished_at=?1 WHERE outcome='sending'", params![now_ms()])?;
    Ok(())
}

const COLUMNS: &str = "id,started_at,finished_at,purpose,method,host,path,connection,model,params_json,bytes_out,outcome,http_status,error_code,error_message,request_id,task_id,usage_json,info";

fn row_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "startedAt": r.get::<_, i64>(1)?, "finishedAt": r.get::<_, Option<i64>>(2)?,
        "purpose": r.get::<_, String>(3)?, "method": r.get::<_, String>(4)?, "host": r.get::<_, String>(5)?, "path": r.get::<_, String>(6)?,
        "connection": r.get::<_, String>(7)?, "model": r.get::<_, Option<String>>(8)?, "params": parse(r.get(9)?),
        "bytesOut": r.get::<_, i64>(10)?, "outcome": r.get::<_, String>(11)?, "httpStatus": r.get::<_, Option<i64>>(12)?,
        "errorCode": r.get::<_, Option<String>>(13)?, "errorMessage": r.get::<_, Option<String>>(14)?,
        "requestId": r.get::<_, Option<String>>(15)?, "taskId": r.get::<_, Option<String>>(16)?, "usage": parse(r.get(17)?), "info": r.get::<_, Option<String>>(18)?,
    }))
}

/// Requests that can cost money: everything sent with POST (generation and uploads).
const PAID: &str = "method='POST'";

/// Newest first. `paid_only` leaves out status checks, balance reads and downloads.
pub fn list(conn: &Connection, paid_only: bool, limit: i64, offset: i64) -> Res<Value> {
    let filter = if paid_only { PAID } else { "1=1" };
    let mut st = conn.prepare(&format!("SELECT {COLUMNS} FROM api_log WHERE {filter} ORDER BY id DESC LIMIT ?1 OFFSET ?2"))?;
    let rows: Vec<Value> = st.query_map(params![limit.clamp(1, 1000), offset.max(0)], row_json)?.collect::<Result<_, _>>()?;
    let total: i64 = conn.query_row(&format!("SELECT count(*) FROM api_log WHERE {filter}"), [], |r| r.get(0))?;
    let mut st = conn.prepare("SELECT purpose, count(*), sum(outcome IN ('ok','partial')), sum(outcome='unknown'),
        sum(coalesce(json_extract(usage_json,'$.total_tokens'),0)) FROM api_log WHERE method='POST' GROUP BY purpose ORDER BY purpose")?;
    let totals: Vec<Value> = st.query_map([], |r| Ok(json!({
        "purpose": r.get::<_, String>(0)?, "requests": r.get::<_, i64>(1)?, "ok": r.get::<_, i64>(2)?, "unknown": r.get::<_, i64>(3)?, "tokens": r.get::<_, f64>(4)?,
    })))?.collect::<Result<_, _>>()?;
    // A video's usage arrives with its status check, not with the request that created it.
    let video_tokens: f64 = conn.query_row("SELECT coalesce(sum(t),0) FROM (SELECT max(coalesce(json_extract(usage_json,'$.total_tokens'),0)) t FROM api_log WHERE purpose='video_status' AND task_id IS NOT NULL GROUP BY task_id)", [], |r| r.get(0))?;
    let first: Option<i64> = conn.query_row("SELECT min(started_at) FROM api_log", [], |r| r.get(0))?;
    Ok(json!({"rows": rows, "total": total, "totals": totals, "videoTokens": video_tokens, "since": first}))
}

/// 2026-11-17T12:44:37Z from Unix milliseconds, without a date crate.
pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn cell(s: &str) -> String {
    // Quote always; a leading = + - @ would be run as a formula by a spreadsheet.
    let s = s.replace('\r', " ").replace('\n', " ");
    let guard = if s.starts_with(['=', '+', '-', '@']) { "'" } else { "" };
    format!("\"{guard}{}\"", s.replace('"', "\"\""))
}

/// The whole log as CSV, oldest first.
pub fn csv(conn: &Connection) -> Res<(String, usize)> {
    let mut out = String::from("id,sent_utc,finished_utc,seconds,purpose,method,host,path,connection,model,outcome,http_status,request_id,task_id,task_status,total_tokens,usage,options,bytes_sent,error_code,error_message\n");
    let mut st = conn.prepare(&format!("SELECT {COLUMNS} FROM api_log ORDER BY id"))?;
    let rows: Vec<Value> = st.query_map([], row_json)?.collect::<Result<_, _>>()?;
    for r in &rows {
        let text = |k: &str| r[k].as_str().unwrap_or("").to_string();
        let num = |k: &str| r[k].as_i64().map(|n| n.to_string()).unwrap_or_default();
        let (a, b) = (r["startedAt"].as_i64().unwrap_or(0), r["finishedAt"].as_i64());
        let obj = |k: &str| if r[k].is_null() || r[k] == json!({}) { String::new() } else { r[k].to_string() };
        let fields = [
            num("id"), iso(a), b.map(iso).unwrap_or_default(), b.map(|b| format!("{:.1}", (b - a) as f64 / 1000.0)).unwrap_or_default(),
            text("purpose"), text("method"), text("host"), text("path"), text("connection"), text("model"), text("outcome"), num("httpStatus"),
            text("requestId"), text("taskId"), text("info"), r["usage"]["total_tokens"].as_f64().map(|t| format!("{t:.0}")).unwrap_or_default(),
            obj("usage"), obj("params"), num("bytesOut"), text("errorCode"), text("errorMessage"),
        ];
        out.push_str(&fields.iter().map(|f| cell(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    Ok((out, rows.len()))
}
