//! Hermes fork: the live voice report routes, against the `default` parity seed (request-level
//! tests skip without it), plus the report's markup on its own.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use campfire_db::{Message, Room};
use serde_json::{Value, json};

use super::*;
use crate::controllers::presenters::test_support::*;
use crate::integrations::gemini_live::TokenMinter;
use crate::integrations::net::BoxFuture;
use crate::integrations::test_support::{FakeServer, Route};

const LIVE: &[(&str, &str)] = &[("GEMINI_API_KEY", "test-key")];

/// Records the requests it's asked to mint, and answers with a fixed token (or fails).
#[derive(Default)]
struct FakeMinter {
    requests: Mutex<Vec<Value>>,
    fail: bool,
}

impl TokenMinter for FakeMinter {
    fn mint(&self, request: Value) -> BoxFuture<'_, std::result::Result<String, MintError>> {
        self.requests.lock().unwrap().push(request);
        let fail = self.fail;
        Box::pin(async move { if fail { Err(MintError::Status(403)) } else { Ok("auth_tokens/fake".to_string()) } })
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

async fn last_message(app: &TestApp, room_id: i64) -> Message {
    let mut messages = app.db().read(move |conn| Message::for_room(conn, room_id)).await.unwrap();
    messages.sort_by_key(|m| (m.created_at, m.id));
    messages.pop().unwrap()
}

#[tokio::test]
async fn everything_is_404_while_the_feature_is_off() {
    let Some(app) = TestApp::boot().await else { return };
    let mut david = app.david();
    assert_eq!(david.get(&voice_path(ALL_TALK)).await.status, StatusCode::NOT_FOUND);
    let token = david.write(json_post(&voice_token_path(ALL_TALK), &json!({}))).await;
    assert_eq!(token.status, StatusCode::NOT_FOUND);
    let report = david.write(json_post(&voice_report_path(ALL_TALK), &json!({ "title": "t", "summary": "s" }))).await;
    assert_eq!(report.status, StatusCode::NOT_FOUND);

    let room = david.get(&format!("/rooms/{ALL_TALK}")).await;
    assert_eq!(room.status, StatusCode::OK);
    assert!(!room.text().contains("/voice\""), "no mic button");
}

#[tokio::test]
async fn the_page_wires_the_voice_controller() {
    let Some(app) = TestApp::boot_with(LIVE).await else { return };
    let mut david = app.david();

    let room = david.get(&format!("/rooms/{ALL_TALK}")).await;
    assert!(room.text().contains(&format!(r#"href="/rooms/{ALL_TALK}/voice""#)), "the mic button");

    let page = david.get(&voice_path(ALL_TALK)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    let html = page.text();
    assert!(html.contains(r#"data-controller="voice""#));
    assert!(html.contains(&format!(r#"data-voice-token-url-value="/rooms/{ALL_TALK}/voice/token""#)));
    assert!(html.contains(&format!(r#"data-voice-report-url-value="/rooms/{ALL_TALK}/voice/report""#)));
    assert!(html.contains(&format!(r#"data-voice-room-url-value="/rooms/{ALL_TALK}""#)));
    assert!(html.contains(r#"data-voice-room-name-value="All Talk""#), "{html}");
    let worklet = campfire_assets::asset_path(WORKLET_ASSET);
    assert!(worklet.starts_with("/assets/voice/pcm-worklet-"), "{worklet}");
    assert!(html.contains(&format!(r#"data-voice-worklet-url-value="{worklet}""#)));
    for target in ["toggle", "status", "transcript", "result"] {
        assert!(html.contains(&format!(r#"data-voice-target="{target}""#)), "{target}");
    }
    assert!(html.contains(r#""controllers/voice_controller""#), "pinned in the import map");

    // Served, as JavaScript, for audioWorklet.addModule.
    let served = david.get(&worklet).await;
    assert_eq!(served.status, StatusCode::OK);
    assert!(served.content_type().unwrap().starts_with("text/javascript"));

    // RoomScoped: David isn't in Kevin and Bender's room.
    assert_eq!(david.get(&voice_path(DIRECT_KEVIN_BENDER)).await.status, StatusCode::NOT_FOUND);
    // Signed out: off to sign in.
    assert_eq!(app.anonymous().get(&voice_path(ALL_TALK)).await.status, StatusCode::FOUND);
}

#[tokio::test]
async fn token_mints_with_the_locked_setup() {
    let Some(app) = TestApp::boot_with(LIVE).await else { return };
    let minter = install(&app, FakeMinter::default());
    let mut david = app.david();

    let reply = david.write(json_post(&voice_token_path(ALL_TALK), &json!({}))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["token"], "auth_tokens/fake");
    assert_eq!(body["ws_url"], gemini_live::WS_URL);
    assert_eq!(body["model"], "models/gemini-3.8-live");
    let expires_at: jiff::Timestamp = body["expires_at"].as_str().unwrap().parse().unwrap();
    assert!(expires_at > jiff::Timestamp::now());

    let requests = minter.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let setup = &requests[0]["bidiGenerateContentSetup"];
    assert_eq!(requests[0]["uses"], 1);
    assert_eq!(setup["model"], "models/gemini-3.8-live");
    assert_eq!(setup["tools"][0]["functionDeclarations"][0]["name"], "submit_incident");
    let instruction = setup["systemInstruction"]["parts"][0]["text"].as_str().unwrap();
    assert!(instruction.contains(r#""David""#) && instruction.contains(r#""All Talk""#), "{instruction}");

    // Forgery protection, membership, no GET.
    let cross_site = david.send(json_post(&voice_token_path(ALL_TALK), &json!({})).header("sec-fetch-site", "cross-site")).await;
    assert_eq!(cross_site.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(david.write(json_post(&voice_token_path(DIRECT_KEVIN_BENDER), &json!({}))).await.status, StatusCode::NOT_FOUND);
    assert_eq!(david.get(&voice_token_path(ALL_TALK)).await.status, StatusCode::NOT_FOUND);
    assert_eq!(minter.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn token_requests_are_rate_limited_and_upstream_failures_are_502() {
    let Some(app) = TestApp::boot_with(&[("GEMINI_API_KEY", "test-key"), ("GEMINI_LIVE_TOKENS_PER_HOUR", "2")]).await else { return };
    install(&app, FakeMinter::default());
    let mut david = app.david();
    for _ in 0..2 {
        assert_eq!(david.write(json_post(&voice_token_path(ALL_TALK), &json!({}))).await.status, StatusCode::OK);
    }
    let limited = david.write(json_post(&voice_token_path(ALL_TALK), &json!({}))).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.json()["error"], "rate_limited");

    let Some(app) = TestApp::boot_with(LIVE).await else { return };
    install(&app, FakeMinter { fail: true, ..FakeMinter::default() });
    let failed = app.david().write(json_post(&voice_token_path(ALL_TALK), &json!({}))).await;
    assert_eq!(failed.status, StatusCode::BAD_GATEWAY);
    assert_eq!(failed.json()["error"], "upstream_error");
    assert!(!failed.text().contains("test-key"));
}

#[tokio::test]
async fn reports_need_membership_and_a_title_and_summary() {
    let Some(app) = TestApp::boot_with(LIVE).await else { return };
    let mut david = app.david();
    let report = json!({ "title": "Chute", "summary": "Une chute." });
    assert_eq!(david.write(json_post(&voice_report_path(DIRECT_KEVIN_BENDER), &report)).await.status, StatusCode::NOT_FOUND);
    let incomplete = david.write(json_post(&voice_report_path(ALL_TALK), &json!({ "title": "Chute", "summary": "  " }))).await;
    assert_eq!(incomplete.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(incomplete.json()["error"], "invalid_report");
    let cross_site = david.send(json_post(&voice_report_path(ALL_TALK), &report).header("sec-fetch-site", "cross-site")).await;
    assert_eq!(cross_site.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Quiet Corner has no bot to address the report to.
    let no_bot = david.write(json_post(&voice_report_path(QUIET_CORNER), &report)).await;
    assert_eq!(no_bot.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", no_bot.text());
    assert_eq!(no_bot.json()["error"], "bot_not_in_room");
}

/// The open point of the plan: the mention the server writes is recognized like a human's, so in a
/// shared (non-direct) room, where only mentioned bots are called, the bot's webhook fires.
#[tokio::test(flavor = "multi_thread")]
async fn a_report_mentions_the_bot_whose_webhook_then_fires_in_a_shared_room() {
    let bot = BENDER.to_string();
    let Some(app) = TestApp::boot_with(&[("GEMINI_API_KEY", "test-key"), ("GEMINI_LIVE_VOICE_BOT", bot.as_str())]).await else { return };
    let hook = FakeServer::start(vec![Route::new("POST", "*", "/hook", 200)]).await;
    let url = format!("http://{}/hook", hook.addr);
    app.db()
        .write(move |tx| {
            let now = tx.now();
            let updated = tx.conn().execute(r#"UPDATE "webhooks" SET "url" = ? WHERE "user_id" = ?"#, rusqlite::params![url, BENDER])?;
            if updated == 0 {
                tx.conn().execute(
                    r#"INSERT INTO "webhooks" ("created_at", "updated_at", "url", "user_id") VALUES (?, ?, ?, ?)"#,
                    rusqlite::params![now, now, url, BENDER],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let room = app.db().read(|conn| Room::find(conn, ALL_TALK)).await.unwrap();
    assert!(!room.direct(), "a shared room: bots are only called when mentioned");

    let mut david = app.david();
    let report = json!({
        "title": "Chute <script>alert(1)</script>",
        "summary": "Un visiteur a glissé.",
        "location": "Hall d'entrée",
        "severity": "medium",
        "transcript": "Employé : il a glissé\nAssistant : merci",
    });
    let reply = david.write(json_post(&voice_report_path(ALL_TALK), &report)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let body = reply.json();
    let message = last_message(&app, ALL_TALK).await;
    assert_eq!(body["message_id"], message.id);
    assert_eq!(body["message_url"], format!("http://campfire.test/rooms/{ALL_TALK}/@{}", message.id));
    assert_eq!(message.creator_id, DAVID, "posted as the current user");

    let stored = app.db().read(move |conn| message.body_html(conn)).await.unwrap().unwrap();
    assert!(!stored.contains("<script>"), "{stored}");
    assert!(stored.contains("Chute &lt;script&gt;"), "{stored}");
    assert!(stored.contains("application/vnd.campfire.mention"), "{stored}");

    let mut delivered = Vec::new();
    for _ in 0..100 {
        delivered = hook.received();
        if !delivered.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(delivered.len(), 1, "Bender's webhook was called");
    let payload: Value = serde_json::from_slice(&delivered[0].body).unwrap();
    assert_eq!(payload["message"]["id"], body["message_id"]);
    assert_eq!(payload["user"]["id"], DAVID);
    assert_eq!(payload["room"]["id"], ALL_TALK);
    let plain = payload["message"]["body"]["plain"].as_str().unwrap();
    assert!(plain.contains("Un visiteur a glissé."), "{plain}");
    assert!(plain.contains("il a glissé"), "the transcript too: {plain}");
}

// --- The report on its own ------------------------------------------------------------------------

fn params(value: Value) -> ParamMap {
    campfire_kit::params::from_json_body(&serde_json::to_vec(&value).unwrap()).unwrap()
}

#[test]
fn reads_the_report_and_caps_its_sizes() {
    assert_eq!(IncidentReport::from_params(&params(json!({ "title": "t" }))), None);
    assert_eq!(IncidentReport::from_params(&params(json!({ "title": 3, "summary": "s" }))), None);

    let long = "é".repeat(MAX_TRANSCRIPT_BYTES);
    let report = IncidentReport::from_params(&params(json!({
        "title": " Chute ",
        "summary": "s",
        "location": "",
        "severity": "HIGH",
        "people_involved": "x".repeat(MAX_FIELD_BYTES + 10),
        "transcript": long,
    })))
    .unwrap();
    assert_eq!(report.title, "Chute");
    assert_eq!(report.location, None);
    assert_eq!(report.severity, Some(Severity::High));
    let people = report.people_involved.as_deref().unwrap();
    assert!(people.len() <= MAX_FIELD_BYTES && people.ends_with('…'));
    let transcript = report.transcript.as_deref().unwrap();
    assert!(transcript.len() <= MAX_TRANSCRIPT_BYTES && transcript.ends_with('…'));

    let unknown = IncidentReport::from_params(&params(json!({ "title": "t", "summary": "s", "severity": "apocalyptic" }))).unwrap();
    assert_eq!(unknown.severity, None);
}

#[test]
fn the_report_is_escaped_markup_with_the_mention_first() {
    let report = IncidentReport {
        title: "<b>Titre</b>".into(),
        summary: "Ligne 1\nLigne \"2\"".into(),
        injuries: Some("aucune".into()),
        severity: Some(Severity::Critical),
        transcript: Some("a <img src=x onerror=alert(1)>…".into()),
        ..IncidentReport::default()
    };
    let html = report.to_html("sgid\"--x");
    assert!(html.starts_with(
        r#"<p><action-text-attachment sgid="sgid&quot;--x" content-type="application/vnd.campfire.mention"></action-text-attachment> "#
    ));
    assert!(html.contains("<h3>&lt;b&gt;Titre&lt;/b&gt;</h3>"));
    assert!(html.contains("Ligne 1<br>Ligne &quot;2&quot;"));
    assert!(html.contains("<li><strong>Blessures :</strong> aucune</li>"));
    assert!(html.contains("<li><strong>Lieu :</strong> <em>non précisé</em></li>"));
    assert!(html.contains("critique (critical)"));
    assert!(html.contains("Transcription de l’entretien (tronquée) :"));
    assert!(html.contains("<blockquote>a &lt;img src=x onerror=alert(1)&gt;…</blockquote>"));
    assert!(!html.contains("<img"));
}

#[test]
fn routes_come_after_the_rails_table() {
    use crate::controllers::recognize;
    let found = |method: Method, path: &str| recognize(&method, path).unwrap().map(|(route, params)| (route.endpoint, params.str("room_id").map(str::to_string)));
    assert_eq!(found(Method::GET, "/rooms/7/voice"), Some(("hermes/voice#show", Some("7".into()))));
    assert_eq!(found(Method::POST, "/rooms/7/voice/token"), Some(("hermes/voice#token", Some("7".into()))));
    assert_eq!(found(Method::POST, "/rooms/7/voice/report"), Some(("hermes/voice#report", Some("7".into()))));
    assert_eq!(found(Method::GET, "/rooms/7/voice/token"), None);
    // The Rails routes still win where they match.
    assert_eq!(found(Method::GET, "/rooms/7").map(|(endpoint, _)| endpoint), Some("rooms#show"));
}
