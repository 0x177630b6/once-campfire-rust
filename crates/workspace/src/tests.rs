//! The poll against a fake Fizzy, and the hooks on top of what it saw.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;
use serde_json::{Value, json};

use super::*;
use crate::fizzy::{HttpClient, HttpResponse};

const TOKEN: &str = "s3cret-token";

/// Status, JSON body and `Link` header.
type Reply = (u16, Value, Option<String>);
/// URL and headers.
type Request = (String, Vec<(&'static str, String)>);

/// Answers GETs from a table of `path?query` → reply; anything else is a 404. Records requests.
#[derive(Default)]
struct FakeFizzy {
    replies: Mutex<HashMap<String, Reply>>,
    requests: Mutex<Vec<Request>>,
    down: Mutex<bool>,
}

impl FakeFizzy {
    fn reply(&self, path: &str, body: Value) {
        self.replies.lock().unwrap().insert(path.to_string(), (200, body, None));
    }

    fn reply_paged(&self, path: &str, body: Value) {
        self.replies
            .lock()
            .unwrap()
            .insert(path.to_string(), (200, body, Some(format!("<http://localhost:8484{path}&page=2>; rel=\"next\""))));
    }

    fn paths(&self) -> Vec<String> {
        self.requests.lock().unwrap().iter().map(|(url, _)| url.trim_start_matches("http://fizzy").to_string()).collect()
    }
}

impl HttpClient for FakeFizzy {
    fn get<'a>(&'a self, url: &'a str, headers: &'a [(&'static str, String)]) -> BoxFuture<'a, Result<HttpResponse, String>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push((url.to_string(), headers.to_vec()));
            if *self.down.lock().unwrap() {
                return Err("connection refused".into());
            }
            let path = url.strip_prefix("http://fizzy").unwrap_or(url);
            match self.replies.lock().unwrap().get(path) {
                Some((status, body, link)) => Ok(HttpResponse { status: *status, body: body.to_string().into_bytes(), link: link.clone() }),
                None => Ok(HttpResponse { status: 404, body: br#"{"status":404,"error":"Not Found"}"#.to_vec(), link: None }),
            }
        })
    }
}

fn config() -> WorkspaceConfig {
    WorkspaceConfig::from_lookup(|name| match name {
        "FIZZY_URL" => Some("http://fizzy".into()),
        "FIZZY_TOKEN" => Some(TOKEN.into()),
        "FIZZY_PUBLIC_URL" => Some("https://fizzy.example".into()),
        _ => None,
    })
    .unwrap()
    .unwrap()
}

fn now() -> Timestamp {
    "2026-09-30T09:00:00Z".parse().unwrap()
}

fn card(number: u64, title: &str, tags: &[&str], column: Option<&str>) -> Value {
    json!({
        "id": format!("id{number}"), "number": number, "title": title, "status": "published",
        "tags": tags, "closed": false, "postponed": false,
        "created_at": "2026-09-30T06:00:00Z", "last_active_at": "2026-09-30T08:00:00Z",
        "url": format!("http://localhost:8484/897/cards/{number}"),
        "board": {"id": "b1", "name": "Incident Log"},
        "column": column.map(|name| json!({"id": "c1", "name": name, "color": {"name": "Lime", "value": "var(--color-card-4)"}})),
        "assignees": []
    })
}

fn mention_sgid(user_id: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!(r#"{{"_rails":{{"data":"gid://fizzy/User/{user_id}"}}}}"#))
}

fn fizzy() -> FakeFizzy {
    let fizzy = FakeFizzy::default();
    fizzy.reply("/my/identity.json", json!({"accounts": [{"id": "a", "slug": "/897", "user": {"id": "fz-hermes"}}]}));
    fizzy.reply("/897/boards.json", json!([{"id": "b0", "name": "Engineering"}, {"id": "b1", "name": "Incident Log"}]));
    fizzy.reply("/897/boards/b1/columns.json", json!([{"id": "c1", "name": "In progress", "color": {"name": "Lime"}}]));
    fizzy.reply_paged("/897/cards.json?board_ids%5B%5D=b1", json!([card(13, "Guest slip in lobby", &["incident", "sev-critical"], None)]));
    fizzy
        .reply("/897/cards.json?board_ids%5B%5D=b1&page=2", json!([card(12, "Lift B out of service", &["sev-high"], Some("In progress"))]));
    fizzy.reply("/897/cards.json?board_ids%5B%5D=b1&indexed_by=not_now", json!([]));
    fizzy.reply("/897/cards.json?board_ids%5B%5D=b1&indexed_by=closed", json!([]));
    fizzy.reply(
        "/897/activities.json",
        json!([
            {
                "id": "act2", "action": "comment_created", "created_at": "2026-09-30T08:30:00Z",
                "url": "http://localhost:8484/897/cards/12",
                "eventable_type": "Comment",
                "eventable": {"body": {"plain_text": "@Maya can you call the lift company?", "html": format!(
                    r#"<p><action-text-attachment content-type="application/vnd.actiontext.mention" sgid="{}--x"></action-text-attachment> can you call the lift company?</p>"#,
                    mention_sgid("fz-maya"))}, "card": {"id": "id12", "url": "http://localhost:8484/897/cards/12"}},
                "board": {"id": "b1", "name": "Incident Log"},
                "creator": {"id": "fz-karim", "name": "Karim"}
            },
            {
                "id": "act1", "action": "card_triaged", "created_at": "2026-09-30T08:00:00Z",
                "eventable_type": "Card", "eventable": card(40, "Ice machine, 5th floor", &["sev-medium"], Some("Doing")),
                "board": {"id": "b2", "name": "Engineering"}
            }
        ]),
    );
    fizzy.reply("/897/users/fz-maya.json", json!({"id": "fz-maya", "name": "Maya", "email_address": "Maya@Hotel.test"}));
    fizzy.reply("/897/cards/77.json", card(77, "Pool pump knocking", &[], Some("In progress")));
    fizzy
}

#[tokio::test]
async fn a_poll_builds_the_picture() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();

    let snapshot = workspace.snapshot();
    assert_eq!(snapshot.account.as_deref(), Some("897"));
    assert_eq!(snapshot.board.as_ref().unwrap().id, "b1");
    assert_eq!(snapshot.open, vec![13, 12], "both pages");
    assert_eq!(snapshot.card(40).unwrap().title, "Ice machine, 5th floor", "cards from the activity feed");
    assert_eq!(snapshot.origins, vec!["http://localhost:8484".to_string()]);
    assert_eq!(snapshot.mentions.len(), 1);
    assert_eq!(snapshot.emails.get("fz-maya").map(String::as_str), Some("maya@hotel.test"));
    assert_eq!(snapshot.last_success, Some(now()));

    // Every request is JSON with the token, and nothing else carries it.
    for (url, headers) in fizzy.requests.lock().unwrap().iter() {
        assert!(!url.contains(TOKEN));
        assert!(headers.contains(&("Accept", "application/json".into())));
        assert!(headers.contains(&("Authorization", format!("Bearer {TOKEN}"))));
    }
    let paths = fizzy.paths();
    assert!(paths.contains(&"/897/activities.json?page=3".to_string()) || paths.contains(&"/897/activities.json".to_string()));
}

#[tokio::test]
async fn chips_ask_for_unknown_cards_and_the_next_poll_fetches_them() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();

    let chips = workspace.chips(&[12, 77]);
    assert!(chips[&12].contains("Lift B out of service") && chips[&12].contains("https://fizzy.example/897/cards/12"));
    assert!(!chips.contains_key(&77));

    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(fizzy.paths().contains(&"/897/cards/77.json".to_string()));
    assert!(workspace.chips(&[77])[&77].contains("Pool pump knocking"));
}

#[tokio::test]
async fn fizzy_down_keeps_the_last_picture() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();
    *fizzy.down.lock().unwrap() = true;
    let error = workspace.poll(&fizzy, now() + SignedDuration::from_secs(30)).await.unwrap_err();
    assert!(!error.to_string().contains(TOKEN));

    let snapshot = workspace.snapshot();
    assert!(snapshot.is_stale());
    assert_eq!(snapshot.last_success, Some(now()));
    assert_eq!(snapshot.open, vec![13, 12]);
    assert!(workspace.chips(&[13]).contains_key(&13));
}

#[tokio::test]
async fn a_missing_board_is_an_error_and_is_looked_up_again() {
    let fizzy = fizzy();
    fizzy.reply("/897/boards.json", json!([{"id": "b0", "name": "Engineering"}]));
    let workspace = Workspace::new(config());
    let error = workspace.poll(&fizzy, now()).await.unwrap_err();
    assert!(error.to_string().contains("Incident Log"), "{error}");
    assert!(workspace.snapshot().board.is_none());

    fizzy.reply("/897/boards.json", json!([{"id": "b1", "name": "incident log"}]));
    workspace.poll(&fizzy, now()).await.unwrap();
    assert_eq!(workspace.snapshot().board.as_ref().unwrap().id, "b1");
}

#[tokio::test]
async fn messages_get_chips_and_bot_drafts_get_buttons() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();
    workspace.set_bots(vec![Bot { id: 9, name: "Hermes".into(), sgid: "sg".into() }]);

    let filed = r#"<div class="lexxy-content">Card filed: <a target="_blank" href="http://localhost:8484/897/cards/13">http://localhost:8484/897/cards/13</a></div>"#;
    let out = workspace.decorate_message(1, 9, filed).unwrap();
    assert!(out.contains(r#"class="ws-chip ws-cc--new""#) && !out.contains("ws-draft"));

    let draft = "<p><b>Incident — AC leak — room 103</b></p><p>Reply <b>confirm</b> to file it in Fizzy, or tell me what to change.</p>";
    let out = workspace.decorate_message(2, 9, draft).unwrap();
    assert!(out.starts_with(draft) && out.contains(r#"data-ws-draft="2""#));
    assert_eq!(workspace.decorate_message(3, 5, draft), None, "not from a bot");
    assert_eq!(workspace.decorate_message(4, 9, "<p>Hello</p>"), None);

    let unknown = r#"<a href="http://fizzy/897/cards/77">#77</a>"#;
    assert!(workspace.decorate_message(5, 5, unknown).unwrap().contains(r#"data-ws-card="77""#));
    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(fizzy.paths().contains(&"/897/cards/77.json".to_string()), "the render asked for it");
}

struct Messages(Vec<ChatMessage>);

impl ChatSource for Messages {
    fn recent_messages(&self, since: Timestamp) -> BoxFuture<'_, Result<Vec<ChatMessage>, String>> {
        let messages = self.0.iter().filter(|message| message.created_at >= since).cloned().collect();
        Box::pin(async move { Ok(messages) })
    }
}

#[tokio::test]
async fn the_home_page() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();
    workspace.set_bots(vec![Bot { id: 9, name: "Hermes".into(), sgid: "sg".into() }]);
    let draft = ChatMessage {
        id: 2,
        room_id: 1,
        room_name: "front-desk".into(),
        url: "/rooms/1/@2".into(),
        creator_id: 9,
        creator_name: "Hermes".into(),
        creator_is_bot: true,
        created_at: now() - SignedDuration::from_mins(10),
        body_html: "<p><b>Incident — AC leak — room 103</b></p><p>Reply <b>confirm</b> to file it in Fizzy.</p>".into(),
    };
    let viewer = Viewer { id: 5, name: "Maya".into(), email: Some("maya@hotel.test".into()) };
    let home = workspace.home(&viewer, &Messages(vec![draft]), now()).await.unwrap();
    assert_eq!(home.to_confirm.len(), 1);
    assert_eq!(home.open_count, 2);
    assert_eq!(home.mentions.len(), 1);
    assert_eq!(home.mentions[0].card_title, "Lift B out of service");
    let html = askama::Template::render(&home).unwrap();
    assert!(html.contains("Guest slip in lobby") && html.contains("Karim"));
}
