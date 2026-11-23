//! Keyless integration tests: everything here runs against the mock provider
//! or a local fake HTTP server. No BytePlus account is ever contacted.
use seed_studio_lib::api::dispatch;
use seed_studio_lib::core::{Core, NullEmitter};
use seed_studio_lib::provider::{ErrKind, MockProvider};
use seed_studio_lib::secrets::MemoryStore;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("seedstudio-test-{}", uuid::Uuid::new_v4().simple()))
}
fn open(dir: &PathBuf) -> Core {
    Core::open(dir.clone(), Arc::new(NullEmitter), Arc::new(MemoryStore::default()), 0).unwrap()
}
async fn call(core: &Core, cmd: &str, args: Value) -> Value {
    dispatch(core, cmd, args).await.unwrap_or_else(|e| panic!("{cmd} failed: {e}"))
}
async fn fail(core: &Core, cmd: &str, args: Value) -> String {
    dispatch(core, cmd, args).await.expect_err("expected an error").code
}
async fn import_fixture(core: &Core, project: &str) {
    use base64::Engine as _;
    let pattern = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([(x * 4) as u8, (y * 4) as u8, 96]));
    let mut png = std::io::Cursor::new(Vec::new());
    pattern.write_to(&mut png, image::ImageFormat::Png).unwrap();
    call(core, "canvas_import", json!({
        "projectId": project,
        "json": include_str!("support/canvas.json"),
        "files": {"assets/reference.png": base64::engine::general_purpose::STANDARD.encode(png.into_inner())}
    })).await;
}
/// Gateway compatibility is tested against loopback only; no private service is shipped.
fn with_test_gateway(mut core: Core, base: &str) -> Core {
    assert!(base.starts_with("http://127.0.0.1:"));
    core.catalog.regions.insert("test-gateway".into(), base.into());
    core.catalog.raw["gateways"]["test-gateway"] = json!({
        "label": "Test gateway", "balanceUrl": base.replace("/api/v3", "/abpp/balance"),
        "assetsUrl": base.replace("/api/v3", "/abpp/assets"), "maxCompletionTokens": 4096,
        "defaultVideoDuration": 5, "listsModels": false, "acceptsEndpointIds": false
    });
    core
}

#[tokio::test]
async fn first_launch_is_empty_and_only_the_user_creates_content() {
    let dir = temp_dir();
    let core = open(&dir);
    let state = call(&core, "app_state", json!({})).await;
    assert!(state["hasKey"].as_object().unwrap().values().all(|v| v == false));
    assert!(state["lastProjectId"].is_null());
    assert_eq!(state["gateways"], json!({}));
    assert_eq!(call(&core, "projects_list", json!({})).await, json!([]));
    let team = seed_studio_lib::cloud::state(&core);
    assert_eq!(team["configured"], false);
    assert!(team["user"].is_null());
    assert_eq!(fail(&core, "project_create", json!({"name": "  "})).await, "bad_args");
    let p = call(&core, "project_create", json!({"name": "Test workspace"})).await["id"].as_str().unwrap().to_string();
    let c = canvas(&core, &p).await;
    assert_eq!(nodes(&c), 0);
    assert_eq!(c["graph"]["edges"], json!([]));
    assert_eq!(call(&core, "assets_list", json!({"projectId": p})).await, json!([]));
    call(&core, "setting_set", json!({"key": "last_project", "value": p})).await;
    call(&core, "project_delete", json!({"projectId": p})).await;
    drop(core);
    let reopened = open(&dir);
    assert_eq!(call(&reopened, "projects_list", json!({})).await, json!([]));
    assert!(call(&reopened, "app_state", json!({})).await["lastProjectId"].is_null());
}

/// Synthetic fixture, created only by tests; no example projects or media ship in the app.
async fn fixture_project() -> (Core, String, PathBuf) {
    let dir = temp_dir();
    let core = open(&dir);
    call(&core, "setting_set", json!({"key": "provider_mode", "value": "mock"})).await;
    let p = call(&core, "project_create", json!({"name": "Test project"})).await["id"].as_str().unwrap().to_string();
    import_fixture(&core, &p).await;
    (core, p, dir)
}
async fn canvas(core: &Core, p: &str) -> Value {
    call(core, "canvas_get", json!({"projectId": p})).await
}
fn nodes(c: &Value) -> usize {
    c["graph"]["nodes"].as_array().unwrap().len()
}
async fn patch(core: &Core, p: &str, ops: Value) -> Result<Value, seed_studio_lib::error::AppError> {
    let rev = canvas(core, p).await["revision"].clone();
    dispatch(core, "canvas_patch", json!({"projectId": p, "expectedRevision": rev, "operations": ops})).await
}
async fn plan(core: &Core, p: &str, ids: Value, force: bool) -> Value {
    call(core, "plan_run", json!({"projectId": p, "nodeIds": ids, "forceNew": force})).await
}

#[tokio::test]
async fn projects_are_isolated_and_survive_restart() {
    let dir = temp_dir();
    let (a, b);
    {
        let core = open(&dir);
        a = call(&core, "project_create", json!({"name": "أ"})).await["id"].as_str().unwrap().to_string();
        b = call(&core, "project_create", json!({"name": "ب"})).await["id"].as_str().unwrap().to_string();
        import_fixture(&core, &a).await;
        call(&core, "canvas_viewport", json!({"projectId": a, "viewport": {"x": 12, "y": -30, "zoom": 1.25}})).await;
        call(&core, "project_update", json!({"projectId": a, "instructions": "العبوة زجاجية"})).await;
        call(&core, "setting_set", json!({"key": "theme", "value": "dark"})).await;
    }
    let core = open(&dir);
    assert_eq!(call(&core, "app_state", json!({})).await["theme"], "dark");
    let ca = canvas(&core, &a).await;
    assert_eq!(nodes(&ca), 7);
    assert_eq!(ca["graph"]["edges"].as_array().unwrap().len(), 6);
    assert_eq!(ca["viewport"]["zoom"], 1.25);
    assert_eq!(nodes(&canvas(&core, &b).await), 0);
    assert_eq!(call(&core, "assets_list", json!({"projectId": a})).await.as_array().unwrap().len(), 1);
    assert_eq!(call(&core, "assets_list", json!({"projectId": b})).await.as_array().unwrap().len(), 0);
    let projects = call(&core, "projects_list", json!({})).await;
    assert!(projects.as_array().unwrap().iter().any(|p| p["instructions"] == "العبوة زجاجية"));
    // The app opened and worked with no key at all.
    assert_eq!(call(&core, "app_state", json!({})).await["hasKey"]["ap-southeast-1"], false);
}

#[tokio::test]
async fn validator_rejects_bad_wiring_atomically() {
    let (core, p, _) = fixture_project().await;
    let rev = canvas(&core, &p).await["revision"].clone();
    // image output → text input
    let e = patch(&core, &p, json!([{"op": "connect", "source": "reference", "target": "day-image", "targetHandle": "prompt"}])).await.unwrap_err();
    assert_eq!(e.code, "invalid_patch");
    // second text into a single-input port
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "night-prompt", "target": "day-image", "targetHandle": "prompt"}])).await.is_err());
    // cycle: day-image → night-image(refs) → day-image(refs)
    let e = patch(&core, &p, json!([
        {"op": "connect", "source": "day-image", "target": "night-image", "targetHandle": "refs"},
        {"op": "connect", "source": "night-image", "target": "day-image", "targetHandle": "refs"}
    ])).await.unwrap_err();
    assert_eq!(e.detail.unwrap()["opIndex"], 1);
    // asset from another project
    let other = call(&core, "project_create", json!({"name": "آخر"})).await["id"].as_str().unwrap().to_string();
    import_fixture(&core, &other).await;
    let foreign = call(&core, "assets_list", json!({"projectId": other})).await[0]["id"].clone();
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "reference", "changes": {"assetId": foreign}}])).await.is_err());
    // unknown option value and unknown field
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"settings": {"size": "8K"}}}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"settings": {"steps": "30"}}}])).await.is_err());
    // nothing above changed the canvas
    let c = canvas(&core, &p).await;
    assert_eq!(c["revision"], rev);
    assert_eq!(c["graph"]["edges"].as_array().unwrap().len(), 6);
    // stale revision from the user's side is a conflict
    assert_eq!(fail(&core, "canvas_patch", json!({"projectId": p, "expectedRevision": 0, "operations": [{"op": "removeNode", "nodeId": "reference"}]})).await, "conflict");
}

#[tokio::test]
async fn model_switch_explains_option_changes() {
    let (core, p, _) = fixture_project().await;
    let r = patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"modelKey": "seedream-5.0-lite"}}])).await.unwrap();
    let node = r["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "day-image").unwrap().clone();
    assert_eq!(node["data"]["settings"]["size"], "2K");
    assert!(node["data"]["settings"].get("background").is_none(), "Lite never sends background");
    assert_eq!(r["notes"].as_array().unwrap().len(), 2);
    // "Flash" is a model name, not a prompt-optimisation mode.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"modelKey": "seedream-5.0-flash"}}])).await.unwrap();
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"optimize_prompt": "fast"}}}])).await.is_err());
    // transparent needs PNG output
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"background": "transparent", "output_format": "jpeg"}}}])).await.is_err());
    // Flash transparency needs exactly one PNG reference with alpha: the example reference decides.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"background": "transparent"}}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    let action = pl["items"][0]["action"].as_str().unwrap();
    assert!(action == "new" || action == "blocked");
}

#[tokio::test]
async fn import_cannot_escape_its_folder() {
    let (core, p, _) = fixture_project().await;
    let doc = json!({"schemaVersion": 1, "assets": [{"id": "x", "relativePath": "../../etc/passwd", "mimeType": "image/png"}], "nodes": [], "edges": []});
    assert_eq!(fail(&core, "canvas_import", json!({"projectId": p, "json": doc.to_string(), "files": {}})).await, "bad_import");
    assert_eq!(nodes(&canvas(&core, &p).await), 7);
}

#[tokio::test]
async fn run_plan_reuse_and_double_click() {
    let (core, p, _) = fixture_project().await;
    let pl = plan(&core, &p, json!(["day-preview", "night-preview"]), false).await;
    assert_eq!(pl["newImages"], 2);
    assert_eq!(pl["newTexts"], 0);
    assert_eq!(pl["sharedRefs"], true);
    assert_eq!(pl["byModel"][0]["label"], "Seedream 5.5");
    assert_eq!(pl["byModel"][0]["count"], 2);
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 0, "planning never generates");

    let id = pl["id"].as_str().unwrap();
    let (first, second) = tokio::join!(
        dispatch(&core, "run_execute", json!({"planId": id})),
        dispatch(&core, "run_execute", json!({"planId": id}))
    );
    assert!(first.is_ok() != second.is_ok(), "exactly one of two clicks runs");
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 2);

    // Wire format of the recorded request.
    let reference_sha = {
        let assets = call(&core, "assets_list", json!({"projectId": p})).await;
        let r = assets.as_array().unwrap().iter().find(|a| a["kind"] == "reference").unwrap()["id"].as_str().unwrap().to_string();
        core.asset_bytes(&r).unwrap().2
    };
    let body = core.mock.image_log.lock().unwrap()[0].clone();
    assert_eq!(body["model"], "dola-seedream-5-5-261028");
    assert_eq!(body["size"], "1.5K");
    assert_eq!(body["output_format"], "png");
    assert_eq!(body["background"], "opaque");
    assert_eq!(body["response_format"], "b64_json");
    assert_eq!(body["image"], json!([reference_sha]));
    assert!(body.get("aspect_ratio").is_none() && body.get("seed").is_none());

    let c = canvas(&core, &p).await;
    assert_eq!(c["results"]["day-image"]["status"], "succeeded");
    assert_eq!(c["results"]["day-image"]["provider"], "mock", "simulation results are labelled, never 'tried'");
    let generated: Vec<Value> = call(&core, "assets_list", json!({"projectId": p})).await.as_array().unwrap().iter().filter(|a| a["kind"] == "generated").cloned().collect();
    assert_eq!(generated.len(), 2);
    assert_eq!(generated[0]["parentAssetIds"].as_array().unwrap().len(), 1);

    // Same inputs → nothing new.
    let again = plan(&core, &p, json!(["day-image", "night-image"]), false).await;
    assert_eq!(again["newImages"], 0);
    assert_eq!(again["reused"], 2);
    // Moving a node does not change results.
    patch(&core, &p, json!([{"op": "moveNode", "nodeId": "night-image", "position": {"x": 500, "y": 500}}])).await.unwrap();
    assert_eq!(plan(&core, &p, json!(["day-image", "night-image"]), false).await["newImages"], 0);
    // Change the evening prompt only → only the evening branch reruns.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-prompt", "changes": {"text": "blue evening light"}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["day-image", "night-image"]), false).await;
    assert_eq!(pl["newImages"], 1);
    let fresh: Vec<&Value> = pl["items"].as_array().unwrap().iter().filter(|i| i["action"] == "new").collect();
    assert_eq!(fresh[0]["nodeId"], "night-image");
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 3);
    // "New version" regenerates on purpose and keeps both.
    let pl = plan(&core, &p, json!(["day-image"]), true).await;
    assert_eq!(pl["newImages"], 1);
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(canvas(&core, &p).await["results"]["day-image"]["versions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn stale_plan_is_refreshed_not_run() {
    let (core, p, _) = fixture_project().await;
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"settings": {"size": "2K"}}}])).await.unwrap();
    let r = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(r["stale"], true);
    assert_eq!(r["plan"]["items"][0]["settings"]["size"], "2K");
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 0);
    // Missing prompt blocks at run time and names the node.
    patch(&core, &p, json!([{"op": "disconnect", "source": "night-prompt", "target": "night-image"}])).await.unwrap();
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    assert_eq!(pl["blocked"], 1);
    let e = dispatch(&core, "run_execute", json!({"planId": pl["id"]})).await.unwrap_err();
    assert_eq!(e.detail.unwrap()["nodeId"], "night-image");
}

#[tokio::test]
async fn failure_stays_in_its_branch_and_uncertain_is_not_resent() {
    let (core, p, _) = fixture_project().await;
    *core.mock.fail_when.lock().unwrap() = Some(("evening".into(), ErrKind::Uncertain));
    let pl = plan(&core, &p, json!(["day-image", "night-image"]), false).await;
    let r = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(r["status"], "partial");
    let c = canvas(&core, &p).await;
    assert_eq!(c["results"]["day-image"]["status"], "succeeded");
    assert_eq!(c["results"]["night-image"]["status"], "uncertain");
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 2, "one attempt per node, no automatic retry");
    // The original image file is still there and the day branch is reusable.
    let pl = plan(&core, &p, json!(["day-image", "night-image"]), false).await;
    assert_eq!((pl["reused"].clone(), pl["newImages"].clone()), (json!(1), json!(1)));
}

#[tokio::test]
async fn agent_builds_branch_and_undo_is_one_step() {
    let (core, p, _) = fixture_project().await;
    let t = call(&core, "agent_thread", json!({"projectId": p})).await["id"].as_str().unwrap().to_string();
    let before = canvas(&core, &p).await;
    let r = call(&core, "agent_send", json!({"projectId": p, "threadId": t, "text": "اعمل لي نسختين من نفس الإعلان، واحدة بإضاءة صباحية والثانية مسائية، وحافظ على الشعار", "selection": ["reference"]})).await;
    assert_eq!(r["meta"]["changed"], true);
    let after = canvas(&core, &p).await;
    assert_eq!(nodes(&after), 13);
    let refs: Vec<&Value> = after["graph"]["edges"].as_array().unwrap().iter().filter(|e| e["source"] == "reference" && e["targetHandle"] == "refs").collect();
    assert_eq!(refs.len(), 4, "both new branches use the selected reference");
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 0, "the agent cannot generate");
    assert_eq!(after["undo"]["actor"], "agent");
    // A later user edit is its own undo step.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-prompt", "changes": {"title": "صباح"}}])).await.unwrap();
    call(&core, "canvas_undo", json!({"projectId": p})).await;
    assert_eq!(nodes(&canvas(&core, &p).await), 13);
    let undone = call(&core, "canvas_undo", json!({"projectId": p})).await;
    assert_eq!(undone["graph"], before["graph"], "one undo removes the whole agent batch and nothing else");
    assert_eq!(call(&core, "assets_list", json!({"projectId": p})).await.as_array().unwrap().len(), 1);
    assert_eq!(nodes(&call(&core, "canvas_redo", json!({"projectId": p})).await), 13);
}

#[tokio::test]
async fn agent_bad_batches_change_nothing() {
    let (core, p, _) = fixture_project().await;
    let t = call(&core, "agent_thread", json!({"projectId": p})).await["id"].as_str().unwrap().to_string();
    let rev = canvas(&core, &p).await["revision"].as_i64().unwrap();
    // valid connect + cyclic connect in one batch, then a wrong revision, then an unknown field
    {
        let mut script = core.mock.tool_script.lock().unwrap();
        script.push_back(MockProvider::tool_turn(vec![("c1", "apply_canvas_patch", json!({"expectedRevision": rev, "operations": [
            {"op": "connect", "source": "day-image", "target": "night-image", "targetHandle": "refs"},
            {"op": "connect", "source": "night-image", "target": "day-image", "targetHandle": "refs"}]}))], ""));
        script.push_back(MockProvider::tool_turn(vec![("c2", "apply_canvas_patch", json!({"expectedRevision": rev - 1, "operations": [
            {"op": "removeNode", "nodeId": "ghost"}, {"op": "moveNode", "nodeId": "reference", "position": {"x": 1, "y": 1}}]}))], ""));
        script.push_back(MockProvider::tool_turn(vec![("c3", "apply_canvas_patch", json!({"expectedRevision": rev, "projectId": "other", "operations": [
            {"op": "moveNode", "nodeId": "reference", "position": {"x": 1, "y": 1}}]}))], ""));
        script.push_back(MockProvider::tool_turn(vec![("c4", "run_shell", json!({"cmd": "ls"}))], ""));
    }
    let r = call(&core, "agent_send", json!({"projectId": p, "threadId": t, "text": "جرّب", "selection": []})).await;
    assert_eq!(r["meta"]["changed"], false);
    assert!(r["text"].as_str().unwrap().contains("limit of 4 tool rounds"));
    let c = canvas(&core, &p).await;
    assert_eq!(c["revision"], rev);
    assert!(c["undo"]["actor"] != "agent");
    // removal is held for the user instead of being applied
    assert_eq!(r["meta"]["pendingPatch"]["operations"][0]["op"], "removeNode");
}

#[tokio::test]
async fn agent_conflict_keeps_user_edit_and_duplicates_apply_once() {
    let (core, p, _) = fixture_project().await;
    let t = call(&core, "agent_thread", json!({"projectId": p})).await["id"].as_str().unwrap().to_string();
    let rev = canvas(&core, &p).await["revision"].as_i64().unwrap();
    // The user edits while the agent "thinks": the agent's batch carries the old revision.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-prompt", "changes": {"text": "تعديل المستخدم"}}])).await.unwrap();
    let stale = json!({"expectedRevision": rev, "operations": [{"op": "setNodeData", "nodeId": "night-prompt", "changes": {"text": "نص الوكيل"}}]});
    let good = json!({"expectedRevision": rev + 1, "operations": [{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"size": "2K"}}}]});
    {
        let mut script = core.mock.tool_script.lock().unwrap();
        script.push_back(MockProvider::tool_turn(vec![("c1", "apply_canvas_patch", stale)], ""));
        // same tool_call id sent twice in one turn, and once more in the next round
        script.push_back(MockProvider::tool_turn(vec![("c2", "apply_canvas_patch", good.clone()), ("c2", "apply_canvas_patch", good.clone())], ""));
        script.push_back(MockProvider::tool_turn(vec![("c2", "apply_canvas_patch", good)], ""));
        script.push_back(MockProvider::tool_turn(vec![], "رفعتُ دقة نسخة المساء فقط."));
    }
    let r = call(&core, "agent_send", json!({"projectId": p, "threadId": t, "text": "ارفع دقة نسخة المساء فقط", "selection": []})).await;
    let c = canvas(&core, &p).await;
    let node = |id: &str| c["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap().clone();
    assert_eq!(node("night-prompt")["data"]["text"], "تعديل المستخدم");
    assert_eq!(node("night-image")["data"]["settings"], json!({"size": "2K", "output_format": "png", "background": "opaque"}));
    assert_eq!(c["revision"], rev + 2, "user edit + exactly one agent edit");
    assert_eq!(r["meta"]["actions"].as_array().unwrap().len(), 1);
    assert_eq!(core.mock.image_calls.load(Ordering::SeqCst), 0, "changing a setting does not regenerate");
}

#[tokio::test]
async fn chat_streams_and_edit_uses_the_chosen_image() {
    let (core, p, _) = fixture_project().await;
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    let r = call(&core, "chat_send", json!({"threadId": t, "text": "اقترح إضاءتين"})).await;
    assert_eq!((r["status"].clone(), r["model"].clone()), (json!("complete"), json!("mock-seed")));
    let r = call(&core, "chat_send", json!({"threadId": t, "text": "اختبار انقطاع البث"})).await;
    assert_eq!(r["status"], "incomplete");
    let msgs = call(&core, "messages_list", json!({"threadId": t})).await;
    let last = msgs.as_array().unwrap().last().unwrap();
    assert!(last["status"] == "incomplete" && !last["content"].as_str().unwrap().is_empty(), "partial text is kept");

    // Generate from chat → same engine, visible flow on the canvas.
    let flow = call(&core, "chat_image_flow", json!({"projectId": p, "threadId": t, "prompt": "عبوة على خشب فاتح"})).await;
    assert_eq!(flow["plan"]["newImages"], 1);
    assert_eq!(nodes(&canvas(&core, &p).await), 10);
    call(&core, "run_execute", json!({"planId": flow["plan"]["id"], "threadId": t})).await;
    let msgs = call(&core, "messages_list", json!({"threadId": t})).await;
    let first = msgs.as_array().unwrap().last().unwrap()["meta"]["assetIds"][0].as_str().unwrap().to_string();
    // Running the same node from the canvas does not create a second request.
    assert_eq!(plan(&core, &p, json!([flow["nodeId"]]), false).await["newImages"], 0);

    // "Change the background only" on that exact image.
    let edit = call(&core, "chat_image_flow", json!({"projectId": p, "threadId": t, "prompt": "غيّر الخلفية إلى لون رملي فقط", "refAssetId": first})).await;
    call(&core, "run_execute", json!({"planId": edit["plan"]["id"], "threadId": t})).await;
    let sent = core.mock.image_log.lock().unwrap().last().unwrap().clone();
    assert_eq!(sent["image"], json!([core.asset_bytes(&first).unwrap().2]), "bytes of the chosen version were sent");
    let assets = call(&core, "assets_list", json!({"projectId": p})).await;
    let newest = &assets[0];
    assert_eq!(newest["parentAssetIds"], json!([first]));
    assert!(core.asset_bytes(&first).is_ok(), "the original is kept next to the edit");
}

#[tokio::test]
async fn interrupted_request_is_uncertain_after_restart() {
    let (core, p, dir) = fixture_project().await;
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    core.db(|c| {
        c.execute("UPDATE runs SET status='running' WHERE id=?1", [pl["id"].as_str().unwrap()])?;
        c.execute("INSERT INTO node_runs(id,run_id,project_id,node_id,kind,status,fingerprint,provider,region,model_id,final_prompt,settings_json,started_at) VALUES('nr',?1,?2,'day-image','image','sending','f','modelark','ap-southeast-1','m','p','{}',1)",
            [pl["id"].as_str().unwrap(), p.as_str()])?;
        Ok(())
    }).unwrap();
    drop(core);
    let core = open(&dir);
    assert_eq!(canvas(&core, &p).await["results"]["day-image"]["status"], "uncertain");
    assert_eq!(fail(&core, "run_execute", json!({"planId": pl["id"]})).await, "run_already_started");
}

// ───────────── real ModelArk client against a local fake server ─────────────

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, String, Value)>>>);

/// Starts a fake ModelArk. `mode` picks how /images/generations behaves.
fn fake_modelark(mode: &'static str) -> (String, Seen) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v3", server.server_addr());
    let seen = Seen::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let auth = req.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.to_string()).unwrap_or_default();
            let url = req.url().to_string();
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            log.0.lock().unwrap().push((url.clone(), auth, v.clone()));
            let json = |v: Value, code: u16| tiny_http::Response::from_string(v.to_string()).with_status_code(code)
                .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
            if mode == "chatdenied" && url.ends_with("/chat/completions") {
                let _ = req.respond(json(json!({"error": {"code": "AccessDenied", "message": "The request failed because you do not have access to the requested resource."}}), 403));
            } else if url.ends_with("/models") {
                let _ = req.respond(json(json!({"data": [{"id": "dola-seed-2-1-turbo-260628"}, {"id": "dola-seedream-5-5-261028"}]}), 200));
            } else if url.ends_with("/chat/completions") && v["stream"] == true {
                let mut sse = String::new();
                for part in ["مرحبًا", " من", " Seed"] {
                    sse.push_str(&format!("data: {}\n\n", json!({"model": "dola-seed-2-1-turbo-260628", "choices": [{"delta": {"content": part}}]})));
                }
                if !v["messages"].to_string().contains("انقطاع") {
                    sse.push_str("data: [DONE]\n\n");
                }
                let _ = req.respond(tiny_http::Response::from_string(sse));
            } else if url.ends_with("/chat/completions") {
                let has_tool_result = v["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool");
                let message = if has_tool_result {
                    json!({"role": "assistant", "content": "رفعتُ الدقة."})
                } else {
                    let rev = v["messages"].to_string().split("\\\"revision\\\":").nth(1).and_then(|s| s.split(',').next().map(|n| n.trim().parse::<i64>().unwrap())).unwrap();
                    json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "apply_canvas_patch",
                        "arguments": json!({"expectedRevision": rev, "operations": [{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"size": "2K"}}}]}).to_string()}}]})
                };
                let _ = req.respond(json(json!({"model": "dola-seed-2-1-turbo-260628", "choices": [{"message": message}]}), 200));
            } else if url.ends_with("/images/generations") {
                match mode {
                    "ok" => {
                        use base64::Engine as _;
                        let png = seed_studio_lib::provider::mock_image("fake", &[], false, false);
                        let _ = req.respond(json(json!({"model": v["model"], "data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(png)}], "usage": {"generated_images": 1}}), 200));
                    }
                    "auth" => { let _ = req.respond(json(json!({"error": {"code": "AuthenticationError", "message": "invalid api key"}}), 401)); }
                    "notopen" => { let _ = req.respond(json(json!({"error": {"code": "ModelNotOpen", "message": "model not activated"}}), 404)); }
                    _ => drop(req), // connection closes with no response: outcome unknown
                }
            }
        }
    });
    (base, seen)
}

async fn live_like(mode: &'static str) -> (Core, String, Seen, PathBuf) {
    let (core, p, dir) = fixture_project().await;
    let (base, seen) = fake_modelark(mode);
    *core.base_override.lock().unwrap() = Some(base);
    call(&core, "setting_set", json!({"key": "provider_mode", "value": "modelark"})).await;
    (core, p, seen, dir)
}

#[tokio::test]
async fn modelark_requires_key_and_never_stores_it() {
    let (core, p, seen, dir) = live_like("ok").await;
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    assert_eq!(fail(&core, "chat_send", json!({"threadId": t, "text": "مرحبا"})).await, "no_key");
    assert!(seen.0.lock().unwrap().is_empty(), "nothing is sent without a key");
    let secret = "test-only-placeholder-credential";
    let state = call(&core, "key_set", json!({"region": "ap-southeast-1", "key": secret})).await;
    assert_eq!(state["hasKey"]["ap-southeast-1"], true);
    assert_eq!(state["hasKey"]["eu-west-1"], false, "keys are per region");
    assert!(!state.to_string().contains(secret));
    let check = call(&core, "check_connection", json!({})).await;
    let models = check["state"]["models"].as_array().unwrap();
    assert_eq!(models.iter().find(|m| m["key"] == "seedream-5.5").unwrap()["status"], "visible");
    assert_eq!(models.iter().find(|m| m["key"] == "seedream-5.0-pro").unwrap()["status"], "not_visible");
    assert_eq!(seen.0.lock().unwrap().len(), 1, "the check is one GET and generates nothing");
    // chat: streamed text, model name reported by the provider
    let r = call(&core, "chat_send", json!({"threadId": t, "text": "مرحبا"})).await;
    assert_eq!((r["status"].clone(), r["model"].clone()), (json!("complete"), json!("dola-seed-2-1-turbo-260628")));
    let msgs = call(&core, "messages_list", json!({"threadId": t})).await;
    assert_eq!(msgs.as_array().unwrap().last().unwrap()["content"], "مرحبًا من Seed");
    assert_eq!(call(&core, "chat_send", json!({"threadId": t, "text": "اختبار انقطاع"})).await["status"], "incomplete");
    let (_, auth, body) = seen.0.lock().unwrap()[1].clone();
    assert_eq!(auth, format!("Bearer {secret}"));
    assert_eq!(body["stream"], true);
    // 'tried' only after a real success of that operation
    let models = call(&core, "app_state", json!({})).await["models"].clone();
    assert_eq!(models.as_array().unwrap().iter().find(|m| m["key"] == "seed-2.1-turbo").unwrap()["status"], "tried");
    assert_eq!(models.as_array().unwrap().iter().find(|m| m["key"] == "seedream-5.5").unwrap()["status"], "visible");
    // the key is in neither the database nor any file under the data folder
    drop(core);
    fn scan(dir: &std::path::Path, needle: &[u8]) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().any(|e| {
            let p = e.path();
            if p.is_dir() { scan(&p, needle) } else { std::fs::read(&p).map(|b| b.windows(needle.len()).any(|w| w == needle)).unwrap_or(false) }
        })
    }
    assert!(!scan(&dir, secret.as_bytes()));
}

#[tokio::test]
async fn modelark_image_and_agent_round_trip() {
    let (core, p, seen, _) = live_like("ok").await;
    call(&core, "key_set", json!({"region": "ap-southeast-1", "key": "k"})).await;
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    assert_eq!(pl["provider"], "modelark");
    let r = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(r["status"], "completed");
    let (url, _, body) = seen.0.lock().unwrap()[0].clone();
    assert!(url.ends_with("/images/generations"));
    assert!(body["image"].as_str().unwrap().starts_with("data:image/png;base64,"), "one reference is sent as a data URI string");
    assert_eq!(body["prompt"].as_str().unwrap().contains("morning light"), true);
    let asset = r["results"][0]["assetId"].as_str().unwrap();
    assert!(core.asset_bytes(asset).unwrap().3.starts_with(&[0x89, b'P', b'N', b'G']), "saved locally; viewing needs no network");
    // agent through the real wire format: tool definitions out, tool result back
    let t = call(&core, "agent_thread", json!({"projectId": p})).await["id"].as_str().unwrap().to_string();
    let r = call(&core, "agent_send", json!({"projectId": p, "threadId": t, "text": "ارفع دقة نسخة المساء فقط", "selection": ["night-image"]})).await;
    assert_eq!(r["text"], "رفعتُ الدقة.");
    let reqs = seen.0.lock().unwrap().clone();
    let first = &reqs[1].2;
    assert_eq!(first["stream"], false);
    assert_eq!(first["thinking"]["type"], "disabled");
    let names: Vec<&str> = first["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["read_canvas", "get_model_options", "apply_canvas_patch", "plan_run"]);
    let second = reqs[2].2["messages"].as_array().unwrap().clone();
    let tool_msg = second.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(tool_msg["tool_call_id"], "call_1");
    assert!(second.iter().any(|m| m["tool_calls"][0]["id"] == "call_1"), "assistant tool-call message is echoed back");
    let c = canvas(&core, &p).await;
    assert_eq!(c["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "night-image").unwrap()["data"]["settings"]["size"], "2K");
    assert_eq!(reqs.iter().filter(|r| r.0.ends_with("/images/generations")).count(), 1, "the agent's edit did not regenerate");
}

#[tokio::test]
async fn modelark_errors_are_classified_and_never_retried() {
    for (mode, status, needle) in [("auth", "failed", "API key was rejected"), ("notopen", "failed", "no access to this model")] {
        let (core, p, seen, _) = live_like(mode).await;
        call(&core, "key_set", json!({"region": "ap-southeast-1", "key": "k"})).await;
        let pl = plan(&core, &p, json!(["day-image", "night-image"]), false).await;
        let r = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
        assert_eq!(r["results"][0]["status"], status, "{mode}");
        assert!(r["results"][0]["error"].as_str().unwrap().contains(needle), "{mode}: {}", r["results"][0]["error"]);
        assert_eq!(seen.0.lock().unwrap().len(), 2, "{mode}: one POST per node, no silent retry or failover");
        let region_ok = seen.0.lock().unwrap().iter().all(|r| r.2["model"] == "dola-seedream-5-5-261028");
        assert!(region_ok, "model is never swapped on error");
        if mode == "notopen" {
            let models = call(&core, "app_state", json!({})).await["models"].clone();
            assert_eq!(models.as_array().unwrap().iter().find(|m| m["key"] == "seedream-5.5").unwrap()["status"], "needs_activation");
        }
    }
}

#[tokio::test]
async fn modelark_dropped_connection_is_uncertain_and_sent_once() {
    use std::io::Read;
    // A server that reads the request and hangs up without answering.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v3", listener.local_addr().unwrap());
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut s = stream;
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            drop(s);
        }
    });
    let (core, p, _) = fixture_project().await;
    *core.base_override.lock().unwrap() = Some(base);
    call(&core, "setting_set", json!({"key": "provider_mode", "value": "modelark"})).await;
    call(&core, "key_set", json!({"region": "ap-southeast-1", "key": "k"})).await;
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    let r = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(r["results"][0]["status"], "uncertain");
    assert!(r["results"][0]["error"].as_str().unwrap().contains("outcome is unknown"));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "a POST with an unknown outcome is not sent again");
    // Unreachable server: the request never left, so it is a plain failure.
    *core.base_override.lock().unwrap() = Some("http://127.0.0.1:9/api/v3".into());
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    let r = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(r["results"][0]["status"], "failed");
}

#[tokio::test]
async fn dropped_files_become_images_or_document_text() {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let (core, p, seen, _) = live_like("ok").await;
    call(&core, "key_set", json!({"region": "ap-southeast-1", "key": "  Bearer k-123\n"})).await;
    let png = seed_studio_lib::provider::mock_image("x", &[], false, false);
    let img = call(&core, "file_import", json!({"projectId": p, "name": "shot.png", "base64": b64(&png)})).await;
    assert_eq!(img["kind"], "image");
    let doc = call(&core, "file_import", json!({"projectId": p, "name": "brief.md", "base64": b64("The bottle label must stay black.".as_bytes())})).await;
    assert_eq!(doc["kind"], "document");
    assert_eq!(fail(&core, "file_import", json!({"projectId": p, "name": "archive.zip", "base64": b64(&[0x50, 0x4b, 3, 4, 0, 0xff, 0xfe, 0])})).await, "unsupported_file");
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    call(&core, "chat_send", json!({"threadId": t, "text": "Summarize", "attachmentIds": [img["id"]], "documentIds": [doc["id"]]})).await;
    let (_, auth, body) = seen.0.lock().unwrap()[0].clone();
    assert_eq!(auth, "Bearer k-123", "pasted header form and whitespace are cleaned");
    let parts = body["messages"].as_array().unwrap().last().unwrap()["content"].as_array().unwrap().clone();
    assert!(parts[0]["text"].as_str().unwrap().contains("<file name=\"brief.md\">\nThe bottle label must stay black."));
    assert!(parts[1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
    // A follow-up still carries the document text.
    call(&core, "chat_send", json!({"threadId": t, "text": "And shorter?"})).await;
    let body = seen.0.lock().unwrap()[1].2.clone();
    assert!(body["messages"].to_string().contains("The bottle label must stay black."));
    let msgs = call(&core, "messages_list", json!({"threadId": t})).await;
    assert_eq!(msgs[0]["meta"]["documents"][0]["name"], "brief.md");
    assert_eq!(msgs[0]["content"], "Summarize", "the stored message is what the user typed");
}

#[tokio::test]
async fn model_test_reports_no_access_without_switching_models() {
    let (core, p, seen, _) = live_like("chatdenied").await;
    call(&core, "key_set", json!({"region": "ap-southeast-1", "key": "k"})).await;
    let r = call(&core, "model_test", json!({"modelKey": "seed-2.1-turbo"})).await;
    assert_eq!(r["ok"], false);
    assert!(r["message"].as_str().unwrap().contains("no access to this model"));
    let m = r["state"]["models"].as_array().unwrap().iter().find(|m| m["key"] == "seed-2.1-turbo").unwrap().clone();
    assert_eq!(m["status"], "needs_activation");
    assert_eq!(r["state"]["chatModel"], "seed-2.1-turbo", "the chosen model is never changed for the user");
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    assert_eq!(fail(&core, "chat_send", json!({"threadId": t, "text": "hi"})).await, "provider_activation");
    assert!(seen.0.lock().unwrap().iter().all(|r| r.2["model"] == "dola-seed-2-1-turbo-260628"));
    assert_eq!(fail(&core, "model_test", json!({"modelKey": "seedream-5.5"})).await, "bad_args");
}

#[tokio::test]
async fn aspect_ratio_is_sent_as_pixels_inside_documented_range() {
    let (core, p, _) = fixture_project().await;
    // 16:9 at 2K on Seedream 5.5 → explicit pixels, no invented aspect field.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"settings": {"aspect": "16:9", "size": "2K"}}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    assert_eq!(pl["items"][0]["pixelSize"], "2720×1536");
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    let body = core.mock.image_log.lock().unwrap()[0].clone();
    assert_eq!(body["size"], "2720x1536");
    assert!(body.get("aspect").is_none() && body.get("aspect_ratio").is_none());
    // Every ratio × tier of every model stays inside that model's documented pixel range.
    let cat = &core.catalog;
    for m in cat.models.iter().filter(|m| m.kind == "image") {
        let (min, max) = (m.controls["pixelRange"][0].as_u64().unwrap(), m.controls["pixelRange"][1].as_u64().unwrap());
        for tier in m.controls["sizePresets"].as_array().unwrap() {
            for a in seed_studio_lib::catalog::ASPECTS.iter().filter(|a| **a != "auto") {
                let s = json!({"aspect": a, "size": tier}).as_object().unwrap().clone();
                let (w, h) = cat.pixel_size(&m.key, &s).unwrap_or_else(|| panic!("{} {a} {tier}", m.key));
                let px = w as u64 * h as u64;
                assert!(px >= min && px <= max, "{} {a} {tier}: {w}x{h}", m.key);
                let (x, y) = a.split_once(':').unwrap();
                let want = x.parse::<f64>().unwrap() / y.parse::<f64>().unwrap();
                assert!(((w as f64 / h as f64) / want - 1.0).abs() < 0.03, "{} {a}: {w}x{h}", m.key);
            }
        }
    }
    // "auto" keeps the tier; unknown ratios are rejected by the validator.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"aspect": "auto"}}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert_eq!(core.mock.image_log.lock().unwrap()[1]["size"], "1.5K");
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"settings": {"aspect": "7:5"}}}])).await.is_err());
    // Chat flow carries the chosen options to the node.
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    let flow = call(&core, "chat_image_flow", json!({"projectId": p, "threadId": t, "prompt": "a bottle", "settings": {"aspect": "9:16", "size": "1K", "output_format": "jpeg"}})).await;
    assert_eq!(flow["plan"]["items"][0]["pixelSize"], "768×1360");
    assert_eq!(flow["plan"]["items"][0]["settings"]["output_format"], "jpeg");
}

// ───────────── video (Seedance tasks) ─────────────

async fn video_until_done(core: &Core, id: &str) -> Value {
    for _ in 0..5 {
        let v = call(core, "video_poll", json!({"videoId": id})).await;
        if v["status"] != "queued" && v["status"] != "running" { return v; }
    }
    panic!("video did not finish");
}

#[tokio::test]
async fn video_draft_then_upscale_and_rules() {
    let (core, p, dir) = fixture_project().await;
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    let bottle = call(&core, "assets_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    // documented rules
    let mixed = json!({"projectId": p, "threadId": t, "prompt": "x", "inputs": [{"role": "first_frame", "assetId": bottle}, {"role": "reference_image", "assetId": bottle}]});
    assert_eq!(fail(&core, "video_plan", mixed).await, "bad_video_request");
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "prompt": "x", "inputs": [{"role": "last_frame", "assetId": bottle}]})).await, "bad_video_request");
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "prompt": "x", "duration": 3})).await, "bad_video_request");
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "prompt": ""})).await, "bad_video_request");

    // draft with first + last frame: forced to 480p and adaptive ratio, and says so
    let plan = call(&core, "video_plan", json!({"projectId": p, "threadId": t, "prompt": "The bottle turns slowly", "draft": true, "resolution": "1080p", "ratio": "16:9", "duration": 5,
        "inputs": [{"role": "last_frame", "assetId": bottle}, {"role": "first_frame", "assetId": bottle}]})).await;
    assert_eq!(plan["params"], json!({"draft": true, "duration": 5, "ratio": "adaptive", "resolution": "480p"}));
    assert_eq!(plan["notes"].as_array().unwrap().len(), 2);
    assert_eq!(core.mock.video_log.lock().unwrap().len(), 0, "planning sends nothing");
    assert_eq!(call(&core, "videos_list", json!({"projectId": p})).await.as_array().unwrap().len(), 0, "plans are not listed as videos");

    let id = plan["id"].as_str().unwrap().to_string();
    let (a, b) = tokio::join!(dispatch(&core, "video_start", json!({"videoId": id})), dispatch(&core, "video_start", json!({"videoId": id})));
    assert!(a.is_ok() != b.is_ok(), "two clicks create one task");
    let body = core.mock.video_log.lock().unwrap()[0].clone();
    assert_eq!(body["model"], "dreamina-seedance-2-5-260628");
    assert_eq!(body["content"][0], json!({"type": "text", "text": "The bottle turns slowly"}));
    assert_eq!((body["content"][1]["role"].clone(), body["content"][2]["role"].clone()), (json!("first_frame"), json!("last_frame")));
    assert_eq!((body["draft"].clone(), body["resolution"].clone(), body["ratio"].clone(), body["duration"].clone()), (json!(true), json!("480p"), json!("adaptive"), json!(5)));

    // the task id is stored at once; polling resumes after a restart and saves the file locally
    drop(core);
    let core = open(&dir);
    let v = call(&core, "videos_list", json!({"projectId": p})).await[0].clone();
    assert_eq!((v["status"].clone(), v["taskId"].clone()), (json!("queued"), json!("mock-task-1")));
    let done = video_until_done(&core, &id).await;
    assert_eq!((done["status"].clone(), done["hasFile"].clone(), done["provider"].clone()), (json!("succeeded"), json!(true), json!("mock")));
    let (_, mime, _, bytes) = core.asset_bytes(&id).unwrap();
    assert_eq!(mime, "video/mp4");
    assert_eq!(&bytes[4..8], b"ftyp");
    let msgs = call(&core, "messages_list", json!({"threadId": t})).await;
    let kinds: Vec<&str> = msgs.as_array().unwrap().iter().filter_map(|m| m["meta"]["kind"].as_str()).collect();
    assert_eq!(kinds, ["video_request", "video"]);

    // upscale = final video from the draft task id; only the resolution travels with it
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "draftOf": id, "resolution": "480p"})).await, "bad_video_request");
    let up = call(&core, "video_plan", json!({"projectId": p, "threadId": t, "draftOf": id, "resolution": "1080p"})).await;
    assert_eq!((up["kind"].clone(), up["draftOf"].clone(), up["prompt"].clone()), (json!("final"), json!(id), json!("The bottle turns slowly")));
    call(&core, "video_start", json!({"videoId": up["id"]})).await;
    let body = core.mock.video_log.lock().unwrap()[0].clone();
    assert_eq!(body, json!({"model": "dreamina-seedance-2-5-260628", "content": [{"type": "draft_task", "draft_task": {"id": "mock-task-1"}}], "resolution": "1080p"}));
    // "recreate" data: the stored row still has everything needed to refill the composer
    assert_eq!(up["inputs"].as_array().unwrap().len(), 2);
    // a final cannot itself be upscaled
    video_until_done(&core, up["id"].as_str().unwrap()).await;
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "draftOf": up["id"], "resolution": "4k"})).await, "bad_video_request");
}

#[tokio::test]
async fn video_unknown_outcome_is_not_resent() {
    let (core, p, _) = fixture_project().await;
    *core.mock.fail_when.lock().unwrap() = Some(("storm".into(), ErrKind::Uncertain));
    let plan = call(&core, "video_plan", json!({"projectId": p, "prompt": "a storm at sea", "draft": true})).await;
    let v = call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    assert_eq!(v["status"], "uncertain");
    assert!(v["error"].as_str().unwrap().contains("outcome is unknown"));
    assert_eq!(fail(&core, "video_start", json!({"videoId": plan["id"]})).await, "video_already_started");
    assert_eq!(call(&core, "video_poll", json!({"videoId": plan["id"]})).await["status"], "uncertain");
}

#[tokio::test]
async fn video_node_on_canvas() {
    let (core, p, _) = fixture_project().await;
    let c = patch(&core, &p, json!([
        {"op": "addNode", "id": "vid", "type": "seedance", "position": {"x": 760, "y": 240}, "data": {"title": "Bottle spin"}},
        {"op": "connect", "source": "day-prompt", "target": "vid", "targetHandle": "prompt"},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"}
    ])).await.unwrap();
    let node = c["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "vid").unwrap().clone();
    assert_eq!(node["data"]["settings"], json!({"duration": "6", "pass": "draft", "ratio": "16:9", "resolution": "1080p", "sound": "on", "voice": "off"}));
    // typed ports and closed settings, same validator as every other node
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "night-prompt", "target": "vid", "targetHandle": "first"}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "reference", "target": "vid", "targetHandle": "prompt"}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "vid", "changes": {"settings": {"duration": "45"}}}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "vid", "changes": {"settings": {"fps": "60"}}}])).await.is_err());
    // an image that an upstream Seedream node has not produced yet is named, not guessed
    patch(&core, &p, json!([{"op": "connect", "source": "day-image", "target": "vid", "targetHandle": "last"}])).await.unwrap();
    let e = dispatch(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await.unwrap_err();
    assert!(e.message.contains("Morning shot"));
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    // now the plan uses the prompt text, the bottle as first frame and the generated image as last frame
    let plan_v = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert!(plan_v["prompt"].as_str().unwrap().contains("morning light"));
    let roles: Vec<&str> = plan_v["inputs"].as_array().unwrap().iter().map(|i| i["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["first_frame", "last_frame"]);
    assert_eq!(plan_v["params"]["resolution"], "480p");
    // frames and references cannot be mixed: the node may be wired that way, the plan refuses it
    patch(&core, &p, json!([{"op": "connect", "source": "reference", "target": "vid", "targetHandle": "refs"}])).await.unwrap();
    assert_eq!(fail(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await, "bad_video_request");
    patch(&core, &p, json!([{"op": "disconnect", "source": "reference", "target": "vid", "targetHandle": "refs"}])).await.unwrap();
    // run it: the video is tied to the node, and its upscale stays on the same node
    call(&core, "video_start", json!({"videoId": plan_v["id"]})).await;
    let done = video_until_done(&core, plan_v["id"].as_str().unwrap()).await;
    assert_eq!((done["nodeId"].clone(), done["status"].clone()), (json!("vid"), json!("succeeded")));
    let up = call(&core, "video_plan", json!({"projectId": p, "draftOf": done["id"], "resolution": "4k"})).await;
    assert_eq!(up["nodeId"], "vid");
    // "Run selected" on image nodes ignores the video node: nothing video is sent by the image engine
    let pl = plan(&core, &p, json!(["vid", "day-image"]), false).await;
    assert_eq!((pl["newImages"].clone(), pl["items"].as_array().unwrap().len()), (json!(0), 1));
}

#[tokio::test]
async fn gateway_caps_chat_checks_balance_and_reports_budget() {
    use seed_studio_lib::provider::{ChatRequest, ModelArk};
    // The cap is sent only when a gateway asks for it.
    let req = ChatRequest { model: "m".into(), messages: vec![], tools: None, thinking: None, cancel: Default::default() };
    assert!(ModelArk::chat_body(&req, false, None).get("max_completion_tokens").is_none());
    assert_eq!(ModelArk::chat_body(&req, false, Some(4096))["max_completion_tokens"], 4096);

    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v3", server.server_addr());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for mut rq in server.incoming_requests() {
            let mut body = String::new();
            let _ = rq.as_reader().read_to_string(&mut body);
            seen2.lock().unwrap().push(format!("{} {} {}", rq.method(), rq.url(), body));
            let (code, text) = if rq.url().ends_with("/abpp/balance") {
                (200, r#"{"spent":1.5,"held":0.25,"left":8.25}"#)
            } else {
                (402, r#"{"error":{"code":"abpp_budget_exceeded","message":"maximum cost exceeds remaining budget"}}"#)
            };
            let _ = rq.respond(tiny_http::Response::from_string(text).with_status_code(code));
        }
    });
    let dir = temp_dir();
    let core = open(&dir);
    dispatch(&core, "setting_set", json!({"key":"provider_mode","value":"modelark"})).await.unwrap();
    let core = with_test_gateway(core, &base);
    dispatch(&core, "setting_set", json!({"key":"region","value":"test-gateway"})).await.unwrap();
    dispatch(&core, "key_set", json!({"region":"test-gateway","key":"test-gateway-key"})).await.unwrap();
    *core.base_override.lock().unwrap() = Some(base);

    // An endpoint id is ignored on the gateway: the documented model id is sent.
    dispatch(&core, "setting_set", json!({"key":"endpoint:seed-2.0-lite","value":"ep-123"})).await.unwrap();
    assert_eq!(core.resolve_model_id("seed-2.0-lite").unwrap(), "seed-2-0-lite-260428");

    // Check connection reads the balance, never GET /models.
    let r = dispatch(&core, "check_connection", json!({})).await.unwrap();
    assert_eq!(r["balance"]["left"], 8.25);
    assert!(seen.lock().unwrap().iter().all(|l| !l.contains("/models")));

    // A 402 is reported as a budget refusal, with the cap in the request that was sent.
    let t = dispatch(&core, "model_test", json!({"modelKey":"seed-2.0-mini"})).await;
    let text = match t { Ok(v) => v.to_string(), Err(e) => e.message };
    assert!(text.contains("budget"), "{text}");
    let log = seen.lock().unwrap().join("\n");
    assert!(log.contains("max_completion_tokens") && log.contains("seed-2-0-mini-260428"), "{log}");
}

#[tokio::test]
async fn video_node_sends_a_voice_clip_for_lip_sync() {
    use base64::Engine;
    use seed_studio_lib::provider::{ModelArk, VideoImage, VideoRequest};
    let (core, p, _) = fixture_project().await;
    // A 3-second mono WAV as the project's voice track.
    let rate = 8000u32;
    let mut wav = Vec::new();
    let data = rate * 3 * 2;
    wav.extend_from_slice(b"RIFF"); wav.extend_from_slice(&(36 + data).to_le_bytes()); wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); wav.extend_from_slice(&1u16.to_le_bytes()); wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes()); wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data"); wav.extend_from_slice(&data.to_le_bytes());
    for i in 0..rate * 3 { wav.extend_from_slice(&(((i % 80) as i16 - 40) * 300).to_le_bytes()); }
    let a = call(&core, "audio_import", json!({"projectId": p, "name": "voice.wav", "base64": base64::engine::general_purpose::STANDARD.encode(&wav)})).await;
    assert_eq!(a["durationMs"], 3000);
    assert_eq!(a["peaks"].as_array().unwrap().len(), 60);

    patch(&core, &p, json!([
        {"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "settings": {"voice": "on", "duration": "4"}}},
        {"op": "connect", "source": "day-prompt", "target": "vid", "targetHandle": "prompt"},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"}
    ])).await.unwrap();
    // Voice on without a clip is refused before anything is planned.
    assert_eq!(fail(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await, "bad_video_request");
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "vid", "changes": {"settings": {"voiceStart": "0.5", "voiceEnd": "1.5"}}}])).await.unwrap();
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["inputs"][0]["role"], "reference_image", "with a voice clip the frame is a reference image");
    assert!(plan["prompt"].as_str().unwrap().starts_with("@Image 1 is the first frame"));
    assert_eq!(plan["params"]["_audio"]["startMs"], 500);
    assert_eq!(plan["params"]["generate_audio"], true);
    let v = call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    assert_eq!(v["status"], "queued");

    // Wire format: the clip is reference audio; app-only keys never reach the provider.
    let mut params = serde_json::Map::new();
    params.insert("_audio".into(), json!({"audioId": "x"}));
    params.insert("duration".into(), json!(4));
    let body = ModelArk::video_body(&VideoRequest {
        model: "m".into(), prompt: "p".into(), draft_task_id: None, params,
        images: vec![VideoImage { role: "reference_image".into(), mime: "image/png".into(), bytes: vec![1], library: None }],
        audio: vec![("audio/wav".into(), vec![1, 2])],
    });
    assert!(body.get("_audio").is_none());
    assert_eq!(body["content"][2]["type"], "audio_url");
    assert_eq!(body["content"][2]["role"], "reference_audio");
    assert!(body["content"][2]["audio_url"]["url"].as_str().unwrap().starts_with("data:audio/wav;base64,"));

    // A final from a draft is only offered at the documented resolutions.
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "draftOf": plan["id"], "resolution": "720p"})).await, "bad_video_request");
}

#[tokio::test]
async fn describe_node_sends_the_image_to_the_chat_model() {
    let (core, p, _) = fixture_project().await;
    patch(&core, &p, json!([
        {"op": "addNode", "id": "desc", "type": "describe", "data": {"title": "Describe"}},
        {"op": "addNode", "id": "gen", "type": "seedream", "data": {"title": "From description"}},
        {"op": "connect", "source": "desc", "target": "gen", "targetHandle": "prompt"}
    ])).await.unwrap();
    // Without an image it is blocked, with a reason, and nothing runs.
    let pl = plan(&core, &p, json!(["desc"]), false).await;
    assert_eq!(pl["blocked"], 1);
    // Text cannot go into its image port.
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "day-prompt", "target": "desc", "targetHandle": "image"}])).await.is_err());
    patch(&core, &p, json!([{"op": "connect", "source": "reference", "target": "desc", "targetHandle": "image"}])).await.unwrap();
    let pl = plan(&core, &p, json!(["gen"]), false).await;
    assert_eq!(pl["newTexts"], 1);
    assert_eq!(pl["newImages"], 1);
    let run = call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    assert!(run["results"].as_array().unwrap().iter().all(|r| r["status"] == "succeeded"), "{run}");
    let state = call(&core, "canvas_get", json!({"projectId": p})).await;
    assert!(state["results"]["desc"]["text"].as_str().unwrap().contains("Describe this image"));
}

#[tokio::test]
async fn nodes_hold_their_own_prompt_and_video_takes_an_audio_node() {
    use base64::Engine;
    let (core, p, _) = fixture_project().await;
    // An image node with its own prompt needs no prompt node.
    patch(&core, &p, json!([{"op": "addNode", "id": "solo", "type": "seedream", "data": {"title": "Solo"}}])).await.unwrap();
    assert_eq!(plan(&core, &p, json!(["solo"]), false).await["blocked"], 1, "an empty prompt blocks the run");
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "solo", "changes": {"prompt": "a red kite over dunes"}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["solo"]), false).await;
    assert_eq!(pl["blocked"], 0);
    assert_eq!(pl["items"][0]["finalPrompt"], "a red kite over dunes");

    // Voice file, an audio clip node, and a video node with its own prompt.
    let rate = 8000u32;
    let data = rate * 3 * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF"); wav.extend_from_slice(&(36 + data).to_le_bytes()); wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); wav.extend_from_slice(&1u16.to_le_bytes()); wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes()); wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data"); wav.extend_from_slice(&data.to_le_bytes());
    wav.resize(44 + data as usize, 7);
    let a = call(&core, "audio_import", json!({"projectId": p, "name": "line.wav", "base64": base64::engine::general_purpose::STANDARD.encode(&wav)})).await;
    patch(&core, &p, json!([
        {"op": "addNode", "id": "voice", "type": "audio", "data": {"title": "Line", "audioId": a["id"], "start": "0.5", "end": "2.5"}},
        {"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "prompt": "He greets the camera.", "settings": {"duration": "4"}}},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"},
        {"op": "connect", "source": "voice", "target": "vid", "targetHandle": "audio"}
    ])).await.unwrap();
    // Audio only fits the audio input.
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "voice", "target": "vid", "targetHandle": "refs"}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "reference", "target": "vid", "targetHandle": "audio"}])).await.is_err());
    let vp = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert!(vp["prompt"].as_str().unwrap().ends_with("He greets the camera."));
    assert_eq!(vp["params"]["_audio"], json!({"audioId": a["id"], "startMs": 500, "endMs": 2500}));
    assert_eq!(vp["inputs"][0]["role"], "reference_image");
}

#[test]
fn real_person_refusal_is_explained() {
    use seed_studio_lib::provider::{ErrKind, ProviderError};
    let mut e = ProviderError::new(ErrKind::BadRequest, "the input image 'content[1]' may contain real person");
    e.code = Some("InputImageSensitiveContentDetected.PrivacyInformation".into());
    let text = e.display("test-gateway", "dreamina-seedance-2-5-260628");
    assert!(text.contains("Seedream 5.0") && text.contains("Nothing was generated"), "{text}");
}

#[tokio::test]
async fn gateway_library_sends_faces_by_reference() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v3", server.server_addr());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for mut rq in server.incoming_requests() {
            let mut body = String::new();
            let _ = rq.as_reader().read_to_string(&mut body);
            seen2.lock().unwrap().push(format!("{} {} {}", rq.method(), rq.url(), body));
            let text = if rq.url().ends_with("/abpp/assets") {
                r#"{"assets":[{"id":"as-1","name":"Hero Front.png","status":"Active","group_id":"g1"},{"id":"as-2","name":"pending.png","status":"Processing"}]}"#
            } else if rq.url().ends_with("/abpp/balance") {
                r#"{"left":5}"#
            } else if rq.method().as_str() == "POST" && body.contains("crash the proxy") {
                let _ = rq.respond(tiny_http::Response::from_string("error code: 520").with_status_code(520));
                continue;
            } else if rq.method().as_str() == "POST" {
                r#"{"id":"task-1"}"#
            } else {
                r#"{"id":"task-1","status":"failed","error":{"code":"OutputAudioSensitiveContentDetected","message":"audio flagged"}}"#
            };
            let _ = rq.respond(tiny_http::Response::from_string(text));
        }
    });
    let (core, p, _) = fixture_project().await;
    dispatch(&core, "setting_set", json!({"key":"provider_mode","value":"modelark"})).await.unwrap();
    let core = with_test_gateway(core, &base);
    dispatch(&core, "setting_set", json!({"key":"region","value":"test-gateway"})).await.unwrap();
    dispatch(&core, "key_set", json!({"region":"test-gateway","key":"test-gateway-key"})).await.unwrap();
    *core.base_override.lock().unwrap() = Some(base);

    patch(&core, &p, json!([
        {"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "prompt": "He turns to the camera.", "settings": {"duration": "5", "sound": "off"}}},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"}
    ])).await.unwrap();

    // No match by name: the file itself would be sent, as before.
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["inputs"][0]["role"], "first_frame");
    assert!(plan["inputs"][0].get("library").is_none());
    // The library was read once for the plan, with the key as bearer and no generation.
    assert_eq!(seen.lock().unwrap().iter().filter(|l| l.starts_with("GET /abpp/assets")).count(), 1);

    // An entry that is not active yet cannot be picked.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "reference", "changes": {"library": "as-2"}}])).await.unwrap();
    assert_eq!(fail(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await, "bad_video_request");

    // Named like a library entry: matched without the extension or case, and sent by reference.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "reference", "changes": {"library": "", "title": "hero front"}}])).await.unwrap();
    let lib = call(&core, "gateway_library", json!({"projectId": p})).await;
    assert_eq!((lib["assets"].as_array().unwrap().len(), lib["byNode"]["reference"]["id"].clone()), (2, json!("as-1")));
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["inputs"][0]["role"], "reference_image");
    assert_eq!(plan["inputs"][0]["library"]["id"], "as-1");
    assert!(plan["prompt"].as_str().unwrap().starts_with("@Image 1 is the first frame"));
    assert_eq!(plan["params"]["generate_audio"], false);
    assert!(plan["notes"].to_string().contains("character library"));

    let v = call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    assert_eq!(v["status"], "queued");
    let post = seen.lock().unwrap().iter().find(|l| l.starts_with("POST")).cloned().unwrap();
    assert!(post.contains(r#""url":"asset://as-1""#) && post.contains("reference_image") && !post.contains("data:image"), "{post}");

    // A soundtrack refusal arrives with the task and is explained.
    let v = call(&core, "video_poll", json!({"videoId": plan["id"]})).await;
    assert_eq!(v["status"], "failed");
    assert!(v["error"].as_str().unwrap().contains("turn Sound off"), "{}", v["error"]);

    // "Off" on the node sends the file again.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "reference", "changes": {"library": "off"}}])).await.unwrap();
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["inputs"][0]["role"], "first_frame");

    // A proxy error on task creation (Cloudflare 520) is an unknown outcome, not a clean failure.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "vid", "changes": {"prompt": "crash the proxy"}}])).await.unwrap();
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    let v = call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    assert_eq!(v["status"], "uncertain", "{v}");
    assert!(v["error"].as_str().unwrap().contains("not known whether"), "{}", v["error"]);
}

#[tokio::test]
async fn new_reference_images_are_added_to_the_gateway_library_first() {
    seed_studio_lib::library::POLL_MS.store(20, std::sync::atomic::Ordering::Relaxed);
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v3", server.server_addr());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        let mut polls = 0;
        for mut rq in server.incoming_requests() {
            let mut body = Vec::new();
            let _ = rq.as_reader().read_to_end(&mut body);
            let attested = rq.headers().iter().any(|h| h.field.equiv("x-abpp-ai-generated") && h.value == "true");
            let kind = rq.headers().iter().find(|h| h.field.equiv("content-type")).map(|h| h.value.to_string()).unwrap_or_default();
            let url = rq.url().to_string();
            seen2.lock().unwrap().push(format!("{} {} attested={attested} type={kind} bytes={} {}", rq.method(), url, body.len(), if url.contains("/tasks") { String::from_utf8_lossy(&body).to_string() } else { String::new() }));
            let (code, text) = if url.contains("/abpp/assets/upload") {
                if !attested { (400, r#"{"error":{"code":"abpp_attestation_required","message":"declare it"}}"#.to_string()) }
                else { (200, r#"{"id":"as-new","uri":"asset://as-new","group_id":"auto","status":"Processing"}"#.to_string()) }
            } else if url.ends_with("/abpp/assets/as-new") {
                polls += 1;
                (200, format!(r#"{{"id":"as-new","status":"{}"}}"#, if polls >= 2 { "Active" } else { "Processing" }))
            } else if url.ends_with("/abpp/assets") {
                (200, if polls >= 2 { r#"{"assets":[{"id":"as-new","name":"Reference bottle","status":"Active"}]}"# } else { r#"{"assets":[]}"# }.to_string())
            } else {
                (200, r#"{"id":"task-9"}"#.to_string())
            };
            let _ = rq.respond(tiny_http::Response::from_string(text).with_status_code(code));
        }
    });
    let (core, p, _) = fixture_project().await;
    dispatch(&core, "setting_set", json!({"key":"provider_mode","value":"modelark"})).await.unwrap();
    let core = with_test_gateway(core, &base);
    dispatch(&core, "setting_set", json!({"key":"region","value":"test-gateway"})).await.unwrap();
    dispatch(&core, "key_set", json!({"region":"test-gateway","key":"test-gateway-key"})).await.unwrap();
    *core.base_override.lock().unwrap() = Some(base);
    patch(&core, &p, json!([
        {"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "prompt": "He turns.", "settings": {"duration": "5"}}},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"}
    ])).await.unwrap();
    let uploads = |seen: &Arc<Mutex<Vec<String>>>| seen.lock().unwrap().iter().filter(|l| l.contains("/abpp/assets/upload")).count();

    // Switch off (the default): the file is sent as before and nothing is uploaded.
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert!(plan["inputs"][0].get("library").is_none());
    assert_eq!(fail(&core, "setting_set", json!({"key":"library_upload:ap-southeast","value":"1"})).await, "bad_args");

    // Switch on: the plan says what will be added; planning itself uploads nothing.
    let st = call(&core, "setting_set", json!({"key":"library_upload:test-gateway","value":"1"})).await;
    assert_eq!(st["libraryUpload"], true);
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["inputs"][0]["library"]["pending"], true);
    assert_eq!(plan["inputs"][0]["role"], "reference_image");
    assert!(plan["notes"].to_string().contains("declared AI-generated"), "{}", plan["notes"]);
    assert_eq!(uploads(&seen), 0);

    // Sending: one upload with the declaration, wait until Active, then the task names it by reference.
    let v = call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    assert_eq!(v["status"], "queued", "{v}");
    assert_eq!(v["inputs"][0]["library"]["id"], "as-new");
    let log = seen.lock().unwrap().clone();
    let up = log.iter().find(|l| l.contains("/abpp/assets/upload")).unwrap();
    assert!(up.starts_with("POST") && up.contains("name=") && up.contains("attested=true") && up.contains("type=image/") && !up.contains("bytes=0 "), "{up}");
    let task = log.iter().find(|l| l.contains("/tasks")).unwrap();
    assert!(task.contains(r#""url":"asset://as-new""#) && !task.contains("data:image"), "{task}");
    assert!(log.iter().position(|l| l.contains("/abpp/assets/as-new")).unwrap() < log.iter().position(|l| l.contains("/tasks")).unwrap());

    // The next video finds it in the library: no second upload.
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["inputs"][0]["library"]["id"], "as-new");
    call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    assert_eq!(uploads(&seen), 1);

    // "Don't use" on the node keeps that image out of the library.
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "reference", "changes": {"library": "off"}}])).await.unwrap();
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert!(plan["inputs"][0].get("library").is_none());
    // The node's own button: an image already there is not uploaded again; with the switch off it refuses.
    let bottle = canvas(&core, &p).await["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "reference").unwrap()["data"]["assetId"].clone();
    let lib = call(&core, "library_add", json!({"projectId": p, "assetId": bottle})).await;
    assert_eq!((uploads(&seen), lib["assets"].as_array().unwrap().len()), (1, 1));
    call(&core, "setting_set", json!({"key":"library_upload:test-gateway","value":"0"})).await;
    let other = call(&core, "assets_list", json!({"projectId": p})).await.as_array().unwrap().iter().map(|a| a["id"].clone()).find(|id| *id != bottle);
    if let Some(id) = other {
        assert_eq!(fail(&core, "library_add", json!({"projectId": p, "assetId": id})).await, "library_upload");
        call(&core, "setting_set", json!({"key":"library_upload:test-gateway","value":"1"})).await;
        call(&core, "library_add", json!({"projectId": p, "assetId": id})).await;
        assert_eq!(uploads(&seen), 2);
    }
}

#[tokio::test]
async fn an_older_version_can_be_made_current_without_a_request() {
    let (core, p, _) = fixture_project().await;
    let run = |force: bool| { let (core, p) = (&core, &p); async move {
        let pl = plan(core, p, json!(["day-image"]), force).await;
        call(core, "run_execute", json!({"planId": pl["id"]})).await;
    }};
    run(false).await;
    run(true).await;
    let c = canvas(&core, &p).await;
    let versions = c["results"]["day-image"]["versions"].as_array().unwrap().clone();
    assert_eq!(versions.len(), 2);
    assert_eq!(c["results"]["day-image"]["assetId"], versions[1]);
    // Going back to the first version: it becomes current, no version is added, nothing is sent.
    let c = call(&core, "node_use_version", json!({"projectId": p, "nodeId": "day-image", "assetId": versions[0]})).await;
    assert_eq!(c["results"]["day-image"]["assetId"], versions[0]);
    assert_eq!(c["results"]["day-image"]["versions"].as_array().unwrap().len(), 2);
    // A later run reuses the chosen version instead of generating.
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    assert_eq!(pl["newImages"], 0);
    // A version of another node is refused; saving an older version to disk works.
    assert_eq!(fail(&core, "node_use_version", json!({"projectId": p, "nodeId": "night-image", "assetId": versions[0]})).await, "not_found");
    std::env::set_var("SEED_STUDIO_EXPORT_DIR", temp_dir());
    let x = call(&core, "asset_export", json!({"assetId": versions[1]})).await;
    assert!(std::path::Path::new(x["path"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn notes_and_groups_organise_the_canvas_without_running() {
    let (core, p, _) = fixture_project().await;
    let before = nodes(&canvas(&core, &p).await);
    patch(&core, &p, json!([
        {"op": "addNode", "id": "n1", "type": "note", "position": {"x": 0, "y": -200}, "data": {"title": "Status", "text": "Shots 1–5 done", "color": "green"}},
        {"op": "addNode", "id": "g1", "type": "group", "position": {"x": -40, "y": -60}, "data": {"title": "Done", "color": "green", "width": 900, "height": 600}}
    ])).await.unwrap();
    assert_eq!(nodes(&canvas(&core, &p).await), before + 2);
    // Closed schema: unknown colours, sizes out of range and connections are refused.
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "n1", "changes": {"color": "pink"}}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "g1", "changes": {"width": 5}}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "connect", "source": "n1", "target": "day-image", "targetHandle": "prompt"}])).await.is_err());
    // They never add a request to a run.
    let pl = plan(&core, &p, json!(["n1", "g1", "day-image"]), false).await;
    assert_eq!(pl["newImages"], 1);
    // A colour tag on a generation node is only a mark: allowed, checked, and it does not make the saved result stale.
    let fp = |pl: &Value| pl["items"].as_array().unwrap().iter().find(|i| i["nodeId"] == "day-image").unwrap()["fingerprint"].clone();
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"color": "green"}}])).await.unwrap();
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"color": "pink"}}])).await.is_err());
    assert!(fp(&pl).is_string());
    assert_eq!(fp(&plan(&core, &p, json!(["day-image"]), false).await), fp(&pl));
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "day-image", "changes": {"color": null}}])).await.unwrap();
    call(&core, "canvas_undo", json!({"projectId": p})).await;
    call(&core, "canvas_undo", json!({"projectId": p})).await;
    // One undo removes both.
    call(&core, "canvas_undo", json!({"projectId": p})).await;
    assert_eq!(nodes(&canvas(&core, &p).await), before);
}

#[tokio::test]
async fn video_prompt_points_at_the_attachments_by_tag() {
    let (core, p, _) = fixture_project().await;
    let rate = 8000u32;
    let mut wav = Vec::new();
    let data = rate * 3 * 2;
    wav.extend_from_slice(b"RIFF"); wav.extend_from_slice(&(36 + data).to_le_bytes()); wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); wav.extend_from_slice(&1u16.to_le_bytes()); wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes()); wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data"); wav.extend_from_slice(&data.to_le_bytes());
    wav.extend_from_slice(&vec![0u8; data as usize]);
    use base64::Engine;
    let a = call(&core, "audio_import", json!({"projectId": p, "name": "line.wav", "base64": base64::engine::general_purpose::STANDARD.encode(&wav)})).await;
    let card = json!({
        "shot": "Safety line", "model": "Seedance", "duration": "4s", "aspect_ratio": "16:9",
        "input": "Use the connected image as the exact first frame. Use the connected audio clip as his real voice.",
        "spoken_transcript": "Fire in the pit, never on the sand.",
        "prompt": "He turns to the lens and speaks. Match the supplied frame throughout",
        "camera": {"move": "very slow push in", "height": "locked"}
    }).to_string();
    patch(&core, &p, json!([
        {"op": "addNode", "id": "clip", "type": "audio", "data": {"title": "Line", "audioId": a["id"], "start": "0", "end": "3"}},
        {"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "prompt": card, "settings": {"duration": "5"}}},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"},
        {"op": "connect", "source": "clip", "target": "vid", "targetHandle": "audio"}
    ])).await.unwrap();
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    let text = plan["prompt"].as_str().unwrap();
    assert!(text.starts_with("@Image 1 is the first frame of the video: open on exactly this image"), "{text}");
    assert!(text.contains("@Audio 1 is the voice of the person in @Image 1"), "{text}");
    // The card became sentences; its settings fields are gone; loose phrases name the attachments.
    assert!(!text.contains('{') && !text.contains("aspect_ratio") && !text.contains("4s") && !text.contains("Seedance,"), "{text}");
    assert!(text.contains("Use @Image 1 as the exact first frame. Use @Audio 1 as his real voice."), "{text}");
    assert!(text.contains("The spoken line is: \"Fire in the pit, never on the sand.\"") && text.contains("Match @Image 1 throughout."), "{text}");
    assert!(text.contains("Camera: ") && text.contains("very slow push in"), "{text}");
    assert_eq!(plan["notes"].as_array().unwrap().iter().filter(|n| n.as_str().unwrap().contains("JSON card") || n.as_str().unwrap().contains("@Image 1 / @Audio 1")).count(), 2);
    // A plain prompt on a silent first-frame shot is left exactly as written.
    patch(&core, &p, json!([
        {"op": "disconnect", "source": "clip", "target": "vid", "targetHandle": "audio"},
        {"op": "setNodeData", "nodeId": "vid", "changes": {"prompt": "He turns. Match the supplied frame."}}
    ])).await.unwrap();
    let plan = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(plan["prompt"], "He turns. Match the supplied frame.");
    assert_eq!(plan["inputs"][0]["role"], "first_frame");
}

#[tokio::test]
async fn past_generations_give_time_and_size_estimates() {
    let (core, p, _) = fixture_project().await;
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    patch(&core, &p, json!([{"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "prompt": "waves", "settings": {"duration": "5"}}}])).await.unwrap();
    let vp = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    call(&core, "video_start", json!({"videoId": vp["id"]})).await;
    for _ in 0..200 {
        if call(&core, "video_poll", json!({"videoId": vp["id"]})).await["status"] == "succeeded" { break; }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let s = call(&core, "gen_stats", json!({"projectId": p})).await;
    assert_eq!(s["video"]["draft:480p"]["n"], 1, "{s}");
    assert!(s["video"]["draft:480p"]["secs"].as_f64().is_some());
    // Simulation results never count as spending.
    assert_eq!(s["project"]["videos"], 0);
    // The canvas says when a run started, so a node can show its progress.
    assert!(canvas(&core, &p).await["results"]["day-image"]["startedAt"].as_i64().unwrap() > 0);
    // Prices are the owner's own numbers and are validated.
    assert_eq!(fail(&core, "setting_set", json!({"key": "price_video_per_m", "value": "abc"})).await, "bad_args");
    let st = call(&core, "setting_set", json!({"key": "price_video_per_m", "value": "2.5"})).await;
    assert_eq!(st["prices"]["videoPerM"], 2.5);
}

#[tokio::test]
async fn elements_are_called_into_prompts_with_at() {
    let (core, p, _) = fixture_project().await;
    let bottle = canvas(&core, &p).await["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "reference").unwrap()["data"]["assetId"].clone();
    let list = call(&core, "element_save", json!({"projectId": p, "kind": "prop", "name": "Amber Bottle", "assetId": bottle, "description": "square amber glass bottle with a black cap"})).await;
    assert_eq!(list[0]["handle"], "AmberBottle");
    call(&core, "element_save", json!({"projectId": p, "kind": "environment", "name": "Majlis", "description": "a red-carpeted Bedouin tent at blue hour"})).await;
    // Names are unique per project and cannot shadow Seedance's own tags.
    assert_eq!(fail(&core, "element_save", json!({"projectId": p, "kind": "prop", "name": "amber bottle"})).await, "bad_element");
    assert_eq!(fail(&core, "element_save", json!({"projectId": p, "kind": "prop", "name": "Image 1"})).await, "bad_element");

    // Image node: the element's picture becomes a reference and the prompt names it by position.
    patch(&core, &p, json!([{"op": "addNode", "id": "img", "type": "seedream", "data": {"title": "Ad", "prompt": "@amberbottle on a table in @Majlis, close-up"}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["img"]), false).await;
    let item = pl["items"].as_array().unwrap().iter().find(|i| i["nodeId"] == "img").unwrap().clone();
    let text = item["finalPrompt"].as_str().unwrap_or_else(|| panic!("{item}"));
    assert!(text.starts_with("Amber Bottle (reference image 1) on a table in Majlis, close-up"), "{text}");
    assert!(text.contains("Amber Bottle: square amber glass bottle with a black cap.") && text.contains("Majlis: a red-carpeted Bedouin tent at blue hour."), "{text}");

    // Video node with a first frame: everything goes in as references and the tags line up.
    patch(&core, &p, json!([
        {"op": "addNode", "id": "vid", "type": "seedance", "data": {"title": "Shot", "prompt": "The camera circles @AmberBottle.", "settings": {"duration": "5"}}},
        {"op": "connect", "source": "reference", "target": "vid", "targetHandle": "first"}
    ])).await.unwrap();
    let vp = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    let roles: Vec<&str> = vp["inputs"].as_array().unwrap().iter().map(|i| i["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["reference_image", "reference_image"]);
    let text = vp["prompt"].as_str().unwrap();
    assert!(text.starts_with("@Image 1 is the first frame of the video") && text.contains("The camera circles Amber Bottle (@Image 2)."), "{text}");

    // Deleting an element leaves the mention as plain text.
    let id = list[0]["id"].clone();
    call(&core, "element_delete", json!({"projectId": p, "id": id})).await;
    let vp = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "vid"})).await;
    assert_eq!(vp["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(vp["inputs"][0]["role"], "first_frame");
}

#[tokio::test]
async fn camera_and_colour_are_copied_without_copying_the_picture() {
    let (core, p, _) = fixture_project().await;
    let png = |rgb: [u8; 3]| {
        let mut buf = std::io::Cursor::new(vec![]);
        image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([rgb[0].saturating_add((x * 2) as u8), rgb[1].saturating_add(y as u8), rgb[2]])).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    };
    let mean = |bytes: &[u8]| {
        let img = image::load_from_memory(bytes).unwrap().to_rgb8();
        let n = (img.width() * img.height()) as f32;
        [0, 1, 2].map(|k| img.pixels().map(|p| p[k] as f32).sum::<f32>() / n)
    };
    // Colour: a blue picture takes the warm grade of an orange one; size and pixel layout stay.
    let (blue, orange) = (png([20, 40, 150]), png([170, 90, 20]));
    let out = seed_studio_lib::look::match_colour(&blue, &orange, 1.0).unwrap();
    let (m, want) = (mean(&out), mean(&orange));
    assert!(m[0] > m[2] && (m[0] - want[0]).abs() < 25.0, "matched mean {m:?} vs {want:?}");
    let img = image::load_from_memory(&out).unwrap().to_rgb8();
    assert_eq!((img.width(), img.height()), (64, 48));
    assert!(img.get_pixel(60, 10)[0] > img.get_pixel(2, 10)[0], "the gradient is still there");
    // Strength 0 changes nothing.
    assert_eq!(mean(&seed_studio_lib::look::match_colour(&blue, &orange, 0.0).unwrap()), mean(&blue));
    let strip = image::load_from_memory(&seed_studio_lib::look::palette(&orange).unwrap()).unwrap();
    assert_eq!((strip.width(), strip.height()), (1024, 256));

    // On a node: run it, then match its colour to the bottle. A new version, no request.
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    let st = canvas(&core, &p).await;
    let before = st["results"]["night-image"]["assetId"].as_str().unwrap().to_string();
    let bottle = st["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "reference").unwrap()["data"]["assetId"].as_str().unwrap().to_string();
    let st = call(&core, "colour_match", json!({"projectId": p, "nodeId": "night-image", "fromAssetId": bottle, "strength": 1.0})).await;
    let r = &st["results"]["night-image"];
    assert_ne!(r["assetId"], before);
    assert_eq!(r["versions"].as_array().unwrap().len(), 2);
    // The matched version counts as the saved result: running again asks for nothing.
    assert_eq!(plan(&core, &p, json!(["night-image"]), false).await["newImages"], 0);
    assert!(seed_studio_lib::look::colour_version(&core, &p, "night-image", r["assetId"].as_str().unwrap(), 1.0).is_err());
    let pal = call(&core, "palette_asset", json!({"projectId": p, "fromAssetId": bottle})).await;
    assert!(pal["title"].as_str().unwrap().starts_with("Palette of"));

    // Camera: one text request; its text rides in the prompt, the source image is not a reference.
    let cam = call(&core, "camera_describe", json!({"projectId": p, "assetId": bottle})).await;
    assert!(!cam["text"].as_str().unwrap().is_empty());
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-image", "changes": {"camera": "Low angle, wide lens, subject in the right third."}}])).await.unwrap();
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    let item = pl["items"].as_array().unwrap().iter().find(|i| i["nodeId"] == "night-image").unwrap().clone();
    assert_eq!(pl["newImages"], 1, "a new camera makes the saved result stale");
    assert!(item["finalPrompt"].as_str().unwrap().ends_with("Low angle, wide lens, subject in the right third."));
    assert_eq!(item["refs"].as_array().unwrap().len(), 1);
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "night-prompt", "changes": {"camera": "x"}}])).await.is_err());
}

/// A stand-in for the owner's Supabase project: sign-in, profiles, sessions with the 8-seat rule, the admin function.
fn fake_supabase() -> (String, Arc<Mutex<Vec<String>>>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        let uid = |n: usize| format!("00000000-0000-4000-8000-{n:012}");
        let mut members: Vec<String> = vec![];
        let mut sessions = 0usize;
        let mut refreshes = 0usize;
        for mut rq in server.incoming_requests() {
            let mut body = String::new();
            let _ = rq.as_reader().read_to_string(&mut body);
            let auth = rq.headers().iter().find(|h| h.field.equiv("authorization")).map(|h| h.value.to_string()).unwrap_or_default();
            let (method, url) = (rq.method().as_str().to_string(), rq.url().to_string());
            log.lock().unwrap().push(format!("{method} {url} [{auth}] {body}"));
            let b: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let session = |n: usize| json!({"access_token": format!("access-{n}"), "refresh_token": format!("refresh-{n}"), "expires_in": 3600, "user": {"id": uid(1), "email": "owner@example.test"}}).to_string();
            let (code, text) = if url.starts_with("/auth/v1/settings") { (200, "{}".to_string()) }
                else if url.contains("grant_type=password") {
                    if b["password"] == "correct horse" { (200, session(0)) } else { (400, r#"{"error_code":"invalid_credentials","msg":"Invalid login credentials"}"#.into()) }
                }
                else if url.contains("grant_type=refresh_token") { refreshes += 1; (200, session(refreshes)) }
                else if !auth.starts_with("Bearer access-") { (401, r#"{"message":"JWT expired"}"#.into()) }
                else if url.starts_with("/rest/v1/profiles") && method == "GET" {
                    let disabled = b["x"].is_string();
                    let all: Vec<Value> = (1..=10).map(|n| json!({"id": uid(n), "email": format!("u{n}@example.test"), "name": format!("User {n}"), "role": if n == 1 { "admin" } else { "user" }, "disabled": disabled})).collect();
                    (200, if url.contains("id=eq.") { json!([all[0]]).to_string() } else { json!(all).to_string() })
                }
                else if url.starts_with("/rest/v1/workspaces") && method == "POST" { sessions += 1; members = vec![uid(1)]; (201, "[]".into()) }
                else if url.starts_with("/rest/v1/workspaces") && method == "GET" {
                    let list: Vec<Value> = (0..sessions).map(|_| json!({"id": "11111111-1111-4111-8111-111111111111", "name": "Film", "owner": uid(1),
                        "workspace_members": members.iter().map(|m| json!({"user_id": m, "role": if *m == uid(1) { "owner" } else { "member" }})).collect::<Vec<_>>()})).collect();
                    (200, json!(list).to_string())
                }
                else if url.starts_with("/rest/v1/workspace_members") && method == "POST" {
                    if members.len() >= 8 { (400, r#"{"code":"P0001","message":"seat_limit: a shared session holds at most 8 people"}"#.into()) }
                    else { members.push(b["user_id"].as_str().unwrap().to_string()); (201, "[]".into()) }
                }
                else if url.starts_with("/rest/v1/workspace_members") && method == "DELETE" {
                    let before = members.len();
                    members.retain(|m| !url.contains(m.as_str()) || *m == uid(1));
                    (200, if members.len() < before { "[{}]".into() } else { "[]".to_string() })
                }
                else if url.starts_with("/functions/v1/admin") { (200, json!({"user": {"id": uid(11), "email": b["email"], "name": b["name"], "role": "user", "disabled": false}}).to_string()) }
                else { (200, "{}".into()) };
            let _ = rq.respond(tiny_http::Response::from_string(text).with_status_code(code));
        }
    });
    (base, seen)
}

#[tokio::test]
async fn team_sign_in_seats_and_admin_keep_secrets_off_disk() {
    let (core, _, _) = fixture_project().await;
    let (base, seen) = fake_supabase();
    let uid = |n: usize| format!("00000000-0000-4000-8000-{n:012}");
    let jwt = |role: &str| format!("eyJhbGciOiJIUzI1NiJ9.{}.signature-signature", base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, json!({"role": role}).to_string()));

    // Nothing is set up: the state says so and nothing is asked of anyone.
    assert_eq!(call(&core, "cloud_state", json!({})).await["configured"], false);
    assert_eq!(fail(&core, "cloud_sign_in", json!({"email": "a@b.c", "password": "x"})).await, "cloud_not_set");
    // The secret service key is refused and not saved; the publishable key is accepted.
    assert_eq!(fail(&core, "cloud_configure", json!({"url": base, "key": jwt("service_role")})).await, "secret_key");
    assert_eq!(fail(&core, "cloud_configure", json!({"url": "ftp://x", "key": jwt("anon")})).await, "bad_url");
    assert_eq!(call(&core, "cloud_configure", json!({"url": format!("{base}/"), "key": jwt("anon")})).await["configured"], true);

    assert_eq!(fail(&core, "cloud_sign_in", json!({"email": "owner@example.test", "password": "wrong"})).await, "bad_login");
    assert!(core.secrets.get("cloud:session").unwrap().is_none());
    let st = call(&core, "cloud_sign_in", json!({"email": "owner@example.test", "password": "correct horse"})).await;
    assert_eq!(st["user"]["role"], "admin");
    assert!(st.to_string().find("access-0").is_none(), "the session never reaches the interface");
    // The session is in the secret store; the database holds neither token nor password.
    assert!(core.secrets.get("cloud:session").unwrap().unwrap().contains("refresh-0"));
    let dump: String = core.db(|c| { let mut st = c.prepare("SELECT key || '=' || value FROM settings")?; let rows: Vec<String> = st.query_map([], |r| r.get(0))?.filter_map(|r| r.ok()).collect(); Ok(rows.join("\n")) }).unwrap();
    assert!(!dump.contains("access-0") && !dump.contains("refresh-0") && !dump.contains("correct horse"));

    // An expired session is refreshed once, then used.
    core.secrets.set("cloud:session", &json!({"access": "access-0", "refresh": "refresh-0", "exp": 0}).to_string()).unwrap();
    let people = call(&core, "cloud_people", json!({})).await;
    assert_eq!(people["users"].as_array().unwrap().len(), 10);
    assert_eq!(seen.lock().unwrap().iter().filter(|l| l.contains("grant_type=refresh_token")).count(), 1);
    assert!(seen.lock().unwrap().last().unwrap().contains("Bearer access-1"));

    // A shared session: the owner has the first seat, seven more fit, the ninth person is refused.
    let list = call(&core, "cloud_session_create", json!({"name": "Film"})).await;
    let sid = list[0]["id"].as_str().unwrap().to_string();
    for n in 2..=8 { call(&core, "cloud_member_add", json!({"sessionId": sid, "userId": uid(n)})).await; }
    assert_eq!(fail(&core, "cloud_member_add", json!({"sessionId": sid, "userId": uid(9)})).await, "seat_limit");
    let list = call(&core, "cloud_member_remove", json!({"sessionId": sid, "userId": uid(8)})).await;
    assert_eq!(list[0]["workspace_members"].as_array().unwrap().len(), 7);
    assert_eq!(fail(&core, "cloud_member_remove", json!({"sessionId": sid, "userId": uid(1)})).await, "forbidden");
    assert_eq!(fail(&core, "cloud_member_add", json!({"sessionId": "x' or 1=1", "userId": uid(2)})).await, "bad_args");

    // Accounts are made only through the admin function; extra fields are not passed on.
    let made = call(&core, "cloud_admin", json!({"action": "create_user", "email": "new@example.test", "name": "New", "password": "temporary-pass-1", "role": "user", "service_key": "nope"})).await;
    assert_eq!(made["user"]["email"], "new@example.test");
    assert!(!seen.lock().unwrap().last().unwrap().contains("service_key"));
    assert_eq!(fail(&core, "cloud_admin", json!({"action": "drop_everything"})).await, "bad_args");
    assert_eq!(fail(&core, "cloud_change_password", json!({"password": "short"})).await, "weak_password");

    // Signing out forgets the session on this Mac.
    assert!(call(&core, "cloud_sign_out", json!({})).await["user"].is_null());
    assert!(core.secrets.get("cloud:session").unwrap().is_none());
    assert_eq!(fail(&core, "cloud_people", json!({})).await, "signed_out");
}

#[tokio::test]
async fn every_provider_request_is_logged_without_key_or_content() {
    let (core, p, seen, dir) = live_like("ok").await;
    let secret = "test-only-log-credential";
    call(&core, "key_set", json!({"region": "ap-southeast-1", "key": secret})).await;
    assert_eq!(call(&core, "api_log_list", json!({})).await["total"], 0, "saving a key sends nothing");
    call(&core, "check_connection", json!({})).await;
    let t = call(&core, "threads_list", json!({"projectId": p})).await[0]["id"].as_str().unwrap().to_string();
    call(&core, "chat_send", json!({"threadId": t, "text": "سر العطر الأزرق"})).await;
    call(&core, "chat_send", json!({"threadId": t, "text": "اختبار انقطاع"})).await;
    let pl = plan(&core, &p, json!(["day-image"]), false).await;
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;
    // A server that cannot be reached: the request never left.
    *core.base_override.lock().unwrap() = Some("http://127.0.0.1:9/api/v3".into());
    let pl = plan(&core, &p, json!(["night-image"]), false).await;
    call(&core, "run_execute", json!({"planId": pl["id"]})).await;

    let log = call(&core, "api_log_list", json!({})).await;
    let rows = log["rows"].as_array().unwrap();
    let seq: Vec<(&str, &str)> = rows.iter().rev().map(|r| (r["purpose"].as_str().unwrap(), r["outcome"].as_str().unwrap())).collect();
    assert_eq!(seq, [("models", "ok"), ("chat", "ok"), ("chat", "partial"), ("image", "ok"), ("image", "not_sent")]);
    let image = &rows[1];
    assert_eq!((image["method"].clone(), image["path"].clone(), image["httpStatus"].clone()), (json!("POST"), json!("/api/v3/images/generations"), json!(200)));
    assert_eq!(image["connection"], "ap-southeast-1");
    assert_eq!(image["usage"]["generated_images"], 1);
    assert_eq!(image["params"]["input_images"], 1, "references are counted, not kept");
    let sent_bytes = seen.0.lock().unwrap().iter().find(|(path, _, _)| path.ends_with("/images/generations")).unwrap().2.to_string().len();
    assert!(image["params"]["prompt_chars"].as_u64().unwrap() > 0);
    assert_eq!(image["bytesOut"].as_u64().unwrap(), sent_bytes as u64);
    assert!(image["model"].as_str().unwrap().contains("seedream"));
    assert_eq!(rows[3]["params"]["messages_count"].as_u64().is_some(), true);
    assert_eq!(rows[0]["httpStatus"], Value::Null, "a request that never left has no status");
    assert_eq!(call(&core, "api_log_list", json!({"paidOnly": true})).await["total"], 4, "status checks and lists are not generation requests");

    // Simulation never reaches the network, so it adds nothing here.
    call(&core, "setting_set", json!({"key": "provider_mode", "value": "mock"})).await;
    call(&core, "chat_send", json!({"threadId": t, "text": "محاكاة"})).await;
    assert_eq!(call(&core, "api_log_list", json!({})).await["total"], 5);

    // The export holds neither the key nor anything that was written or drawn.
    std::env::set_var("SEED_STUDIO_EXPORT_DIR", temp_dir());
    let out = call(&core, "api_log_export", json!({})).await;
    assert_eq!(out["rows"], 5);
    let csv = std::fs::read_to_string(out["path"].as_str().unwrap()).unwrap();
    assert_eq!(csv.lines().count(), 6);
    assert!(csv.contains("/api/v3/images/generations") && csv.contains("Z\","), "paths and UTC times are there");
    for text in [csv.as_str(), log.to_string().as_str()] {
        for hidden in [secret, "Bearer", "العطر", "انقطاع", "morning light", "base64"] {
            assert!(!text.contains(hidden), "the log must not contain {hidden}");
        }
    }

    // A request that was in flight when the app closed is shown as unknown, not as sent or failed.
    core.db(|c| { c.execute("INSERT INTO api_log(started_at,purpose,method,host,path,connection,outcome) VALUES(1,'video_create','POST','h','/p','test-gateway','sending')", [])?; Ok(()) }).unwrap();
    drop(core);
    let core = open(&dir);
    let rows = call(&core, "api_log_list", json!({})).await["rows"].clone();
    assert_eq!((rows[0]["purpose"].clone(), rows[0]["outcome"].clone()), (json!("video_create"), json!("unknown")));
    assert_eq!(seed_studio_lib::apilog::iso(1_794_919_477_541), "2026-11-17T12:44:37Z");
}

#[tokio::test]
async fn seedance_flash_validates_and_keeps_its_model_through_generation() {
    let (core, p, _) = fixture_project().await;
    let model = "seedance-2.5-flash";
    let bottle = call(&core, "assets_list", json!({"projectId": p})).await[0]["id"].clone();
    for invalid in [json!({"draft": true}), json!({"resolution": "1080p"}), json!({"resolution": "4k"}), json!({"duration": 31})] {
        let mut args = json!({"projectId": p, "modelKey": model, "prompt": "A bottle spins"});
        args.as_object_mut().unwrap().extend(invalid.as_object().unwrap().clone());
        assert_eq!(fail(&core, "video_plan", args).await, "bad_video_request");
    }
    let refs: Vec<Value> = (0..11).map(|_| json!({"assetId": bottle, "role": "reference_image"})).collect();
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "modelKey": model, "prompt": "A bottle spins", "inputs": refs})).await, "bad_video_request");
    let plan = call(&core, "video_plan", json!({"projectId": p, "modelKey": model, "prompt": "A bottle spins", "duration": 30, "inputs": [{"assetId": bottle, "role": "first_frame"}], "ratio": "16:9"})).await;
    assert_eq!(plan["modelId"], "dreamina-seedance-2-5-flash-261115");
    assert_eq!(plan["modelKey"], model);
    assert_eq!(plan["kind"], "final");
    assert_eq!(plan["params"], json!({"duration": 30, "ratio": "adaptive", "resolution": "720p"}));
    call(&core, "video_start", json!({"videoId": plan["id"]})).await;
    let sent = core.mock.video_log.lock().unwrap()[0].clone();
    assert_eq!(sent["model"], "dreamina-seedance-2-5-flash-261115");
    assert!(sent.get("draft").is_none());
    let done = video_until_done(&core, plan["id"].as_str().unwrap()).await;
    assert_eq!(done["modelKey"], model);
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "draftOf": done["id"]})).await, "bad_video_request");
}

#[tokio::test]
async fn canvas_video_model_switch_normalizes_settings_and_preserves_draft_model() {
    let (core, p, _) = fixture_project().await;
    patch(&core, &p, json!([
        {"op": "addNode", "id": "legacy-video", "type": "seedance", "data": {"prompt": "A bottle turns"}},
        {"op": "addNode", "id": "flash-video", "type": "seedance", "data": {"modelKey": "seedance-2.5-flash", "prompt": "The sun rises"}}
    ])).await.unwrap();
    let flash = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "flash-video"})).await;
    assert_eq!(flash["kind"], "final");
    assert_eq!(flash["params"]["resolution"], "720p");
    assert_eq!(flash["modelKey"], "seedance-2.5-flash");
    let draft = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "legacy-video"})).await;
    assert_eq!(draft["kind"], "draft");
    assert_eq!(draft["modelKey"], "seedance-2.5");
    call(&core, "video_start", json!({"videoId": draft["id"]})).await;
    video_until_done(&core, draft["id"].as_str().unwrap()).await;
    patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "legacy-video", "changes": {"modelKey": "seedance-2.5-flash"}}])).await.unwrap();
    let switched = call(&core, "video_plan_node", json!({"projectId": p, "nodeId": "legacy-video"})).await;
    assert_eq!(switched["modelKey"], "seedance-2.5-flash");
    assert_eq!(switched["kind"], "final");
    assert_eq!(switched["params"]["resolution"], "720p");
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "flash-video", "changes": {"settings": {"pass": "draft"}}}])).await.is_err());
    assert!(patch(&core, &p, json!([{"op": "setNodeData", "nodeId": "flash-video", "changes": {"settings": {"resolution": "1080p"}}}])).await.is_err());
    let upscale = call(&core, "video_plan", json!({"projectId": p, "draftOf": draft["id"], "resolution": "1080p"})).await;
    assert_eq!(upscale["modelId"], draft["modelId"]);
    assert_eq!(upscale["modelKey"], "seedance-2.5");
    assert_eq!(fail(&core, "video_plan", json!({"projectId": p, "draftOf": draft["id"], "modelKey": "seedance-2.5-flash", "resolution": "720p"})).await, "bad_video_request");
}

#[tokio::test]
async fn videos_keep_their_planned_connection_when_settings_change() {
    let (core, p, seen, _) = live_like("ok").await;
    for region in ["ap-southeast-1", "eu-west-1"] {
        call(&core, "key_set", json!({"region": region, "key": "test-region-key"})).await;
    }
    let planned = call(&core, "video_plan", json!({
        "projectId": p, "modelKey": "seedance-2.5-flash", "prompt": "A bottle turns"
    })).await;
    call(&core, "setting_set", json!({"key": "region", "value": "eu-west-1"})).await;
    assert_eq!(fail(&core, "video_start", json!({"videoId": planned["id"]})).await, "bad_video_request");
    assert!(seen.0.lock().unwrap().is_empty(), "a changed connection must not receive the paid request");

    let queued = call(&core, "video_plan", json!({
        "projectId": p, "modelKey": "seedance-2.5-flash", "prompt": "A bottle turns"
    })).await;
    core.db(|c| {
        c.execute("UPDATE videos SET status='queued', task_id='existing-provider-task' WHERE id=?1", [queued["id"].as_str().unwrap()])?;
        Ok(())
    }).unwrap();
    call(&core, "setting_set", json!({"key": "region", "value": "ap-southeast-1"})).await;
    let pending = call(&core, "video_poll", json!({"videoId": queued["id"]})).await;
    assert_eq!(pending["status"], "queued");
    assert!(pending["pollNote"].as_str().unwrap().contains("connection"));
    assert!(seen.0.lock().unwrap().is_empty(), "another connection must not receive the task ID");
}
