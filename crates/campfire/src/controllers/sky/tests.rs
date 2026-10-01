//! Hermes fork: Sky push-to-talk's routes (batch 1a), against the `default` parity seed
//! (request-level tests skip without it). David is an administrator.

use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::*;
use crate::controllers::presenters::test_support::*;
use crate::integrations::gemini_live::{MintError, TokenMinter};
use crate::integrations::net::BoxFuture;

const WORKSPACE: [(&str, &str); 2] = [("FIZZY_URL", "http://127.0.0.1:1"), ("FIZZY_TOKEN", "t0k3n")];
const GEMINI: (&str, &str) = ("GEMINI_API_KEY", "test-key");
/// `DAVID`, as `SKY_PTT_USERS` lists him.
const DAVID_ID: &str = "127326141";

fn env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![WORKSPACE[0], WORKSPACE[1], GEMINI];
    env.extend_from_slice(extra);
    env
}

#[derive(Default)]
struct FakeMinter {
    requests: Mutex<Vec<Value>>,
    fail: bool,
}

impl TokenMinter for FakeMinter {
    fn mint(&self, request: Value) -> BoxFuture<'_, std::result::Result<String, MintError>> {
        self.requests.lock().unwrap().push(request);
        let fail = self.fail;
        Box::pin(async move { if fail { Err(MintError::Status(403)) } else { Ok("auth_tokens/sky".to_string()) } })
    }
}

fn install(app: &TestApp, minter: FakeMinter) -> Arc<FakeMinter> {
    let minter = Arc::new(minter);
    app.booted.app.gemini_live.as_ref().unwrap().set_minter(minter.clone());
    minter
}

fn json_post(path: &str, body: &Value) -> Req {
    Req::new(Method::POST, path)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(serde_json::to_vec(body).unwrap())
}

async fn statuses(app: &TestApp) -> [StatusCode; 3] {
    let mut david = app.david();
    [
        david.write(json_post(TOKEN_PATH, &json!({}))).await.status,
        david.write(json_post(CONTEXT_PATH, &json!({"screen": "home"}))).await.status,
        david.write(json_post(USAGE_PATH, &json!({"token_id": "t1"}))).await.status,
    ]
}

/// David, a member instead of an administrator.
async fn demote_david(app: &TestApp) {
    app.db()
        .write(|tx| {
            tx.conn().execute(r#"UPDATE "users" SET "role" = 0 WHERE "id" = ?"#, [DAVID])?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn everything_is_404_and_nothing_rendered_unless_sky_is_on_for_this_person() {
    const NOT_FOUND: [StatusCode; 3] = [StatusCode::NOT_FOUND; 3];
    // SKY_PTT unset; set without the live voice; set without the workspace.
    for vars in [env(&[]), vec![WORKSPACE[0], WORKSPACE[1], ("SKY_PTT", "on")], vec![GEMINI, ("SKY_PTT", "on")]] {
        let Some(app) = TestApp::boot_with(&vars).await else { return };
        assert_eq!(statuses(&app).await, NOT_FOUND, "{vars:?}");
        let page = app.david().get(&format!("/rooms/{ALL_TALK}")).await;
        assert!(!page.text().contains("sky-ptt") && !page.text().contains("data-sky"), "{vars:?}");
    }

    // SKY_PTT=admins: not for a member.
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "admins")])).await else { return };
    install(&app, FakeMinter::default());
    demote_david(&app).await;
    assert_eq!(statuses(&app).await, NOT_FOUND);
    assert!(!app.david().get(&format!("/rooms/{ALL_TALK}")).await.text().contains("sky-ptt"));

    // SKY_PTT=users: a pilot member gets it.
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "users"), ("SKY_PTT_USERS", DAVID_ID)])).await else { return };
    install(&app, FakeMinter::default());
    demote_david(&app).await;
    assert_eq!(app.david().write(json_post(TOKEN_PATH, &json!({}))).await.status, StatusCode::OK);
    assert!(app.david().get(&format!("/rooms/{ALL_TALK}")).await.text().contains(r#"data-sky-debug="false""#), "no timings for members");
}

#[tokio::test]
async fn administrators_get_the_button_on_every_page_and_hidden_on_the_voice_page() {
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "admins")])).await else { return };
    let mut david = app.david();
    let room = david.get(&format!("/rooms/{ALL_TALK}")).await.text();
    assert_eq!(room.matches(r#"id="sky-ptt""#).count(), 1);
    assert!(room.contains(r#"<div id="sky-ptt" class="sky-ptt" data-turbo-permanent data-sky-token-url="/sky/token""#), "{room}");
    assert!(
        room.contains(&format!(r#"<template data-sky-page data-screen="room" data-room="{ALL_TALK}" data-card="" data-hidden="false">"#))
    );
    assert!(room.contains(r#"data-sky-debug="true""#), "administrators see the timings");
    assert!(room.find(r#"class="ws-tabbar""#).unwrap() < room.find(r#"id="sky-ptt""#).unwrap());
    let script = campfire_assets::asset_path("hermes/sky_ptt.js");
    assert!(room.contains(&format!(r#"<script type="module" src="{script}"></script>"#)));
    assert_eq!(david.get(&script).await.status, StatusCode::OK);
    assert!(room.contains(r#""lib/hermes/live_session""#), "pinned in the import map");

    let home = david.get("/workspace").await.text();
    assert!(home.contains(r#"data-screen="home""#));
    let voice = david.get(&format!("/rooms/{ALL_TALK}/voice")).await.text();
    assert!(voice.contains(r#"id="sky-ptt""#) && voice.contains(r#"data-hidden="true""#), "{voice}");
}

#[tokio::test]
async fn the_token_is_locked_short_and_limited() {
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "admins"), ("SKY_TOKENS_PER_HOUR", "1")])).await else { return };
    let minter = install(&app, FakeMinter::default());
    let mut david = app.david();

    let reply = david.write(json_post(TOKEN_PATH, &json!({})).header("accept-language", "es-MX,es;q=0.9")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!((body["token"].as_str(), body["ws_url"].as_str()), (Some("auth_tokens/sky"), Some(gemini_live::WS_URL)));
    assert_eq!(body["warm_seconds"], 120);
    let token_id = body["token_id"].as_str().unwrap().to_string();

    let request = minter.requests.lock().unwrap()[0].clone();
    assert!(request.get("fieldMask").is_none(), "the whole setup is locked");
    let expires: jiff::Timestamp = request["expireTime"].as_str().unwrap().parse().unwrap();
    let new_session: jiff::Timestamp = request["newSessionExpireTime"].as_str().unwrap().parse().unwrap();
    assert_eq!(expires.duration_since(new_session), jiff::SignedDuration::from_mins(9), "10-minute Sky tokens");
    let setup = &request["bidiGenerateContentSetup"];
    assert_eq!(setup["realtimeInputConfig"]["automaticActivityDetection"]["disabled"], true);
    assert!(setup.get("tools").is_none());
    let instruction = setup["systemInstruction"]["parts"][0]["text"].as_str().unwrap();
    assert!(instruction.contains(r#""David""#) && instruction.ends_with(r#""es-MX", "es""#), "{instruction}");
    assert!(!reply.text().contains("test-key"));

    // One new token an hour here; a reconnection of it counts a quarter.
    let limited = david.write(json_post(TOKEN_PATH, &json!({}))).await;
    assert_eq!((limited.status, limited.json()["error"].clone()), (StatusCode::TOO_MANY_REQUESTS, json!("rate_limited")));
    assert_eq!(david.write(json_post(TOKEN_PATH, &json!({ "reconnect_of": token_id }))).await.status, StatusCode::OK);

    // Forgery protection, no GET.
    let cross_site = david.send(json_post(TOKEN_PATH, &json!({})).header("sec-fetch-site", "cross-site")).await;
    assert_eq!(cross_site.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(david.get(TOKEN_PATH).await.status, StatusCode::NOT_FOUND);

    // Upstream failure: 502, nothing leaked.
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "admins")])).await else { return };
    install(&app, FakeMinter { fail: true, ..FakeMinter::default() });
    let failed = app.david().write(json_post(TOKEN_PATH, &json!({}))).await;
    assert_eq!((failed.status, failed.json()["error"].clone()), (StatusCode::BAD_GATEWAY, json!("upstream_error")));
}

#[tokio::test]
async fn the_month_budget_pauses_tokens() {
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "admins"), ("SKY_MONTHLY_BUDGET_USD", "1")])).await else { return };
    install(&app, FakeMinter::default());
    let mut david = app.david();
    let mut paused = None;
    // Each session reports at most one token's ceiling ($0.23): a few exhaust $1.
    for _ in 0..10 {
        let reply = david.write(json_post(TOKEN_PATH, &json!({}))).await;
        if reply.status != StatusCode::OK {
            paused = Some(reply);
            break;
        }
        let token_id = reply.json()["token_id"].as_str().unwrap().to_string();
        let report = json!({ "token_id": token_id, "held_ms": 600_000, "reply_ms": 600_000, "turns": 50 });
        assert_eq!(david.write(json_post(USAGE_PATH, &report)).await.status, StatusCode::OK);
        assert_eq!(david.write(json_post(USAGE_PATH, &report)).await.status, StatusCode::CONFLICT, "spent once");
    }
    let paused = paused.expect("the budget paused Sky");
    assert_eq!((paused.status, paused.json()["error"].clone()), (StatusCode::PAYMENT_REQUIRED, json!("budget_paused")));
    assert_eq!(david.write(json_post(USAGE_PATH, &json!({ "token_id": "t-forged", "held_ms": 1 }))).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_context_checks_the_pages_hint_and_counts_presses() {
    let Some(app) = TestApp::boot_with(&env(&[("SKY_PTT", "admins"), ("SKY_PRESSES_PER_DAY", "3")])).await else { return };
    let mut david = app.david();

    let room = david.write(json_post(CONTEXT_PATH, &json!({ "screen": "room", "room_id": ALL_TALK }))).await;
    assert_eq!(room.status, StatusCode::OK, "{}", room.text());
    let body = room.json();
    assert_eq!(body["chip"], "Room · All Talk");
    assert!(body["note"].as_str().unwrap().contains(r#"- room: "All Talk""#));
    assert!(body["press_id"].as_str().unwrap().starts_with('p'));
    assert_eq!(body["restricted"], false);

    // Not David's room, and a card the (empty) picture doesn't have: both dropped. The timings of
    // the last press are logged (numbers only).
    let hint = json!({ "screen": "room", "room_id": DIRECT_KEVIN_BENDER, "card": 57, "last": {
        "outcome": "answered", "warm": true, "release_to_first_audio_ms": 1200, "said": "never logged"
    } });
    let elsewhere = david.write(json_post(CONTEXT_PATH, &hint)).await;
    assert_eq!(elsewhere.status, StatusCode::OK);
    let body = elsewhere.json();
    assert_eq!(body["chip"], "Room");
    let note = body["note"].as_str().unwrap();
    assert!(!note.contains("- room:") && !note.contains("#57"), "{note}");

    assert_eq!(david.write(json_post(CONTEXT_PATH, &json!({ "screen": "home" }))).await.status, StatusCode::OK);
    let capped = david.write(json_post(CONTEXT_PATH, &json!({ "screen": "home" }))).await;
    assert_eq!((capped.status, capped.json()["error"].clone()), (StatusCode::TOO_MANY_REQUESTS, json!("daily_cap")));
}

#[test]
fn routes_come_after_the_rails_table() {
    use crate::controllers::recognize;
    let endpoint = |method: Method, path: &str| recognize(&method, path).unwrap().map(|(route, _)| route.endpoint);
    assert_eq!(endpoint(Method::POST, "/sky/token"), Some("hermes/sky#token"));
    assert_eq!(endpoint(Method::POST, "/sky/context"), Some("hermes/sky#context"));
    assert_eq!(endpoint(Method::POST, "/sky/usage"), Some("hermes/sky#usage"));
    assert_eq!(endpoint(Method::GET, "/sky/token"), None);
}

#[test]
fn numbers_from_json_or_strings() {
    assert_eq!(number(Some(&Param::Str(" 42 ".into()))), Some(42));
    assert_eq!(number(Some(&Param::Number(serde_json::Number::from(7)))), Some(7));
    assert_eq!(number(Some(&Param::Number(serde_json::Number::from_f64(1.9).unwrap()))), Some(1));
    assert_eq!(number(Some(&Param::Str("-3".into()))), None);
    assert_eq!(positive_id(Some(&Param::Str("0".into()))), None);
    assert_eq!(positive_id(None), None);
}
