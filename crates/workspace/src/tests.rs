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
/// Method, URL, headers and body.
type Request = (String, String, Vec<(&'static str, String)>, Option<Value>);

/// Answers GETs from a table of `path?query` → reply; anything else is a 404. Records requests.
/// Cards put in `cards` are live: GETs read them and writes change them the way Fizzy does
/// (taggings toggle, triage reopens, …), so tests can check the end state.
#[derive(Default)]
struct FakeFizzy {
    replies: Mutex<HashMap<String, Reply>>,
    requests: Mutex<Vec<Request>>,
    down: Mutex<bool>,
    cards: Mutex<HashMap<u64, Value>>,
    comments: Mutex<HashMap<u64, Vec<Value>>>,
    /// Writes whose path or body contains this answer that status (and change nothing).
    fail_writes: Mutex<Option<(String, u16)>>,
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
        self.requests.lock().unwrap().iter().map(|(_, url, ..)| url.trim_start_matches("http://fizzy").to_string()).collect()
    }

    /// `METHOD path body` of every write, in order.
    fn writes(&self) -> Vec<(String, String, Option<Value>)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, ..)| method != "GET")
            .map(|(method, url, _, body)| (method.clone(), url.trim_start_matches("http://fizzy").to_string(), body.clone()))
            .collect()
    }

    fn live(&self, card: Value) {
        self.cards.lock().unwrap().insert(card["number"].as_u64().unwrap(), card);
    }

    fn live_card(&self, number: u64) -> Value {
        self.cards.lock().unwrap()[&number].clone()
    }

    /// A write to a live card, Fizzy's way. `None`: not a route the fake knows.
    fn write(&self, method: &str, path: &str, body: &Value) -> Option<(u16, Value)> {
        static CARD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"^/897/cards/(\d+)/(taggings|triage|not_now|closure|comments|steps/(\w+))\.json$").unwrap()
        });
        if method == "POST" && path == "/897/boards/b1/cards.json" {
            let number = 100 + self.cards.lock().unwrap().len() as u64;
            let mut created = card(number, body["card"]["title"].as_str().unwrap_or(""), &[], None);
            created["description"] = json!(crate::html::to_text(body["card"]["description"].as_str().unwrap_or("")));
            self.live(created.clone());
            return Some((201, created));
        }
        let caps = CARD.captures(path)?;
        let number: u64 = caps[1].parse().unwrap();
        let mut cards = self.cards.lock().unwrap();
        let card = cards.get_mut(&number)?;
        match (method, &caps[2]) {
            ("POST", "taggings") => {
                let tag = body["tag_title"].as_str().unwrap().trim_start_matches('#').to_lowercase();
                let tags = card["tags"].as_array_mut().unwrap();
                match tags.iter().position(|t| t == &json!(tag)) {
                    Some(at) => {
                        tags.remove(at);
                    }
                    None => tags.push(json!(tag)),
                }
                tags.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
            ("POST", "triage") => {
                let id = body["column_id"].as_str().unwrap();
                card["column"] = json!({"id": id, "name": format!("Column {id}"), "color": {"name": "Lime"}});
                card["closed"] = json!(false);
                card["postponed"] = json!(false);
            }
            ("DELETE", "triage") => {
                card["column"] = Value::Null;
                card["closed"] = json!(false);
                card["postponed"] = json!(false);
            }
            ("POST", "not_now") => {
                card["column"] = Value::Null;
                card["postponed"] = json!(true);
            }
            ("POST", "closure") => {
                card["closed"] = json!(true);
                card["postponed"] = json!(false);
            }
            ("DELETE", "closure") => card["closed"] = json!(false),
            ("POST", "comments") => {
                let comment = json!({"id": "cm", "created_at": "2026-09-30T09:00:00Z", "creator": {"id": "fz-hermes", "name": "Hermes"},
                    "body": {"html": body["comment"]["body"], "plain_text": crate::html::to_text(body["comment"]["body"].as_str().unwrap())}});
                self.comments.lock().unwrap().entry(number).or_default().push(comment.clone());
                return Some((201, comment));
            }
            ("PUT", step) if step.starts_with("steps/") => {
                let id = &caps[3];
                let steps = card["steps"].as_array_mut()?;
                let step = steps.iter_mut().find(|step| step["id"] == json!(id))?;
                step["completed"] = body["step"]["completed"].clone();
                return Some((200, step.clone()));
            }
            _ => return None,
        }
        Some((204, Value::Null))
    }
}

impl HttpClient for FakeFizzy {
    fn send<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'static str, String)],
        body: Option<&'a [u8]>,
    ) -> BoxFuture<'a, Result<HttpResponse, String>> {
        Box::pin(async move {
            // Like a real request, let other tasks run meanwhile (concurrent writes interleave).
            tokio::task::yield_now().await;
            let body: Option<Value> = body.map(|body| serde_json::from_slice(body).unwrap());
            self.requests.lock().unwrap().push((method.to_string(), url.to_string(), headers.to_vec(), body.clone()));
            if *self.down.lock().unwrap() {
                return Err("connection refused".into());
            }
            let path = url.strip_prefix("http://fizzy").unwrap_or(url);
            let json = |status: u16, body: Value| Ok(HttpResponse { status, body: body.to_string().into_bytes(), link: None });
            if method != "GET" {
                let target = format!("{path} {}", body.as_ref().map(Value::to_string).unwrap_or_default());
                if let Some((fragment, status)) = self.fail_writes.lock().unwrap().clone()
                    && target.contains(&fragment)
                {
                    return json(status, json!({"status": status}));
                }
                return match self.write(method, path, body.as_ref().unwrap_or(&Value::Null)) {
                    Some((status, body)) => json(status, body),
                    None => json(404, json!({"status": 404, "error": "Not Found"})),
                };
            }
            if let Some(number) =
                path.strip_prefix("/897/cards/").and_then(|rest| rest.strip_suffix(".json")).and_then(|n| n.parse::<u64>().ok())
                && let Some(card) = self.cards.lock().unwrap().get(&number)
            {
                return json(200, card.clone());
            }
            if let Some(number) =
                path.strip_prefix("/897/cards/").and_then(|rest| rest.strip_suffix("/comments.json")).and_then(|n| n.parse::<u64>().ok())
                && self.cards.lock().unwrap().contains_key(&number)
            {
                return json(200, Value::Array(self.comments.lock().unwrap().get(&number).cloned().unwrap_or_default()));
            }
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

/// `card` on another board than the incident board.
fn elsewhere(mut card: Value) -> Value {
    card["board"] = json!({"id": "b2", "name": "Engineering"});
    card
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
                "id": "act3", "action": "comment_created", "created_at": "2026-09-30T08:20:00Z",
                "url": "http://localhost:8484/897/cards/40",
                "eventable_type": "Comment",
                "eventable": {"body": {"plain_text": "@Maya salary review draft attached", "html": format!(
                    r#"<p><action-text-attachment content-type="application/vnd.actiontext.mention" sgid="{}--x"></action-text-attachment> salary review draft attached</p>"#,
                    mention_sgid("fz-maya"))}, "card": {"id": "id40", "url": "http://localhost:8484/897/cards/40"}},
                "board": {"id": "b2", "name": "Engineering"},
                "creator": {"id": "fz-karim", "name": "Karim"}
            },
            {
                "id": "act1", "action": "card_triaged", "created_at": "2026-09-30T08:00:00Z",
                "eventable_type": "Card", "eventable": elsewhere(card(40, "Ice machine, 5th floor", &["sev-medium"], Some("Doing"))),
                "board": {"id": "b2", "name": "Engineering"}
            },
            {
                "id": "act0", "action": "card_triaged", "created_at": "2026-09-30T07:00:00Z",
                "eventable_type": "Card", "eventable": card(41, "Broken window, bar", &["sev-low"], Some("In progress")),
                "board": {"id": "b1", "name": "Incident Log"}
            }
        ]),
    );
    fizzy.reply("/897/users/fz-maya.json", json!({"id": "fz-maya", "name": "Maya", "email_address": "Maya@Hotel.test"}));
    fizzy.reply("/897/cards/77.json", card(77, "Pool pump knocking", &[], Some("In progress")));
    fizzy.reply("/897/cards/78.json", elsewhere(card(78, "Payroll export", &[], Some("Doing"))));
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
    assert_eq!(snapshot.card(41).unwrap().title, "Broken window, bar", "incident cards from the activity feed");
    assert!(snapshot.card(40).is_none() && !snapshot.cards.contains_key(&40), "not other boards' cards");
    assert_eq!(snapshot.origins, vec!["http://localhost:8484".to_string()]);
    assert_eq!(snapshot.mentions.len(), 1, "not other boards' mentions");
    assert_eq!(snapshot.mentions[0].card_number, 12);
    assert_eq!(snapshot.emails.get("fz-maya").map(String::as_str), Some("maya@hotel.test"));
    assert_eq!(snapshot.last_success, Some(now()));

    // Every request is JSON with the token, and nothing else carries it.
    for (_, url, headers, _) in fizzy.requests.lock().unwrap().iter() {
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
async fn cards_on_other_boards_never_become_chips() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(workspace.chips(&[40, 78]).is_empty());

    workspace.poll(&fizzy, now()).await.unwrap();
    let asked = |path: &str| fizzy.paths().iter().filter(|asked| *asked == path).count();
    assert_eq!((asked("/897/cards/40.json"), asked("/897/cards/78.json")), (1, 1));
    assert!(workspace.chips(&[40, 78]).is_empty());
    let link = r#"<a href="http://fizzy/897/cards/78">#78</a>"#;
    assert_eq!(
        workspace.decorate_message(1, 5, link).unwrap(),
        r#"<a data-ws-card="78" href="http://fizzy/897/cards/78">#78</a>"#,
        "a plain link"
    );

    workspace.poll(&fizzy, now() + SignedDuration::from_mins(1)).await.unwrap();
    assert_eq!(asked("/897/cards/78.json"), 1, "not asked again right away");
}

#[tokio::test]
async fn one_failed_lookup_does_not_fail_the_poll() {
    let fizzy = fizzy();
    fizzy.replies.lock().unwrap().insert("/897/cards/76.json".into(), (500, json!({"status": 500}), None));
    fizzy.replies.lock().unwrap().insert("/897/users/fz-maya.json".into(), (500, json!({"status": 500}), None));
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();
    workspace.chips(&[76, 77]);
    fizzy.reply("/897/cards.json?board_ids%5B%5D=b1&indexed_by=not_now", json!([card(14, "Wet floor sign missing", &[], None)]));

    workspace.poll(&fizzy, now() + SignedDuration::from_secs(30)).await.unwrap();
    let snapshot = workspace.snapshot();
    assert!(!snapshot.is_stale());
    assert_eq!(snapshot.open, vec![13, 12, 14], "the lists still refresh");
    assert!(snapshot.card(77).is_some(), "the other lookups still happen");
    assert_eq!(snapshot.lookup_errors, vec!["card 76: Fizzy answered 500".to_string(), "user fz-maya: Fizzy answered 500".to_string()]);
    assert!(snapshot.lookup_errors.iter().all(|error| !error.contains(TOKEN)));

    // Both are asked again next poll.
    fizzy.reply("/897/cards/76.json", card(76, "Sauna heater tripped", &[], None));
    fizzy.reply("/897/users/fz-maya.json", json!({"id": "fz-maya", "name": "Maya", "email_address": "maya@hotel.test"}));
    workspace.poll(&fizzy, now() + SignedDuration::from_secs(60)).await.unwrap();
    let snapshot = workspace.snapshot();
    assert!(workspace.chips(&[76])[&76].contains("Sauna heater tripped"));
    assert_eq!(snapshot.emails.get("fz-maya").map(String::as_str), Some("maya@hotel.test"));
    assert!(snapshot.lookup_errors.is_empty());
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
    let viewer = Viewer { id: 5, name: "Maya".into(), email: Some("maya@hotel.test".into()), administrator: false };
    let home = workspace.home(&viewer, &Messages(vec![draft]), now()).await.unwrap();
    assert_eq!(home.to_confirm.len(), 1);
    assert_eq!(home.open_count, 2);
    assert_eq!(home.mentions.len(), 1);
    assert_eq!(home.mentions[0].card_title, "Lift B out of service");
    let html = askama::Template::render(&home).unwrap();
    assert!(html.contains("Guest slip in lobby") && html.contains("Karim"));
}

// --- Phase 1: working cards ------------------------------------------------------------------------

use crate::actions::{Change, NewCard, Target};
use crate::fizzy::Severity;
use crate::pages::BoardFilter;
use crate::settings::{Department, Policy, Settings, SettingsStore};

type Records = std::sync::Arc<Mutex<Vec<WriteRecord>>>;

fn karim() -> Viewer {
    Viewer { id: 7, name: "Karim".into(), email: None, administrator: false }
}

fn scratch_store(settings: Settings) -> SettingsStore {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("campfire-workspace-tests-{}-{nanos}", std::process::id()));
    let store = SettingsStore::open(dir.join("workspace.json"));
    store.save(settings).unwrap();
    store
}

fn departments() -> Settings {
    Settings {
        departments: vec![
            Department { name: "Engineering".into(), tag: "engineering".into(), rooms: vec![3] },
            Department { name: "Security".into(), tag: "security".into(), rooms: vec![4] },
        ],
        ..Settings::default()
    }
}

/// A polled workspace with departments, whose writes are recorded; card 12 is live with a step.
async fn working(settings: Settings) -> (FakeFizzy, Workspace, Records) {
    let fizzy = fizzy();
    let mut live = card(12, "Lift B out of service", &["engineering", "incident", "sev-low"], None);
    live["steps"] = json!([{"id": "s1", "content": "Call the lift company", "completed": false}]);
    fizzy.live(live);
    fizzy.live(elsewhere(card(78, "Payroll export", &[], None)));
    let records: Records = Default::default();
    let sink = records.clone();
    let workspace =
        Workspace::new(config()).with_settings(scratch_store(settings)).with_audit(move |record| sink.lock().unwrap().push(record.clone()));
    workspace.poll(&fizzy, now()).await.unwrap();
    fizzy.requests.lock().unwrap().clear();
    (fizzy, workspace, records)
}

fn tags(fizzy: &FakeFizzy, number: u64) -> Vec<String> {
    fizzy.live_card(number)["tags"].as_array().unwrap().iter().map(|tag| tag.as_str().unwrap().to_string()).collect()
}

fn tag_writes(fizzy: &FakeFizzy) -> Vec<String> {
    fizzy
        .writes()
        .into_iter()
        .filter(|(_, path, _)| path.ends_with("/taggings.json"))
        .map(|(.., body)| body.unwrap()["tag_title"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn severity_is_a_single_choice_over_toggles() {
    let (fizzy, workspace, _) = working(departments()).await;
    let card = workspace.change_card(&fizzy, &karim(), 12, Change::Severity(Some(Severity::High))).await.unwrap();
    assert_eq!(tag_writes(&fizzy), ["sev-low", "sev-high"], "the old one off, the new one on");
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident", "sev-high"]);
    assert_eq!(card.severity(), Some(Severity::High));
    assert_eq!(workspace.snapshot().card(12).unwrap().severity(), Some(Severity::High), "cached at once");

    fizzy.requests.lock().unwrap().clear();
    workspace.change_card(&fizzy, &karim(), 12, Change::Severity(Some(Severity::High))).await.unwrap();
    assert!(tag_writes(&fizzy).is_empty(), "already high: nothing toggled (it would remove it)");

    workspace.change_card(&fizzy, &karim(), 12, Change::Severity(None)).await.unwrap();
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident"]);
}

#[tokio::test]
async fn several_severities_end_as_one() {
    let (fizzy, workspace, _) = working(departments()).await;
    fizzy.live(card(12, "Lift B", &["sev-critical", "sev-low"], None));
    workspace.change_card(&fizzy, &karim(), 12, Change::Severity(Some(Severity::Medium))).await.unwrap();
    assert_eq!(tags(&fizzy, 12), ["sev-medium"]);
}

#[tokio::test]
async fn concurrent_severity_changes_on_one_card_end_with_one_severity() {
    let (fizzy, workspace, _) = working(departments()).await;
    // Unserialized, both read sev-low, both toggle it (off, then back on), then each adds its own.
    let karim = karim();
    let (high, medium) = tokio::join!(
        workspace.change_card(&fizzy, &karim, 12, Change::Severity(Some(Severity::High))),
        workspace.change_card(&fizzy, &karim, 12, Change::Severity(Some(Severity::Medium))),
    );
    assert_eq!(high.unwrap().severity(), Some(Severity::High));
    assert_eq!(medium.unwrap().severity(), Some(Severity::Medium), "the second one ran after the first");
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident", "sev-medium"]);
    assert!(workspace.card_locks.lock().unwrap().is_empty(), "no lock left behind");
}

#[tokio::test]
async fn departments_are_the_card_tags_exactly() {
    let (fizzy, workspace, _) = working(departments()).await;
    let settings = workspace.settings();
    let change = Change::parse("departments", &json!({"tags": ["Security", "#security"]}), &settings).unwrap().unwrap();
    assert_eq!(change, Change::Departments(vec!["security".into()]));
    workspace.change_card(&fizzy, &karim(), 12, change).await.unwrap();
    assert_eq!(tag_writes(&fizzy), ["engineering", "security"]);
    assert_eq!(tags(&fizzy, 12), ["incident", "security", "sev-low"], "other tags untouched");

    let unknown = Change::parse("departments", &json!({"tags": ["kitchen"]}), &settings).unwrap().unwrap_err();
    assert_eq!((unknown.status(), unknown.code()), (422, "unknown_department"));
}

#[tokio::test]
async fn moving_closing_and_not_now() {
    let (fizzy, workspace, _) = working(departments()).await;
    let moved = workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Column("c1".into()))).await.unwrap();
    assert_eq!(moved.column.unwrap().id, "c1");
    assert_eq!(fizzy.writes(), [("POST".to_string(), "/897/cards/12/triage.json".to_string(), Some(json!({"column_id": "c1"})))]);

    fizzy.requests.lock().unwrap().clear();
    let error = workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Column("gone".into()))).await.unwrap_err();
    assert_eq!(error.code(), "unknown_column");
    assert!(fizzy.writes().is_empty(), "nothing written for a column that isn't on the board");

    workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Closed)).await.unwrap();
    let snapshot = workspace.snapshot();
    assert!(snapshot.card(12).unwrap().closed && snapshot.recently_closed[0] == 12 && !snapshot.open.contains(&12));

    workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::NotNow)).await.unwrap();
    let paths: Vec<String> = fizzy.writes().into_iter().map(|(method, path, _)| format!("{method} {path}")).collect();
    assert!(paths.ends_with(&["DELETE /897/cards/12/closure.json".into(), "POST /897/cards/12/not_now.json".into()]), "{paths:?}");
    assert!(workspace.snapshot().card(12).unwrap().postponed && workspace.snapshot().open.contains(&12));

    workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::New)).await.unwrap();
    let card = fizzy.live_card(12);
    assert!(card["column"].is_null() && card["postponed"] == json!(false));
    fizzy.requests.lock().unwrap().clear();
    workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::New)).await.unwrap();
    assert!(fizzy.writes().is_empty(), "already there");
}

#[tokio::test]
async fn steps_and_comments() {
    let (fizzy, workspace, _) = working(departments()).await;
    let card = workspace.change_card(&fizzy, &karim(), 12, Change::Step { id: "s1".into(), completed: true }).await.unwrap();
    assert!(card.steps[0].completed);
    assert_eq!(fizzy.writes()[0].1, "/897/cards/12/steps/s1.json");
    fizzy.requests.lock().unwrap().clear();
    workspace.change_card(&fizzy, &karim(), 12, Change::Step { id: "s1".into(), completed: true }).await.unwrap();
    assert!(fizzy.writes().is_empty());
    let unknown = workspace.change_card(&fizzy, &karim(), 12, Change::Step { id: "nope".into(), completed: true }).await.unwrap_err();
    assert_eq!(unknown.code(), "unknown_step");

    workspace.change_card(&fizzy, &karim(), 12, Change::Comment("On my way <b>now</b>\nwith the key".into())).await.unwrap();
    let (.., body) = fizzy.writes().pop().unwrap();
    assert_eq!(
        body.unwrap()["comment"]["body"],
        "<p>Karim: On my way &lt;b&gt;now&lt;/b&gt;<br>with the key</p>",
        "on Karim's behalf, escaped"
    );
}

#[tokio::test]
async fn only_incident_board_cards_are_touched() {
    let (fizzy, workspace, _) = working(departments()).await;
    for change in [Change::Move(Target::Closed), Change::Severity(Some(Severity::High)), Change::Comment("hi".into())] {
        let error = workspace.change_card(&fizzy, &karim(), 78, change).await.unwrap_err();
        assert_eq!(error.status(), 404);
    }
    assert_eq!(workspace.change_card(&fizzy, &karim(), 999, Change::Move(Target::Closed)).await.unwrap_err(), ActionError::NotFound);
    assert!(fizzy.writes().is_empty());
    assert_eq!(workspace.card_sheet(&fizzy, &karim(), 78).await.unwrap_err(), ActionError::NotFound);
}

#[tokio::test]
async fn the_policy_is_checked_before_anything() {
    let settings = Settings { duty_managers: Some(vec![1]), confirm_policy: Policy::DutyManagersOnly, ..departments() };
    let (fizzy, workspace, _) = working(settings).await;
    let error = workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Closed)).await.unwrap_err();
    assert_eq!((error.status(), error.message().as_str()), (403, "Only duty managers can do this."));
    let new = NewCard { title: "x".into(), description: String::new(), severity: None, departments: vec![], source: None };
    assert_eq!(workspace.create_card(&fizzy, &karim(), new).await.unwrap_err().status(), 403);
    assert!(fizzy.requests.lock().unwrap().is_empty(), "not even a read");

    let manager = Viewer { id: 1, ..karim() };
    workspace.change_card(&fizzy, &manager, 12, Change::Move(Target::Closed)).await.unwrap();

    let reporters = Settings { confirm_policy: Policy::AuthorOrDutyManager, ..departments() };
    let (fizzy, workspace, _) = working(reporters).await;
    assert_eq!(workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Closed)).await.unwrap_err().status(), 403);
    workspace.change_card(&fizzy, &karim(), 12, Change::Comment("seen".into())).await.unwrap();
    let admin = Viewer { administrator: true, ..karim() };
    workspace.change_card(&fizzy, &admin, 12, Change::Move(Target::Closed)).await.unwrap();
    assert!(workspace.authorize(&Act::ConfirmDraft { reporter_id: Some(7) }, &karim()).is_ok());
    assert!(workspace.authorize(&Act::ConfirmDraft { reporter_id: Some(8) }, &karim()).is_err());
}

#[tokio::test]
async fn every_write_is_audited_and_never_carries_the_token() {
    let (fizzy, workspace, records) = working(departments()).await;
    workspace.change_card(&fizzy, &karim(), 12, Change::Severity(Some(Severity::High))).await.unwrap();
    let records = records.lock().unwrap().clone();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0],
        WriteRecord { user_id: 7, card: Some(12), action: "tag -sev-low".into(), identity: "hermes", outcome: "ok".into() }
    );
    assert!(!format!("{records:?}").contains(TOKEN));
    for (method, _, headers, _) in fizzy.requests.lock().unwrap().iter().filter(|(method, ..)| method != "GET") {
        assert!(headers.contains(&("Authorization", format!("Bearer {TOKEN}"))), "{method}: Hermes's token");
        assert!(headers.contains(&("Content-Type", "application/json".into())) || method == "DELETE");
    }
}

#[tokio::test]
async fn a_refused_or_failed_write_says_so_and_shows_the_real_state() {
    let (fizzy, workspace, records) = working(departments()).await;
    // A read-only token: Fizzy answers 401 to every write.
    *fizzy.fail_writes.lock().unwrap() = Some(("/897/cards/12/".into(), 401));
    let error = workspace.change_card(&fizzy, &karim(), 12, Change::Severity(Some(Severity::High))).await.unwrap_err();
    assert!(error.message().contains("write permission"), "{error}");
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident", "sev-low"]);
    assert_eq!(records.lock().unwrap().len(), 1, "stopped at the first refusal, no blind retry");
    assert!(records.lock().unwrap()[0].outcome.contains("401"));

    *fizzy.fail_writes.lock().unwrap() = None;
    *fizzy.down.lock().unwrap() = true;
    let error = workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Closed)).await.unwrap_err();
    assert_eq!(error.code(), "fizzy_unavailable");
    assert!(!error.message().contains(TOKEN));
}

#[tokio::test]
async fn a_half_applied_tag_change_is_reported_and_cached_as_it_is() {
    let (fizzy, workspace, records) = working(departments()).await;
    // Removing sev-low works, adding sev-high fails.
    *fizzy.fail_writes.lock().unwrap() = Some(("sev-high".into(), 500));
    let error = workspace.change_card(&fizzy, &karim(), 12, Change::Severity(Some(Severity::High))).await.unwrap_err();
    assert_eq!((error.status(), error.code()), (502, "fizzy_unavailable"));
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident"]);
    assert_eq!(workspace.snapshot().card(12).unwrap().severity(), None, "the cache shows what Fizzy has");
    let outcomes: Vec<String> = records.lock().unwrap().iter().map(|record| format!("{} {}", record.action, record.outcome)).collect();
    assert_eq!(outcomes, ["tag -sev-low ok", "tag +sev-high Fizzy answered 500"]);
}

#[tokio::test]
async fn no_writes_before_the_board_is_known() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    let error = workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Closed)).await.unwrap_err();
    assert_eq!(error.code(), "fizzy_unavailable");
    assert!(fizzy.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn creating_a_card_from_a_message() {
    let (fizzy, workspace, records) = working(departments()).await;
    let settings = workspace.settings();
    let mut new = NewCard::parse(
        &json!({"title": "  Pool pump\n knocking ", "description": "Since this morning <loud>", "severity": "high", "department": "engineering"}),
        &settings,
    )
    .unwrap();
    assert_eq!(new.title, "Pool pump knocking");
    new.source = Some(crate::actions::CardSource {
        message_url: "https://chat.example/rooms/3/@55".into(),
        room_name: "engineering".into(),
        author_name: "Maya".into(),
    });
    let created = workspace.create_card(&fizzy, &karim(), new).await.unwrap();
    assert_eq!(created.warning, None);
    assert_eq!(created.url, format!("https://fizzy.example/897/cards/{}", created.card.number));
    let number = created.card.number;
    assert_eq!(tags(&fizzy, number), ["engineering", "sev-high"]);
    let writes = fizzy.writes();
    assert_eq!(writes[0].0, "POST");
    assert_eq!(writes[0].1, "/897/boards/b1/cards.json");
    assert_eq!(
        writes[0].2.as_ref().unwrap()["card"],
        json!({"title": "Pool pump knocking", "description": "<p>Since this morning &lt;loud&gt;</p>"})
    );
    let comment = writes.last().unwrap().2.as_ref().unwrap()["comment"]["body"].as_str().unwrap().to_string();
    assert_eq!(
        comment,
        r#"<p>Karim: Created from Maya’s message in engineering: <a href="https://chat.example/rooms/3/@55">https://chat.example/rooms/3/@55</a></p>"#
    );
    assert_eq!(records.lock().unwrap().len(), 4, "create, two tags, comment");

    let snapshot = workspace.snapshot();
    assert!(snapshot.open.contains(&number) && snapshot.card(number).is_some(), "cached before the next poll");
    let panel = workspace.room_panel(&karim(), 3).unwrap();
    assert_eq!(panel.cards[0].number, number, "most severe first in the room's panel");
    assert!(workspace.chip(number).unwrap().contains("Pool pump knocking"));
}

#[tokio::test]
async fn a_card_whose_tags_fail_is_still_created_with_a_warning() {
    let (fizzy, workspace, _) = working(departments()).await;
    *fizzy.fail_writes.lock().unwrap() = Some(("taggings".into(), 500));
    let new = NewCard {
        title: "Broken glass".into(),
        description: String::new(),
        severity: Some(Severity::Low),
        departments: vec![],
        source: None,
    };
    let created = workspace.create_card(&fizzy, &karim(), new).await.unwrap();
    let warning = created.warning.unwrap();
    assert!(warning.starts_with(&format!("Card #{} was created, but its tags couldn't be set", created.card.number)), "{warning}");
    assert!(workspace.snapshot().card(created.card.number).is_some());

    *fizzy.fail_writes.lock().unwrap() = Some(("/boards/b1/cards".into(), 500));
    let new = NewCard { title: "Other".into(), description: String::new(), severity: None, departments: vec![], source: None };
    assert_eq!(workspace.create_card(&fizzy, &karim(), new).await.unwrap_err().code(), "fizzy_unavailable");
}

#[test]
fn new_cards_are_validated() {
    let settings = departments();
    for (params, code) in [
        (json!({"title": "  "}), "blank_title"),
        (json!({"title": "x".repeat(256)}), "title_too_long"),
        (json!({"title": "x", "severity": "urgent"}), "invalid_severity"),
        (json!({"title": "x", "departments": ["kitchen"]}), "unknown_department"),
        (json!({"title": "x", "description": "d".repeat(10_001)}), "description_too_long"),
    ] {
        assert_eq!(NewCard::parse(&params, &settings).unwrap_err().code(), code, "{params}");
    }
    let card = NewCard::parse(&json!({"title": "x", "departments": ["Security", "engineering"]}), &settings).unwrap();
    assert_eq!(card.departments, ["security", "engineering"]);
}

#[test]
fn changes_are_parsed() {
    let settings = departments();
    let parse = |kind: &str, params: Value| Change::parse(kind, &params, &settings);
    assert_eq!(parse("move", json!({"to": "column:c1"})), Some(Ok(Change::Move(Target::Column("c1".into())))));
    assert_eq!(parse("move", json!({"to": "sideways"})).unwrap().unwrap_err().code(), "invalid_target");
    assert_eq!(parse("severity", json!({"severity": ""})), Some(Ok(Change::Severity(None))));
    assert_eq!(parse("step", json!({"step_id": "s1", "completed": "true"})), Some(Ok(Change::Step { id: "s1".into(), completed: true })));
    assert_eq!(parse("step", json!({"step_id": "s1"})).unwrap().unwrap_err().code(), "invalid_step");
    assert_eq!(parse("comment", json!({"body": " "})).unwrap().unwrap_err().code(), "blank_comment");
    assert_eq!(parse("comment", json!({"body": "x".repeat(5001)})).unwrap().unwrap_err().code(), "comment_too_long");
    assert_eq!(parse("delete", json!({})), None, "unknown changes are 404s");
}

#[tokio::test]
async fn the_card_sheet() {
    let (fizzy, workspace, _) = working(departments()).await;
    let mut live = fizzy.live_card(12);
    live["description"] = json!("Stuck on 6.\nNobody inside.");
    live["tags"] = json!(["engineering", "incident", "sev-low"]);
    fizzy.live(live);
    fizzy.comments.lock().unwrap().insert(
        12,
        vec![json!({"id": "c1", "created_at": "2026-09-30T08:00:00Z", "creator": {"id": "u", "name": "Maya"},
            "body": {"plain_text": "Called them <script>", "html": "<p>Called them &lt;script&gt;</p>"}})],
    );
    let sheet = workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap();
    assert_eq!(sheet.severity, Some("low"));
    assert_eq!(sheet.departments.iter().filter(|d| d.selected).map(|d| d.value.as_str()).collect::<Vec<_>>(), ["engineering"]);
    assert_eq!(sheet.other_tags, ["incident"]);
    assert_eq!(sheet.moves.iter().find(|m| m.selected).unwrap().value, "new");
    assert_eq!((sheet.steps.len(), sheet.steps_done), (1, 0));
    let html = askama::Template::render(&sheet).unwrap();
    for expected in [
        "Lift B out of service",
        "Call the lift company",
        "Called them &#60;script&#62;",
        "Stuck on 6.",
        "data-ws-change=\"severity\"",
        "https://fizzy.example/897/cards/12",
    ] {
        assert!(html.contains(expected), "{expected} {html}");
    }
    assert!(!html.contains("<script>"));

    let settings = Settings { confirm_policy: Policy::DutyManagersOnly, ..departments() };
    workspace.settings_store().save(settings).unwrap();
    let read_only = askama::Template::render(&workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap()).unwrap();
    assert!(read_only.contains(r#"data-ws-change="severity" disabled"#) && !read_only.contains("data-ws-comment"));
}

#[tokio::test]
async fn the_board_page() {
    let (fizzy, workspace, _) = working(departments()).await;
    fizzy.live(card(13, "Guest slip in lobby", &["security", "sev-critical"], None));
    workspace.change_card(&fizzy, &karim(), 13, Change::Comment("ok".into())).await.unwrap();

    let board = workspace.board(&karim(), &BoardFilter::default());
    let columns: Vec<(&str, Vec<u64>)> =
        board.columns.iter().map(|c| (c.key.as_str(), c.cards.iter().map(|card| card.number).collect())).collect();
    assert_eq!(columns[0], ("new", vec![13]), "open cards only (41 came from the activity feed)");
    assert_eq!(columns[1].0, "column:c1");
    assert!(columns[1].1.contains(&12));
    assert_eq!(columns.last().unwrap().0, "closed");
    assert!(board.columns[0].active && board.settings_url.is_none() && board.can_change);

    let settings = workspace.settings();
    let security = BoardFilter::parse(Some("security"), &[], &settings);
    let filtered = workspace.board(&karim(), &security);
    assert_eq!(filtered.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 1);
    let critical = BoardFilter::parse(Some("kitchen"), &["critical,high".into()], &settings);
    assert_eq!((critical.department.as_deref(), critical.severities.as_slice()), (None, [Severity::Critical, Severity::High].as_slice()));

    let admin = Viewer { administrator: true, ..karim() };
    let html = askama::Template::render(&workspace.board(&admin, &security)).unwrap();
    assert!(html.contains("Guest slip in lobby") && !html.contains("Lift B out of service"));
    assert!(html.contains(r#"data-ws-move="closed" data-ws-number="13""#) && html.contains(r#"href="/workspace/settings""#));
}

#[tokio::test]
async fn room_panels_follow_the_settings() {
    let (fizzy, workspace, _) = working(departments()).await;
    assert!(workspace.room_panel(&karim(), 9).is_none(), "a room not linked to a department has no panel");
    fizzy.live(card(13, "Door alarm", &["engineering", "sev-critical"], Some("In progress")));
    for number in [12, 13] {
        workspace.change_card(&fizzy, &karim(), number, Change::Comment("ok".into())).await.unwrap();
    }
    let panel = workspace.room_panel(&karim(), 3).unwrap();
    assert_eq!(panel.cards.iter().map(|card| card.number).collect::<Vec<_>>(), [13, 12]);
    assert_eq!(panel.departments, "Engineering");
    let html = askama::Template::render(&panel).unwrap();
    assert!(html.contains(r#"data-ws-panel-url="/workspace/rooms/3/panel""#) && html.contains(r#"data-ws-panel-count="2""#));
    assert!(html.contains(r#"href="/workspace/board?dept=engineering""#));
    assert!(workspace.room_panel(&karim(), 4).unwrap().cards.is_empty());
}

#[tokio::test]
async fn the_new_card_form_is_prefilled() {
    let (_, workspace, _) = working(departments()).await;
    let source = crate::pages::FormSource { message_id: 55, author_name: "Maya".into(), room_name: "engineering".into() };
    let text = text_of("<p>The pool plant room pump has been knocking since this morning</p><p>Loud &amp; clear.</p>");
    let form = workspace.new_card_form(Some(3), Some(&text), Some(source));
    assert_eq!(form.title, "The pool plant room pump has been knocking since this morning");
    assert!(form.description.ends_with("Loud & clear."));
    assert_eq!(form.departments.iter().find(|d| d.selected).unwrap().value, "engineering");
    let html = askama::Template::render(&form).unwrap();
    assert!(html.contains(r#"name="message_id" value="55""#) && html.contains("Loud &#38; clear."), "{html}");
}

#[tokio::test]
async fn the_settings_page_and_the_bot_view() {
    let (_, workspace, _) = working(departments()).await;
    let snapshot = workspace.snapshot();
    let rooms = [(3, "engineering".to_string()), (4, "security <desk>".to_string())];
    let page = crate::pages::settings_page(
        workspace.config(),
        &snapshot,
        &workspace.settings(),
        &rooms,
        &[(1, "Ann".into())],
        &["Ann".into()],
        None,
    );
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains(r#"value="engineering""#) && html.contains("security &#60;desk&#62;"));
    assert!(html.contains(r#"value="admins" checked"#) && html.contains(r#"value="anyone" checked"#));

    let broken = crate::pages::settings_page(
        workspace.config(),
        &snapshot,
        &Settings::fail_closed(),
        &rooms,
        &[(1, "Ann".into())],
        &["Ann".into()],
        Some("workspace.json isn't valid: EOF".into()),
    );
    let html = askama::Template::render(&broken).unwrap();
    assert!(
        html.contains("couldn't be read</strong> (workspace.json isn&#39;t valid: EOF)") && html.contains("only duty managers"),
        "{html}"
    );
    assert!(html.contains(r#"value="duty_managers_only" checked"#) && html.contains(r#"value="admins" checked"#));

    let json = crate::pages::bot_settings(workspace.config(), &snapshot, &workspace.settings(), &rooms, &[(1, "Ann".into())]);
    assert_eq!(json["incident_board"], "Incident Log");
    assert_eq!(json["departments"][0], json!({"name": "Engineering", "tag": "engineering", "rooms": [{"id": 3, "name": "engineering"}]}));
    assert_eq!(json["severity_tags"], json!(["sev-low", "sev-medium", "sev-high", "sev-critical"]));
    assert_eq!(json["duty_managers"], json!([{"id": 1, "name": "Ann"}]));
    assert_eq!(json["confirm_policy"], "anyone");
}
