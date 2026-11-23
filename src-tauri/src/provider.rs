//! Provider boundary. v1 has exactly two interfaces — `ChatProvider` and
//! `ImageProvider` — with two implementations: BytePlus ModelArk (production)
//! and a local mock (tests and the visible "simulation" mode).
use crate::apilog::{safe_params, ApiLog, Call};
use crate::error::AppError;
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrKind {
    Auth,
    Activation,
    BadRequest,
    RateLimit,
    /// A budgeted gateway refused: the request's maximum cost exceeds what is left.
    Budget,
    /// The request never left this machine.
    Network,
    /// A POST was sent and we do not know whether the provider processed it.
    Uncertain,
    Other,
}

#[derive(Debug, Clone)]
pub struct ProviderError {
    pub kind: ErrKind,
    pub code: Option<String>,
    pub message: String,
    pub status: Option<u16>,
    pub request_id: Option<String>,
}

impl ProviderError {
    pub fn new(kind: ErrKind, message: impl Into<String>) -> Self {
        ProviderError { kind, code: None, message: message.into(), status: None, request_id: None }
    }
    pub fn hint(&self) -> &'static str {
        match self.kind {
            ErrKind::Auth => "The API key was rejected. Check the key and the region in Settings.",
            ErrKind::Activation => "Your key connected, but this account has no access to this model in this region. Activate it in the BytePlus console (Model activation), choose another model, or set a custom Endpoint ID in Settings.",
            ErrKind::BadRequest => "The provider rejected an option or an input. The request was not altered or resent.",
            ErrKind::RateLimit => "The account hit a usage limit. Wait, then retry manually.",
            ErrKind::Budget => "The gateway refused this request: its maximum possible cost is more than the budget that is left. Nothing was generated. Check the balance in Settings, then try a shorter or lower-resolution request.",
            ErrKind::Network => "Could not connect; the request was not sent.",
            ErrKind::Uncertain => "The connection dropped after sending, so the outcome is unknown. The request will not be resent automatically.",
            ErrKind::Other => "Provider error.",
        }
    }
    pub fn to_app(&self, region: &str, model: &str) -> AppError {
        let code = match self.kind {
            ErrKind::Auth => "provider_auth",
            ErrKind::Activation => "provider_activation",
            ErrKind::BadRequest => "provider_bad_request",
            ErrKind::RateLimit => "provider_rate_limit",
            ErrKind::Budget => "provider_budget",
            ErrKind::Network => "provider_network",
            ErrKind::Uncertain => "provider_uncertain",
            ErrKind::Other => "provider_other",
        };
        AppError::new(code, self.display(region, model)).with(json!({
            "providerCode": self.code, "status": self.status, "requestId": self.request_id,
            "region": region, "model": model,
        }))
    }
    pub fn display(&self, region: &str, model: &str) -> String {
        let code = self.code.clone().map(|c| format!(" [{c}]")).unwrap_or_default();
        // BytePlus documents AccessDenied as a permission / whitelist refusal, which is
        // different from a model that simply is not activated (ModelNotOpen).
        let hint = if self.code.as_deref() == Some("AccessDenied") {
            "Your key connected, but BytePlus says this key has no access to this model (a permission or whitelist restriction, not a missing activation). In the BytePlus console, check the permissions and project of the API key or sub-account, or ask BytePlus support to enable this model for your account. You can also choose another model or set a custom Endpoint ID in Settings."
        } else if let Some(h) = self.code.as_deref().and_then(content_hint) {
            h
        } else {
            self.hint()
        };
        format!("{hint} ({model} · {region}){code} {}", self.message)
    }
}

/// Plain explanation for a content-check refusal, by provider code. Also used for
/// a task that fails after it was accepted (the code then arrives with the task).
pub fn content_hint(code: &str) -> Option<&'static str> {
    if code.contains("OutputAudioSensitiveContentDetected") {
        Some("BytePlus’s content check flagged the soundtrack it generated for this video, so the video was withheld. A failed video is not charged. Fix: turn Sound off on this video node and generate again; the request then says generate_audio: false.")
    } else if code.contains("SensitiveContentDetected.PrivacyInformation") {
        // BytePlus refuses realistic human faces it cannot trace to this account (documented for Seedance 2.x).
        Some("BytePlus refused the input image because it shows a realistic human face that was not made inside this account. Nothing was generated and nothing was resent. Faces are accepted only on original, unedited images made by Seedream 5.0 (Lite, Pro or Flash) from text alone, or on the last frame of a Seedance video, from this same account within 30 days. Fix: generate this frame here with an Image generation node set to Seedream 5.0 and no reference image, then connect its result to the video node. Shots without a face are not affected. On a compatible gateway a face is also accepted when the image is in the gateway’s character library: select the image node and pick its library entry under Character library.")
    } else if code.contains("SensitiveContentDetected") {
        Some("BytePlus’s content check refused this request. Nothing was generated and nothing was resent. Change the image or the wording that the message points to and try again.")
    } else {
        None
    }
}

pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub tools: Option<Vec<Value>>,
    pub thinking: Option<String>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct ChatOutcome {
    pub text: String,
    /// False when the stream stopped before the provider signalled the end.
    pub complete: bool,
    /// Model id as reported by the provider's response.
    pub model: String,
    pub usage: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolTurn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// Assistant message exactly as it must be echoed back with the tool results.
    pub raw_message: Value,
    pub model: String,
}

pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub settings: Map<String, Value>,
    /// Ordered reference images: (mime, bytes) of the versions the user picked.
    pub images: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone)]
pub struct ImageResult {
    pub bytes: Vec<u8>,
    pub model: String,
    pub usage: Option<Value>,
}

#[async_trait]
pub trait ChatProvider: Send + Sync {
    async fn chat_stream(&self, req: ChatRequest, on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send)) -> Result<ChatOutcome, ProviderError>;
    /// One non-streamed turn with function tools.
    async fn chat_tools(&self, req: ChatRequest) -> Result<ToolTurn, ProviderError>;
    /// Model ids visible to the key (GET, no generation).
    async fn list_models(&self) -> Result<Vec<String>, ProviderError>;
    /// Budget of a gateway (spent / held / left). Only gateways have one.
    async fn balance(&self) -> Result<Value, ProviderError> {
        Err(ProviderError::new(ErrKind::Other, "this connection has no balance endpoint"))
    }
    /// A gateway's private character library (GET, no generation).
    async fn library(&self) -> Result<Value, ProviderError> {
        Err(ProviderError::new(ErrKind::Other, "this connection has no character library"))
    }
    /// Add one image to the gateway's library. `attested` is the owner's declaration that it
    /// is AI-generated and shows no real person; the gateway refuses the upload without it.
    async fn library_upload(&self, _name: &str, _mime: &str, _bytes: Vec<u8>, _attested: bool) -> Result<Value, ProviderError> {
        Err(ProviderError::new(ErrKind::Other, "this connection has no character library"))
    }
    /// One library entry (GET): its status while BytePlus processes it.
    async fn library_asset(&self, _id: &str) -> Result<Value, ProviderError> {
        Err(ProviderError::new(ErrKind::Other, "this connection has no character library"))
    }
}

#[async_trait]
pub trait ImageProvider: Send + Sync {
    async fn generate(&self, req: ImageRequest) -> Result<ImageResult, ProviderError>;
}

// ───────────────────────── BytePlus ModelArk ─────────────────────────

pub struct ModelArk {
    base: String,
    key: String,
    http: reqwest::Client,
    /// Sent as `max_completion_tokens` on chat requests (budgeted gateways hold the maximum cost).
    max_completion_tokens: Option<u64>,
    balance_url: Option<String>,
    /// Where a gateway lists its private character library.
    assets_url: Option<String>,
    /// Where each request is recorded, and the connection name recorded with it.
    log: Option<Arc<ApiLog>>,
    connection: String,
}

impl ModelArk {
    pub fn new(base: &str, key: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        ModelArk { base: base.trim_end_matches('/').to_string(), key, http, max_completion_tokens: None, balance_url: None, assets_url: None, log: None, connection: String::new() }
    }

    /// A budgeted gateway in front of ModelArk: cap chat output and know where the balance is.
    pub fn gateway(mut self, max_completion_tokens: Option<u64>, balance_url: Option<String>, assets_url: Option<String>) -> Self {
        self.max_completion_tokens = max_completion_tokens;
        self.balance_url = balance_url;
        self.assets_url = assets_url;
        self
    }

    /// Record every request this client sends (never the key or the content).
    pub fn logged(mut self, log: Arc<ApiLog>, connection: &str) -> Self {
        self.log = Some(log);
        self.connection = connection.to_string();
        self
    }

    fn call(&self, purpose: &str, method: &str, url: &str, model: Option<&str>, params: Value, bytes_out: usize) -> Call {
        Call::begin(self.log.clone(), purpose, method, url, &self.connection, model, params, bytes_out)
    }

    /// A JSON body as the bytes that are sent, with its content-free summary for the log.
    fn wire(body: &Value) -> (Vec<u8>, Value) {
        (serde_json::to_vec(body).unwrap_or_default(), safe_params(body))
    }

    fn send_err(e: reqwest::Error) -> ProviderError {
        if e.is_connect() {
            ProviderError::new(ErrKind::Network, "could not open a connection to the server")
        } else {
            ProviderError::new(ErrKind::Uncertain, if e.is_timeout() { "the request timed out" } else { "the connection dropped" })
        }
    }

    async fn http_err(resp: reqwest::Response) -> ProviderError {
        let status = resp.status().as_u16();
        let request_id = ["x-request-id", "x-tt-logid", "x-client-request-id"].iter()
            .find_map(|h| resp.headers().get(*h).and_then(|v| v.to_str().ok()).map(String::from));
        let body = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let code = v["error"]["code"].as_str().map(String::from);
        let raw = v["error"]["message"].as_str().map(String::from).unwrap_or(body);
        let message: String = raw.chars().take(300).collect();
        let c = code.clone().unwrap_or_default();
        let activation = c.contains("NotFound") || c.contains("ModelNotOpen") || c.contains("AccessDenied") || c.contains("NotOpen");
        let kind = match status {
            401 => ErrKind::Auth,
            402 => ErrKind::Budget,
            403 => if activation { ErrKind::Activation } else { ErrKind::Auth },
            404 => ErrKind::Activation,
            400 | 422 => if activation { ErrKind::Activation } else { ErrKind::BadRequest },
            429 => ErrKind::RateLimit,
            _ => ErrKind::Other,
        };
        ProviderError { kind, code, message, status: Some(status), request_id }
    }

    pub fn chat_body(req: &ChatRequest, stream: bool, cap: Option<u64>) -> Value {
        let mut body = json!({"model": req.model, "messages": req.messages, "stream": stream});
        if let Some(n) = cap {
            body["max_completion_tokens"] = json!(n);
        }
        if let Some(t) = &req.tools {
            body["tools"] = json!(t);
        }
        if let Some(t) = &req.thinking {
            body["thinking"] = json!({"type": t});
        }
        body
    }

    /// Body for POST /images/generations. Pure, so tests can pin the wire format.
    pub fn image_body(req: &ImageRequest) -> Value {
        let mut body = json!({"model": req.model, "prompt": req.prompt, "response_format": "b64_json"});
        for (k, v) in &req.settings {
            if k == "optimize_prompt" {
                body["optimize_prompt_options"] = json!({"mode": v});
            } else {
                body[k.as_str()] = v.clone();
            }
        }
        let uris: Vec<String> = req.images.iter()
            .map(|(mime, bytes)| format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
            .collect();
        match uris.len() {
            0 => {}
            1 => body["image"] = json!(uris[0]),
            _ => body["image"] = json!(uris),
        }
        body
    }
}

#[async_trait]
impl ChatProvider for ModelArk {
    async fn chat_stream(&self, req: ChatRequest, on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send)) -> Result<ChatOutcome, ProviderError> {
        let url = format!("{}/chat/completions", self.base);
        let (bytes, params) = Self::wire(&Self::chat_body(&req, true, self.max_completion_tokens));
        let call = self.call("chat", "POST", &url, Some(&req.model), params, bytes.len());
        let r: Result<ChatOutcome, ProviderError> = async {
        let resp = self.http.post(&url)
            .bearer_auth(&self.key)
            .header("content-type", "application/json").body(bytes)
            .send().await.map_err(Self::send_err)?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        let mut out = ChatOutcome { text: String::new(), complete: false, model: req.model.clone(), usage: None };
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = vec![];
        'read: loop {
            if req.cancel.load(Ordering::Relaxed) {
                break;
            }
            let chunk = match tokio::time::timeout(Duration::from_secs(90), stream.next()).await {
                Ok(Some(Ok(c))) => c,
                _ => break, // end of body, transport error, or stalled: keep what we have
            };
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|c| *c == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    out.complete = true;
                    break 'read;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(m) = v["model"].as_str() {
                    out.model = m.to_string();
                }
                if v["usage"].is_object() {
                    out.usage = Some(v["usage"].clone());
                }
                if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
                    if !delta.is_empty() {
                        out.text.push_str(delta);
                        on_delta(delta);
                    }
                }
                if v["choices"][0]["finish_reason"].is_string() {
                    out.complete = true;
                }
            }
        }
        call.usage(out.usage.as_ref());
        if !out.complete { call.partial(); }
        Ok(out)
        }.await;
        call.finish(&r);
        r
    }

    async fn chat_tools(&self, req: ChatRequest) -> Result<ToolTurn, ProviderError> {
        let url = format!("{}/chat/completions", self.base);
        let (bytes, params) = Self::wire(&Self::chat_body(&req, false, self.max_completion_tokens));
        let call = self.call("chat_tools", "POST", &url, Some(&req.model), params, bytes.len());
        let r: Result<ToolTurn, ProviderError> = async {
        let resp = self.http.post(&url)
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(120))
            .header("content-type", "application/json").body(bytes)
            .send().await.map_err(Self::send_err)?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        let v: Value = resp.json().await.map_err(|_| ProviderError::new(ErrKind::Uncertain, "incomplete response"))?;
        call.usage(v.get("usage"));
        let msg = v["choices"][0]["message"].clone();
        if !msg.is_object() {
            return Err(ProviderError::new(ErrKind::Other, "response without a message"));
        }
        let tool_calls = msg["tool_calls"].as_array().map(|a| a.iter().map(|tc| ToolCall {
            id: tc["id"].as_str().unwrap_or_default().to_string(),
            name: tc["function"]["name"].as_str().unwrap_or_default().to_string(),
            arguments: tc["function"]["arguments"].as_str().unwrap_or("{}").to_string(),
        }).collect()).unwrap_or_default();
        Ok(ToolTurn {
            content: msg["content"].as_str().unwrap_or_default().to_string(),
            tool_calls,
            raw_message: msg,
            model: v["model"].as_str().unwrap_or(&req.model).to_string(),
        })
        }.await;
        call.finish(&r);
        r
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = format!("{}/models", self.base);
        let call = self.call("models", "GET", &url, None, json!({}), 0);
        let r: Result<Vec<String>, ProviderError> = async {
        let resp = self.http.get(&url)
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(30))
            .send().await
            .map_err(|_| ProviderError::new(ErrKind::Network, "could not reach the server"))?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        let v: Value = resp.json().await.map_err(|_| ProviderError::new(ErrKind::Other, "unreadable response"))?;
        Ok(v["data"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default())
        }.await;
        call.finish(&r);
        r
    }

    async fn balance(&self) -> Result<Value, ProviderError> {
        let url = self.balance_url.clone().ok_or_else(|| ProviderError::new(ErrKind::Other, "this connection has no balance endpoint"))?;
        let call = self.call("balance", "GET", &url, None, json!({}), 0);
        let r: Result<Value, ProviderError> = async {
        let resp = self.http.get(&url).bearer_auth(&self.key).timeout(Duration::from_secs(30)).send().await
            .map_err(|_| ProviderError::new(ErrKind::Network, "could not reach the gateway"))?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        resp.json().await.map_err(|_| ProviderError::new(ErrKind::Other, "unreadable response"))
        }.await;
        call.finish(&r);
        r
    }

    async fn library(&self) -> Result<Value, ProviderError> {
        let url = self.assets_url.clone().ok_or_else(|| ProviderError::new(ErrKind::Other, "this connection has no character library"))?;
        let call = self.call("library", "GET", &url, None, json!({}), 0);
        let r: Result<Value, ProviderError> = async {
        let resp = self.http.get(&url).bearer_auth(&self.key).timeout(Duration::from_secs(30)).send().await
            .map_err(|_| ProviderError::new(ErrKind::Network, "could not reach the gateway"))?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        resp.json().await.map_err(|_| ProviderError::new(ErrKind::Other, "unreadable response"))
        }.await;
        call.finish(&r);
        r
    }

    async fn library_upload(&self, name: &str, mime: &str, bytes: Vec<u8>, attested: bool) -> Result<Value, ProviderError> {
        let url = self.assets_url.clone().ok_or_else(|| ProviderError::new(ErrKind::Other, "this connection has no character library"))?;
        let url = format!("{url}/upload");
        let call = self.call("library_upload", "POST", &url, None, json!({"mime": mime, "declared_ai_generated": attested}), bytes.len());
        let r: Result<Value, ProviderError> = async {
        let mut rq = self.http.post(&url).query(&[("name", name)]).bearer_auth(&self.key)
            .header("content-type", mime).timeout(Duration::from_secs(60)).body(bytes);
        if attested { rq = rq.header("x-abpp-ai-generated", "true"); }
        let resp = rq.send().await.map_err(Self::send_err)?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        resp.json().await.map_err(|_| ProviderError::new(ErrKind::Uncertain, "the response was cut off"))
        }.await;
        call.finish(&r);
        r
    }

    async fn library_asset(&self, id: &str) -> Result<Value, ProviderError> {
        let url = self.assets_url.clone().ok_or_else(|| ProviderError::new(ErrKind::Other, "this connection has no character library"))?;
        let url = format!("{url}/{id}");
        let call = self.call("library_asset", "GET", &url, None, json!({}), 0);
        let r: Result<Value, ProviderError> = async {
        let resp = self.http.get(&url).bearer_auth(&self.key).timeout(Duration::from_secs(30)).send().await
            .map_err(|_| ProviderError::new(ErrKind::Network, "could not reach the gateway"))?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        resp.json().await.map_err(|_| ProviderError::new(ErrKind::Other, "unreadable response"))
        }.await;
        call.finish(&r);
        r
    }
}

#[async_trait]
impl ImageProvider for ModelArk {
    async fn generate(&self, req: ImageRequest) -> Result<ImageResult, ProviderError> {
        let url = format!("{}/images/generations", self.base);
        let (bytes, params) = Self::wire(&Self::image_body(&req));
        let call = self.call("image", "POST", &url, Some(&req.model), params, bytes.len());
        let r: Result<(Value, String, Option<Value>), ProviderError> = async {
        let resp = self.http.post(&url)
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(300))
            .header("content-type", "application/json").body(bytes)
            .send().await.map_err(Self::send_err)?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        let v: Value = resp.json().await.map_err(|_| ProviderError::new(ErrKind::Uncertain, "the response was cut off"))?;
        call.usage(v.get("usage"));
        let item_err = if v["error"].is_object() { &v["error"] } else { &v["data"][0]["error"] };
        if item_err.is_object() {
            return Err(ProviderError {
                kind: ErrKind::BadRequest,
                code: item_err["code"].as_str().map(String::from),
                message: item_err["message"].as_str().unwrap_or_default().chars().take(300).collect(),
                status: Some(200),
                request_id: None,
            });
        }
        let model = v["model"].as_str().unwrap_or(&req.model).to_string();
        let usage = v["usage"].is_object().then(|| v["usage"].clone());
        Ok((v, model, usage))
        }.await;
        call.finish(&r);
        let (v, model, usage) = r?;
        if let Some(b64) = v["data"][0]["b64_json"].as_str() {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())
                .map_err(|_| ProviderError::new(ErrKind::Other, "could not decode the image"))?;
            return Ok(ImageResult { bytes, model, usage });
        }
        if let Some(url) = v["data"][0]["url"].as_str() {
            // The paid POST already succeeded; fetching the file is a plain GET.
            let call = self.call("download", "GET", url, None, json!({"of": "image"}), 0);
            let got: Result<Vec<u8>, ProviderError> = async {
                let failed = || ProviderError::new(ErrKind::Other, "the image was generated but could not be downloaded from its URL");
                let resp = self.http.get(url).timeout(Duration::from_secs(120)).send().await.map_err(|_| failed())?;
                call.seen(&resp);
                Ok(resp.bytes().await.map_err(|_| failed())?.to_vec())
            }.await;
            call.finish(&got);
            return Ok(ImageResult { bytes: got?, model, usage });
        }
        Err(ProviderError::new(ErrKind::Other, "response without an image"))
    }
}

// ───────────────────────── Mock (no network) ─────────────────────────

/// Deterministic offline provider. Tests can script tool turns and inject failures.
#[derive(Default)]
pub struct MockProvider {
    pub image_calls: AtomicUsize,
    pub chat_calls: AtomicUsize,
    /// Bodies of image requests, as ModelArk would have sent them (without image bytes).
    pub image_log: Mutex<Vec<Value>>,
    /// If the prompt contains the string, fail with this kind.
    pub fail_when: Mutex<Option<(String, ErrKind)>>,
    pub tool_script: Mutex<VecDeque<ToolTurn>>,
    /// Bodies of video task requests (image bytes replaced by a marker).
    pub video_log: Mutex<Vec<Value>>,
    pub video_polls: Mutex<std::collections::HashMap<String, u32>>,
    pub delay_ms: u64,
}

impl MockProvider {
    pub fn tool_turn(calls: Vec<(&str, &str, Value)>, content: &str) -> ToolTurn {
        let tool_calls: Vec<ToolCall> = calls.iter().map(|(id, name, args)| ToolCall {
            id: id.to_string(), name: name.to_string(), arguments: args.to_string(),
        }).collect();
        let raw: Vec<Value> = tool_calls.iter().map(|t| json!({"id": t.id, "type": "function",
            "function": {"name": t.name, "arguments": t.arguments}})).collect();
        let mut raw_message = json!({"role": "assistant", "content": content});
        if !raw.is_empty() {
            raw_message["tool_calls"] = json!(raw);
        }
        ToolTurn { content: content.into(), tool_calls, raw_message, model: "mock-seed".into() }
    }
}

fn last_user_text(messages: &[Value]) -> String {
    messages.iter().rev().find(|m| m["role"] == "user").map(|m| match &m["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }).unwrap_or_default()
}

/// Rule-based stand-in for the Seed agent, so the tool loop can be exercised offline.
fn mock_agent(messages: &[Value]) -> ToolTurn {
    if messages.iter().any(|m| m["role"] == "tool") {
        return MockProvider::tool_turn(vec![], "Done (simulation). Review the change on the canvas; nothing is generated until you press Run.");
    }
    let ctx: Value = messages.iter()
        .filter_map(|m| m["content"].as_str())
        .find_map(|c| c.strip_prefix("CANVAS_CONTEXT\n"))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(Value::Null);
    let text = last_user_text(messages);
    let low = text.to_lowercase();
    let rev = ctx["revision"].as_i64().unwrap_or(0);
    let empty = vec![];
    let nodes = ctx["canvas"]["nodes"].as_array().unwrap_or(&empty);
    let selection: Vec<&str> = ctx["selection"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
    let by_type = |t: &str| -> Vec<&Value> { nodes.iter().filter(|n| n["type"] == t).collect() };
    let selected = |t: &str| -> Option<&Value> { nodes.iter().find(|n| n["type"] == t && selection.contains(&n["id"].as_str().unwrap_or(""))) };

    if (text.contains("احذف") || low.contains("delete") || low.contains("remove")) && !selection.is_empty() {
        let ops: Vec<Value> = selection.iter().map(|id| json!({"op": "removeNode", "nodeId": id})).collect();
        return MockProvider::tool_turn(vec![("mock-call-1", "apply_canvas_patch", json!({"expectedRevision": rev, "operations": ops}))], "");
    }
    if text.contains("دقة") || low.contains("resolution") {
        let target = selected("seedream").or_else(|| by_type("seedream").into_iter()
            .find(|n| n["data"]["title"].as_str().unwrap_or("").contains("مسا") || n["data"]["title"].as_str().unwrap_or("").to_lowercase().contains("evening")));
        if let Some(t) = target {
            let args = json!({"expectedRevision": rev, "operations": [
                {"op": "setNodeData", "nodeId": t["id"], "changes": {"settings": {"size": "2K"}}}]});
            return MockProvider::tool_turn(vec![("mock-call-1", "apply_canvas_patch", args)], "");
        }
    }
    if text.contains("فرع") || text.contains("نسخت") || low.contains("branch") || low.contains("version") || low.contains("variant") {
        let evening = text.contains("مسائ") || low.contains("evening");
        let both = text.contains("نسختين") || text.contains("فرعين") || low.contains("two") || (low.contains("morning") && evening);
        let mut branches = vec![];
        if both || !evening { branches.push(("Morning", "soft morning")); }
        if both || evening { branches.push(("Evening", "warm evening side")); }
        let reference = selected("imageAsset").or_else(|| by_type("imageAsset").into_iter().next());
        let (bx, by) = reference.map(|r| (r["position"]["x"].as_f64().unwrap_or(0.0), r["position"]["y"].as_f64().unwrap_or(0.0)))
            .unwrap_or((0.0, 0.0));
        let max_y = nodes.iter().filter_map(|n| n["position"]["y"].as_f64()).fold(by, f64::max);
        let tag = &uuid::Uuid::new_v4().simple().to_string()[..6];
        let mut ops = vec![];
        for (i, (name, light)) in branches.iter().enumerate() {
            let y = max_y + 260.0 * (i as f64 + 1.0);
            let (p, s, v) = (format!("ag-{tag}-p{i}"), format!("ag-{tag}-s{i}"), format!("ag-{tag}-v{i}"));
            ops.push(json!({"op": "addNode", "id": p, "type": "prompt", "position": {"x": bx, "y": y},
                "data": {"title": format!("{name} light"), "text": format!("Photograph the reference product in {light} light. Keep the logo and the bottle shape exactly as in the reference, square composition.")}}));
            ops.push(json!({"op": "addNode", "id": s, "type": "seedream", "position": {"x": bx + 380.0, "y": y},
                "data": {"title": format!("{name} version")}}));
            ops.push(json!({"op": "addNode", "id": v, "type": "preview", "position": {"x": bx + 760.0, "y": y},
                "data": {"title": format!("{name} result")}}));
            ops.push(json!({"op": "connect", "source": p, "target": s, "targetHandle": "prompt"}));
            if let Some(r) = reference {
                ops.push(json!({"op": "connect", "source": r["id"], "target": s, "targetHandle": "refs"}));
            }
            ops.push(json!({"op": "connect", "source": s, "target": v, "targetHandle": "image"}));
        }
        return MockProvider::tool_turn(vec![("mock-call-1", "apply_canvas_patch", json!({"expectedRevision": rev, "operations": ops}))], "");
    }
    if text.contains("خطة") || text.contains("شغّل") || text.contains("شغل") || low.contains("plan") || low.contains("run") {
        let ids: Vec<Value> = if selection.is_empty() { by_type("seedream").iter().map(|n| n["id"].clone()).collect() }
            else { selection.iter().map(|s| json!(s)).collect() };
        return MockProvider::tool_turn(vec![("mock-call-1", "plan_run", json!({"nodeIds": ids}))], "");
    }
    MockProvider::tool_turn(vec![], "Simulation mode understands a few commands: “make two versions, morning and evening”, “raise the resolution of the evening version”, “show the run plan”, “delete the selection”. With ModelArk, Seed itself chooses the tools.")
}

#[async_trait]
impl ChatProvider for MockProvider {
    async fn chat_stream(&self, req: ChatRequest, on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send)) -> Result<ChatOutcome, ProviderError> {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        let user = last_user_text(&req.messages);
        if let Some((needle, kind)) = self.fail_when.lock().unwrap().clone() {
            if user.contains(&needle) {
                return Err(ProviderError::new(kind, "simulated error"));
            }
        }
        let quote: String = user.chars().take(120).collect();
        let reply = format!("This is a reply from the local simulation provider; nothing was sent to BytePlus. I received: “{quote}”. Choose ModelArk in Settings and Seed itself will answer.");
        let words: Vec<&str> = reply.split_inclusive(' ').collect();
        let cut = if user.contains("انقطاع") || user.to_lowercase().contains("interrupt") { words.len() / 2 } else { words.len() };
        let mut text = String::new();
        for w in &words[..cut] {
            if req.cancel.load(Ordering::Relaxed) {
                return Ok(ChatOutcome { text, complete: false, model: "mock-seed".into(), usage: None });
            }
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
            text.push_str(w);
            on_delta(w);
        }
        Ok(ChatOutcome { text, complete: cut == words.len(), model: "mock-seed".into(), usage: None })
    }

    async fn chat_tools(&self, req: ChatRequest) -> Result<ToolTurn, ProviderError> {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(t) = self.tool_script.lock().unwrap().pop_front() {
            return Ok(t);
        }
        Ok(mock_agent(&req.messages))
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }
}

/// Offline placeholder picture. When references are given it is derived from the
/// first one, so an "edit" visibly depends on the bytes that were sent.
pub fn mock_image(prompt: &str, refs: &[(String, Vec<u8>)], transparent: bool, jpeg: bool) -> Vec<u8> {
    mock_image_sized(prompt, refs, transparent, jpeg, (512, 512))
}

/// Same placeholder with the requested proportions (scaled down; it is only a stand-in).
pub fn mock_image_sized(prompt: &str, refs: &[(String, Vec<u8>)], transparent: bool, jpeg: bool, (width, height): (u32, u32)) -> Vec<u8> {
    let h = Sha256::digest(prompt.as_bytes());
    let tint = [h[0], h[1], h[2]];
    let base = refs.first().and_then(|(_, b)| image::load_from_memory(b).ok())
        .map(|i| i.resize_exact(width, height, image::imageops::FilterType::Triangle).to_rgba8());
    let mut img = image::RgbaImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let t = (x + y) as f32 / (width + height) as f32;
        let mut c = [0u8; 4];
        for i in 0..3 {
            let g = (tint[i] as f32 * (0.55 + 0.45 * t)) as u8;
            c[i] = match &base {
                Some(b) => ((b.get_pixel(x, y)[i] as f32) * 0.62 + g as f32 * 0.38) as u8,
                None => g,
            };
        }
        c[3] = if transparent && base.is_none() && (x < 64 || y < 64 || x + 64 >= width || y + 64 >= height) { 0 } else { 255 };
        // Hatched corner marks the file as a simulation result.
        if x + y < 120 && (x + y) % 16 < 8 {
            c = [255, 255, 255, 255];
        }
        *px = image::Rgba(c);
    }
    let mut out = std::io::Cursor::new(vec![]);
    if jpeg {
        image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg).expect("jpeg");
    } else {
        img.write_to(&mut out, image::ImageFormat::Png).expect("png");
    }
    out.into_inner()
}

#[async_trait]
impl ImageProvider for MockProvider {
    async fn generate(&self, req: ImageRequest) -> Result<ImageResult, ProviderError> {
        self.image_calls.fetch_add(1, Ordering::SeqCst);
        let mut body = ModelArk::image_body(&req);
        if body.get("image").is_some() {
            let hashes: Vec<String> = req.images.iter().map(|(_, b)| hex(&Sha256::digest(b))).collect();
            body["image"] = json!(hashes);
        }
        self.image_log.lock().unwrap().push(body);
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms * 20)).await;
        }
        if let Some((needle, kind)) = self.fail_when.lock().unwrap().clone() {
            if req.prompt.contains(&needle) {
                return Err(ProviderError::new(kind, "simulated error"));
            }
        }
        let transparent = req.settings.get("background").and_then(|v| v.as_str()) == Some("transparent");
        let jpeg = req.settings.get("output_format").and_then(|v| v.as_str()) == Some("jpeg");
        // Honour explicit WIDTHxHEIGHT so the stand-in has the chosen shape.
        let dims = req.settings.get("size").and_then(|v| v.as_str()).and_then(|s| s.split_once('x'))
            .and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?)))
            .map(|(w, h)| { let k = 640.0 / w.max(h); ((w * k) as u32, (h * k) as u32) })
            .unwrap_or((512, 512));
        Ok(ImageResult { bytes: mock_image_sized(&req.prompt, &req.images, transparent, jpeg, dims), model: req.model.clone(), usage: None })
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ───────────────────────── Video (Seedance tasks) ─────────────────────────

pub struct VideoImage {
    /// "first_frame", "last_frame" or "reference_image"
    pub role: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    /// An id in the gateway's character library: sent as `asset://<id>` instead of the file.
    pub library: Option<String>,
}

pub struct VideoRequest {
    pub model: String,
    pub prompt: String,
    pub images: Vec<VideoImage>,
    /// Reference audio clips (mime, bytes): the voice a speaking character is lip-synced to.
    pub audio: Vec<(String, Vec<u8>)>,
    /// Set to produce the final video from an approved draft task.
    pub draft_task_id: Option<String>,
    /// resolution, ratio, duration, draft — exactly as they go on the wire.
    pub params: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct VideoTask {
    /// queued | running | succeeded | failed | expired | cancelled
    pub status: String,
    pub video_url: Option<String>,
    pub error: Option<String>,
    pub usage: Option<Value>,
}

#[async_trait]
pub trait VideoProvider: Send + Sync {
    /// POST: creates the task and returns its id. Never retried automatically.
    async fn create_video(&self, req: &VideoRequest) -> Result<String, ProviderError>;
    /// GET: safe to repeat.
    async fn get_video(&self, task_id: &str) -> Result<VideoTask, ProviderError>;
    async fn fetch_video(&self, url: &str) -> Result<Vec<u8>, ProviderError>;
}

impl ModelArk {
    /// Body for POST /contents/generations/tasks. Pure, so tests can pin the wire format.
    pub fn video_body(req: &VideoRequest) -> Value {
        let mut content = vec![];
        if let Some(id) = &req.draft_task_id {
            content.push(json!({"type": "draft_task", "draft_task": {"id": id}}));
        } else {
            if !req.prompt.trim().is_empty() {
                content.push(json!({"type": "text", "text": req.prompt}));
            }
            for img in &req.images {
                let url = match &img.library {
                    Some(id) => format!("asset://{id}"),
                    None => format!("data:{};base64,{}", img.mime, base64::engine::general_purpose::STANDARD.encode(&img.bytes)),
                };
                content.push(json!({"type": "image_url", "image_url": {"url": url}, "role": img.role}));
            }
            for (mime, bytes) in &req.audio {
                let url = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
                content.push(json!({"type": "audio_url", "audio_url": {"url": url}, "role": "reference_audio"}));
            }
        }
        let mut body = json!({"model": req.model, "content": content});
        for (k, v) in req.params.iter().filter(|(k, _)| !k.starts_with('_')) {
            body[k.as_str()] = v.clone();
        }
        body
    }
}

#[async_trait]
impl VideoProvider for ModelArk {
    async fn create_video(&self, req: &VideoRequest) -> Result<String, ProviderError> {
        let url = format!("{}/contents/generations/tasks", self.base);
        let (bytes, params) = Self::wire(&Self::video_body(req));
        let call = self.call("video_create", "POST", &url, Some(&req.model), params, bytes.len());
        let r: Result<String, ProviderError> = async {
        let resp = self.http.post(&url)
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(120))
            .header("content-type", "application/json").body(bytes)
            .send().await.map_err(Self::send_err)?;
        call.seen(&resp);
        if !resp.status().is_success() {
            let mut e = Self::http_err(resp).await;
            // A 5xx without a provider error code came from a proxy in between (e.g. Cloudflare 520):
            // the request may or may not have reached BytePlus, so its outcome is unknown.
            if e.status.map(|s| s >= 500).unwrap_or(false) && e.code.is_none() {
                e.kind = ErrKind::Uncertain;
                e.message = format!("The gateway answered HTTP {} without a result, so it is not known whether a video task was created. Check the gateway balance before sending again. {}", e.status.unwrap_or(0), e.message);
            }
            return Err(e);
        }
        let v: Value = resp.json().await.map_err(|_| ProviderError::new(ErrKind::Uncertain, "the response was cut off"))?;
        let id = v["id"].as_str().map(String::from).ok_or_else(|| ProviderError::new(ErrKind::Uncertain, "the response had no task id"))?;
        call.task(&id);
        Ok(id)
        }.await;
        call.finish(&r);
        r
    }

    async fn get_video(&self, task_id: &str) -> Result<VideoTask, ProviderError> {
        let url = format!("{}/contents/generations/tasks/{task_id}", self.base);
        let call = self.call("video_status", "GET", &url, None, json!({}), 0);
        call.task(task_id);
        let r: Result<VideoTask, ProviderError> = async {
        let resp = self.http.get(&url)
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(30))
            .send().await
            .map_err(|_| ProviderError::new(ErrKind::Network, "could not reach the server"))?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(Self::http_err(resp).await);
        }
        let v: Value = resp.json().await.map_err(|_| ProviderError::new(ErrKind::Network, "unreadable response"))?;
        call.usage(v.get("usage"));
        call.info(v["status"].as_str().unwrap_or(""));
        let error = v["error"]["message"].as_str().map(|m| {
            let code = v["error"]["code"].as_str().unwrap_or("");
            format!("[{code}] {}", m.chars().take(300).collect::<String>())
        });
        Ok(VideoTask {
            status: v["status"].as_str().unwrap_or("running").to_string(),
            video_url: v["content"]["video_url"].as_str().map(String::from),
            error,
            usage: v["usage"].is_object().then(|| v["usage"].clone()),
        })
        }.await;
        call.finish(&r);
        r
    }

    async fn fetch_video(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
        let err = || ProviderError::new(ErrKind::Network, "the video is ready but could not be downloaded yet");
        let call = self.call("download", "GET", url, None, json!({"of": "video"}), 0);
        let r: Result<Vec<u8>, ProviderError> = async {
        let resp = self.http.get(url).timeout(Duration::from_secs(600)).send().await.map_err(|_| err())?;
        call.seen(&resp);
        if !resp.status().is_success() {
            return Err(err());
        }
        Ok(resp.bytes().await.map_err(|_| err())?.to_vec())
        }.await;
        call.finish(&r);
        r
    }
}

/// A two-second test pattern, so the simulation shows a real, playable file.
pub const MOCK_VIDEO: &[u8] = include_bytes!("../assets/mock-video.mp4");

#[async_trait]
impl VideoProvider for MockProvider {
    async fn create_video(&self, req: &VideoRequest) -> Result<String, ProviderError> {
        let mut body = ModelArk::video_body(req);
        // Keep the log small: record roles, not image bytes.
        if let Some(items) = body["content"].as_array_mut() {
            for item in items.iter_mut().filter(|i| i["type"] == "image_url") {
                item["image_url"] = json!("(bytes)");
            }
        }
        if let Some((needle, kind)) = self.fail_when.lock().unwrap().clone() {
            if req.prompt.contains(&needle) {
                return Err(ProviderError::new(kind, "simulated error"));
            }
        }
        let mut log = self.video_log.lock().unwrap();
        log.push(body);
        Ok(format!("mock-task-{}", log.len()))
    }

    async fn get_video(&self, task_id: &str) -> Result<VideoTask, ProviderError> {
        let mut polls = self.video_polls.lock().unwrap();
        let n = polls.entry(task_id.to_string()).or_insert(0);
        *n += 1;
        let done = *n >= 2;
        Ok(VideoTask {
            status: if done { "succeeded" } else { "running" }.into(),
            video_url: done.then(|| "mock://video".to_string()),
            error: None,
            usage: None,
        })
    }

    async fn fetch_video(&self, _url: &str) -> Result<Vec<u8>, ProviderError> {
        Ok(MOCK_VIDEO.to_vec())
    }
}
