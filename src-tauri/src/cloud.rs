//! Team: sign-in, people and shared sessions on the owner's Supabase project.
//!
//! What is kept where:
//! - the project address and its publishable key are plain settings (both are public by design;
//!   the database's own rules decide what a signed-in person may read or change);
//! - the sign-in session (access and refresh token) lives in the Keychain and is read only when
//!   a team request is actually made — never when the app opens;
//! - a password passes through once, to Supabase, and is not stored or logged;
//! - the gateway key and the project's secret service key never come near this module.
//! Nothing here is retried on its own, except refreshing an expired session.
use crate::core::{now_ms, Core};
use crate::error::{AppError, Res};
use serde_json::{json, Value};

const SESSION: &str = "cloud:session";
/// One refresh at a time: Supabase replaces the refresh token each time it is used.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn err(code: &str, msg: impl Into<String>) -> AppError { AppError::new(code, msg) }
fn http() -> Res<reqwest::Client> {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|e| err("cloud", e.to_string()))
}
fn config(core: &Core) -> Res<(String, String)> {
    match (core.setting("cloud_url"), core.setting("cloud_key")) {
        (Some(u), Some(k)) if !u.is_empty() && !k.is_empty() => Ok((u, k)),
        _ => Err(err("cloud_not_set", "The team connection is not set up yet. Add the project address in Settings → Team.")),
    }
}
fn user(core: &Core) -> Option<Value> {
    core.setting("cloud_user").and_then(|s| serde_json::from_str(&s).ok()).filter(|v: &Value| v["id"].is_string())
}
/// An id that is safe to put in a query: Supabase ids are UUIDs.
fn id(v: &str) -> Res<&str> {
    if v.len() == 36 && v.chars().all(|c| c.is_ascii_hexdigit() || c == '-') { Ok(v) } else { Err(err("bad_args", "That id is not valid.")) }
}

/// What the app shows without asking anyone: no network, no Keychain.
pub fn state(core: &Core) -> Value {
    json!({"configured": config(core).is_ok(), "url": core.setting("cloud_url").unwrap_or_default(), "user": user(core)})
}

/// The message of a Supabase error, whichever of its services answered.
fn message(status: u16, body: &str) -> AppError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let text = [&v["error"]["message"], &v["message"], &v["msg"], &v["error_description"], &v["error"]]
        .into_iter().find_map(|x| x.as_str()).unwrap_or("").to_string();
    let code = v["error"]["code"].as_str().or_else(|| v["error_code"].as_str()).or_else(|| v["code"].as_str()).unwrap_or("");
    if text.contains("seat_limit") { return err("seat_limit", "This shared session is full: it holds at most 8 people. Remove someone first."); }
    if text.contains("owner_seat") { return err("owner_seat", "The owner cannot be removed from a session. Delete the session instead."); }
    if code == "invalid_credentials" || text.contains("Invalid login credentials") { return err("bad_login", "The email or password is not right."); }
    if code == "user_banned" || text.to_lowercase().contains("banned") { return err("disabled", "This account has been disabled by an admin."); }
    if status == 401 { return err("signed_out", "Your sign-in has ended. Sign in again."); }
    if status == 403 || code == "42501" { return err("forbidden", if text.is_empty() { "You are not allowed to do that.".into() } else { text }); }
    if code == "23505" { return err("duplicate", "That is already there."); }
    if status == 404 && text.is_empty() { return err("cloud", "The team server has no such address. Was the admin function deployed?"); }
    err(if code.is_empty() { "cloud" } else { code }, if text.is_empty() { format!("The team server answered HTTP {status}.") } else { text })
}

async fn read(resp: reqwest::Response) -> Res<Value> {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) { return Err(message(status, &body)); }
    Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
}
fn unreachable(e: reqwest::Error) -> AppError {
    err("cloud_offline", format!("The team server could not be reached{}. Nothing was changed.", if e.is_timeout() { " in time" } else { "" }))
}

fn save_session(core: &Core, v: &Value) -> Res<()> {
    let (access, refresh) = (v["access_token"].as_str().unwrap_or(""), v["refresh_token"].as_str().unwrap_or(""));
    if access.is_empty() || refresh.is_empty() { return Err(err("cloud", "The team server did not return a session.")); }
    let exp = now_ms() + v["expires_in"].as_i64().unwrap_or(3600) * 1000;
    core.secrets.set(SESSION, &json!({"access": access, "refresh": refresh, "exp": exp}).to_string())
}
fn forget(core: &Core) {
    let _ = core.secrets.delete(SESSION);
    let _ = core.set_setting("cloud_user", "");
}

/// A valid access token, refreshed when it is about to expire.
async fn token(core: &Core) -> Res<String> {
    let (url, key) = config(core)?;
    let _one = REFRESH.lock().await;
    let signed_out = || err("signed_out", "Sign in to your team account first.");
    let s: Value = core.secrets.get(SESSION)?.and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(signed_out)?;
    if s["exp"].as_i64().unwrap_or(0) - now_ms() > 60_000 {
        return s["access"].as_str().map(String::from).ok_or_else(signed_out);
    }
    let resp = http()?.post(format!("{url}/auth/v1/token?grant_type=refresh_token")).header("apikey", &key)
        .json(&json!({"refresh_token": s["refresh"]})).send().await.map_err(unreachable)?;
    match read(resp).await {
        Ok(v) => { save_session(core, &v)?; Ok(v["access_token"].as_str().unwrap_or("").to_string()) }
        Err(e) if e.code == "cloud_offline" => Err(e),
        Err(_) => { forget(core); Err(err("signed_out", "Your sign-in has ended. Sign in again.")) }
    }
}

/// One request as the signed-in person.
async fn call(core: &Core, method: reqwest::Method, path: &str, body: Option<Value>) -> Res<Value> {
    let (url, key) = config(core)?;
    let bearer = token(core).await?;
    let mut rq = http()?.request(method, format!("{url}{path}")).header("apikey", &key).bearer_auth(bearer).header("prefer", "return=representation");
    if let Some(b) = body { rq = rq.json(&b); }
    let out = read(rq.send().await.map_err(unreachable)?).await;
    if matches!(&out, Err(e) if e.code == "signed_out") { forget(core); }
    out
}

/// Save the project address and publishable key after checking that they answer.
pub async fn configure(core: &Core, url: &str, key: &str) -> Res<Value> {
    let (url, key) = (url.trim().trim_end_matches('/').to_string(), key.trim().to_string());
    if url.is_empty() && key.is_empty() {
        forget(core);
        core.set_setting("cloud_url", "")?;
        core.set_setting("cloud_key", "")?;
        return Ok(state(core));
    }
    let local = url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost");
    if !(url.starts_with("https://") || local) { return Err(err("bad_url", "The project address starts with https:// — for example https://abcd1234.supabase.co")); }
    if key.len() < 20 || key.contains(char::is_whitespace) { return Err(err("bad_key", "Paste the publishable (anon) key from Project Settings → API.")); }
    // The secret service key must never be put in an app. Its JWT says so in its payload.
    let role = key.split('.').nth(1).and_then(|p| base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["role"].as_str().map(String::from));
    if role.as_deref() == Some("service_role") || key.starts_with("sb_secret_") {
        return Err(err("secret_key", "That is the secret service key. It was not saved. Never put it in an app: paste the publishable (anon) key instead, and consider replacing the secret key in Supabase, because it has now been typed here."));
    }
    let resp = http()?.get(format!("{url}/auth/v1/settings")).header("apikey", &key).send().await.map_err(unreachable)?;
    read(resp).await.map_err(|e| err("cloud_check", format!("The project did not accept that address and key. {}", e.message)))?;
    if core.setting("cloud_url").as_deref() != Some(url.as_str()) { forget(core); }
    core.set_setting("cloud_url", &url)?;
    core.set_setting("cloud_key", &key)?;
    Ok(state(core))
}

async fn my_profile(core: &Core, uid: &str) -> Res<Value> {
    let rows = call(core, reqwest::Method::GET, &format!("/rest/v1/profiles?id=eq.{}&select=*", id(uid)?), None).await?;
    let p = rows.get(0).cloned().ok_or_else(|| err("no_profile", "This account has no profile on the team server. Ask an admin to check that the team SQL was run."))?;
    if p["disabled"] == true { forget(core); return Err(err("disabled", "This account has been disabled by an admin.")); }
    core.set_setting("cloud_user", &json!({"id": p["id"], "email": p["email"], "name": p["name"], "role": p["role"]}).to_string())?;
    Ok(p)
}

pub async fn sign_in(core: &Core, email: &str, password: &str) -> Res<Value> {
    let (url, key) = config(core)?;
    if email.trim().is_empty() || password.is_empty() { return Err(err("bad_login", "Enter your email and password.")); }
    let resp = http()?.post(format!("{url}/auth/v1/token?grant_type=password")).header("apikey", &key)
        .json(&json!({"email": email.trim(), "password": password})).send().await.map_err(unreachable)?;
    let v = read(resp).await?;
    save_session(core, &v)?;
    let uid = v["user"]["id"].as_str().unwrap_or("").to_string();
    if let Err(e) = my_profile(core, &uid).await { forget(core); return Err(e); }
    Ok(state(core))
}

pub async fn sign_out(core: &Core) -> Res<Value> {
    // Best effort: the session is forgotten on this Mac whether or not the server hears about it.
    if let (Ok((url, key)), Ok(Some(s))) = (config(core), core.secrets.get(SESSION)) {
        if let Some(access) = serde_json::from_str::<Value>(&s).ok().and_then(|v| v["access"].as_str().map(String::from)) {
            let _ = http()?.post(format!("{url}/auth/v1/logout?scope=local")).header("apikey", &key).bearer_auth(access).send().await;
        }
    }
    forget(core);
    Ok(state(core))
}

pub async fn change_password(core: &Core, password: &str) -> Res<Value> {
    if password.chars().count() < 10 { return Err(err("weak_password", "Use at least 10 characters.")); }
    call(core, reqwest::Method::PUT, "/auth/v1/user", Some(json!({"password": password}))).await?;
    Ok(json!({"ok": true}))
}

pub async fn rename_me(core: &Core, name: &str) -> Res<Value> {
    let me = user(core).ok_or_else(|| err("signed_out", "Sign in to your team account first."))?;
    let uid = me["id"].as_str().unwrap_or("").to_string();
    let name: String = name.trim().chars().take(60).collect();
    call(core, reqwest::Method::PATCH, &format!("/rest/v1/profiles?id=eq.{}", id(&uid)?), Some(json!({"name": name}))).await?;
    my_profile(core, &uid).await?;
    Ok(state(core))
}

/// Everyone with an account, and this person's own profile refreshed (a role can change).
pub async fn people(core: &Core) -> Res<Value> {
    let me = user(core).ok_or_else(|| err("signed_out", "Sign in to your team account first."))?;
    let users = call(core, reqwest::Method::GET, "/rest/v1/profiles?select=*&order=created_at.asc", None).await?;
    my_profile(core, me["id"].as_str().unwrap_or("")).await?;
    Ok(json!({"users": users, "state": state(core)}))
}

/// Shared sessions this person may see, each with its members.
pub async fn sessions(core: &Core) -> Res<Value> {
    call(core, reqwest::Method::GET, "/rest/v1/workspaces?select=id,name,owner,created_at,workspace_members(user_id,role,added_at)&order=created_at.asc", None).await
}

pub async fn session_create(core: &Core, name: &str) -> Res<Value> {
    let me = user(core).ok_or_else(|| err("signed_out", "Sign in to your team account first."))?;
    let name: String = name.trim().chars().take(80).collect();
    if name.is_empty() { return Err(err("bad_args", "Give the shared session a name.")); }
    call(core, reqwest::Method::POST, "/rest/v1/workspaces", Some(json!({"name": name, "owner": me["id"]}))).await?;
    sessions(core).await
}

pub async fn session_rename(core: &Core, session: &str, name: &str) -> Res<Value> {
    let name: String = name.trim().chars().take(80).collect();
    if name.is_empty() { return Err(err("bad_args", "Give the shared session a name.")); }
    call(core, reqwest::Method::PATCH, &format!("/rest/v1/workspaces?id=eq.{}", id(session)?), Some(json!({"name": name}))).await?;
    sessions(core).await
}

pub async fn session_delete(core: &Core, session: &str) -> Res<Value> {
    let gone = call(core, reqwest::Method::DELETE, &format!("/rest/v1/workspaces?id=eq.{}", id(session)?), None).await?;
    if gone.as_array().map(|a| a.is_empty()).unwrap_or(false) { return Err(err("forbidden", "Only the session’s owner or an admin can delete it.")); }
    sessions(core).await
}

pub async fn member_add(core: &Core, session: &str, person: &str) -> Res<Value> {
    call(core, reqwest::Method::POST, "/rest/v1/workspace_members", Some(json!({"workspace_id": id(session)?, "user_id": id(person)?, "role": "member"}))).await?;
    sessions(core).await
}

pub async fn member_remove(core: &Core, session: &str, person: &str) -> Res<Value> {
    let gone = call(core, reqwest::Method::DELETE, &format!("/rest/v1/workspace_members?workspace_id=eq.{}&user_id=eq.{}", id(session)?, id(person)?), None).await?;
    if gone.as_array().map(|a| a.is_empty()).unwrap_or(false) { return Err(err("forbidden", "Only the session’s owner or an admin can remove a member.")); }
    sessions(core).await
}

/// Account changes go through the project's admin function, which checks that the caller is an admin.
pub async fn admin(core: &Core, mut payload: Value) -> Res<Value> {
    const ACTIONS: [&str; 5] = ["create_user", "set_password", "set_disabled", "set_role", "rename"];
    if !ACTIONS.contains(&payload["action"].as_str().unwrap_or("")) { return Err(err("bad_args", "Unknown admin action.")); }
    if let Some(o) = payload.as_object_mut() { o.retain(|k, _| ["action", "userId", "email", "name", "password", "role", "disabled"].contains(&k.as_str())); }
    call(core, reqwest::Method::POST, "/functions/v1/admin", Some(payload)).await
}
