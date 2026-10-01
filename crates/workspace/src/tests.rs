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
    /// Comment lists without `X-Total-Count`.
    no_total_count: Mutex<bool>,
    /// Writes whose path or body contains this answer that status (and change nothing).
    fail_writes: Mutex<Option<(String, u16)>>,
    /// Token → the Fizzy user `GET /my/identity` names (else the canned reply).
    identities: Mutex<HashMap<String, String>>,
    /// Comments written: `(card, id, token)`.
    comment_tokens: Mutex<Vec<(u64, String, String)>>,
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
    fn write(&self, method: &str, path: &str, body: &Value, token: &str) -> Option<(u16, Value)> {
        static CARD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"^/897/cards/(\d+)/(taggings|triage|not_now|closure|comments|steps|steps/(\w+)|comments/(\w+))\.json$")
                .unwrap()
        });
        static TITLE: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"^/897/cards/(\d+)\.json$").unwrap());
        if method == "PUT"
            && let Some(caps) = TITLE.captures(path)
        {
            let mut cards = self.cards.lock().unwrap();
            let card = cards.get_mut(&caps[1].parse::<u64>().unwrap())?;
            card["title"] = body["card"]["title"].clone();
            return Some((200, card.clone()));
        }
        if method == "POST" && path == "/897/boards/b1/cards.json" {
            let number = 100 + self.cards.lock().unwrap().len() as u64;
            let mut created = card(number, body["card"]["title"].as_str().unwrap_or(""), &[], None);
            created["description"] = json!(crate::html::to_text(body["card"]["description"].as_str().unwrap_or("")));
            created["steps"] = json!([]);
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
                let id = format!("cm{}", self.comment_tokens.lock().unwrap().len() + 1);
                let comment = json!({"id": id, "created_at": "2026-09-30T09:00:00Z", "creator": {"id": "fz-hermes", "name": "Hermes"},
                    "body": {"html": body["comment"]["body"], "plain_text": crate::html::to_text(body["comment"]["body"].as_str().unwrap())}});
                self.comments.lock().unwrap().entry(number).or_default().push(comment.clone());
                self.comment_tokens.lock().unwrap().push((number, id, token.to_string()));
                return Some((201, comment));
            }
            ("DELETE", comment) if comment.starts_with("comments/") => {
                let id = caps[4].to_string();
                let tokens = self.comment_tokens.lock().unwrap();
                let (_, _, author) = tokens.iter().find(|(card, known, _)| *card == number && *known == id)?;
                // Only the comment's creator may delete it.
                if author != token {
                    return Some((403, Value::Null));
                }
                self.comments.lock().unwrap().entry(number).or_default().retain(|comment| comment["id"] != json!(id));
                return Some((204, Value::Null));
            }
            ("POST", "steps") => {
                let steps = card["steps"].as_array_mut()?;
                let step = json!({"id": format!("s{}", steps.len() + 1), "content": body["step"]["content"], "completed": false});
                steps.push(step.clone());
                return Some((201, step));
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
            let json = |status: u16, body: Value| Ok(HttpResponse { status, body: body.to_string().into_bytes(), link: None, total: None });
            if method != "GET" {
                let target = format!("{path} {}", body.as_ref().map(Value::to_string).unwrap_or_default());
                if let Some((fragment, status)) = self.fail_writes.lock().unwrap().clone()
                    && target.contains(&fragment)
                {
                    return json(status, json!({"status": status}));
                }
                let token = headers
                    .iter()
                    .find(|(name, _)| *name == "Authorization")
                    .map(|(_, value)| value.trim_start_matches("Bearer ").to_string())
                    .unwrap_or_default();
                return match self.write(method, path, body.as_ref().unwrap_or(&Value::Null), &token) {
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
            if path == "/my/identity.json" {
                let token =
                    headers.iter().find(|(name, _)| *name == "Authorization").map(|(_, v)| v.trim_start_matches("Bearer ").to_string());
                if let Some(user) = token.and_then(|token| self.identities.lock().unwrap().get(&token).cloned()) {
                    return json(200, json!({"accounts": [{"slug": "/897", "user": {"id": user}}]}));
                }
            }
            let (list, page) = match path.split_once("?page=") {
                Some((list, page)) => (list, page.parse::<u32>().unwrap()),
                None => (path, 1),
            };
            if let Some(number) =
                list.strip_prefix("/897/cards/").and_then(|rest| rest.strip_suffix("/comments.json")).and_then(|n| n.parse::<u64>().ok())
                && self.cards.lock().unwrap().contains_key(&number)
            {
                // Oldest first, in geared pages, as Fizzy lists them.
                let all = self.comments.lock().unwrap().get(&number).cloned().unwrap_or_default();
                let (from, to) = (crate::fizzy::page_offset(page) as usize, crate::fizzy::page_offset(page + 1) as usize);
                let items = all.get(from.min(all.len())..to.min(all.len())).unwrap_or_default().to_vec();
                let link = (to < all.len()).then(|| format!("<http://localhost:8484{list}?page={}>; rel=\"next\"", page + 1));
                let total = (!*self.no_total_count.lock().unwrap()).then_some(all.len() as u64);
                return Ok(HttpResponse { status: 200, body: Value::Array(items).to_string().into_bytes(), link, total });
            }
            match self.replies.lock().unwrap().get(path) {
                Some((status, body, link)) => {
                    Ok(HttpResponse { status: *status, body: body.to_string().into_bytes(), link: link.clone(), total: None })
                }
                None => Ok(HttpResponse { status: 404, body: br#"{"status":404,"error":"Not Found"}"#.to_vec(), link: None, total: None }),
            }
        })
    }
}

/// The config, with its storage (settings, write log, Hermes log, proposals) in a fresh directory.
fn config() -> WorkspaceConfig {
    config_with(&[])
}

fn config_with(extra: &[(&str, &str)]) -> WorkspaceConfig {
    let storage = crate::store::scratch_dir("tests").display().to_string();
    let extra: HashMap<String, String> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    WorkspaceConfig::from_lookup(|name| match name {
        "FIZZY_URL" => Some("http://fizzy".into()),
        "FIZZY_TOKEN" => Some(TOKEN.into()),
        "FIZZY_PUBLIC_URL" => Some("https://fizzy.example".into()),
        "CAMPFIRE_STORAGE_PATH" => Some(storage.clone()),
        other => extra.get(other).cloned(),
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
    assert!(
        chips[&12].contains("Lift B out of service")
            && chips[&12].contains(r#"href="/workspace/cards/12""#)
            && !chips[&12].contains("fizzy.example")
    );
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

    /// The viewer is a member of the rooms of these messages.
    fn room_ids(&self) -> BoxFuture<'_, Result<Vec<i64>, String>> {
        let rooms = self.0.iter().map(|message| message.room_id).collect::<BTreeSet<_>>().into_iter().collect();
        Box::pin(async move { Ok(rooms) })
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
use crate::pages::{BoardFilter, CardSheet};
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
            Department { name: "Engineering".into(), tag: "engineering".into(), rooms: vec![3], restricted: false },
            Department { name: "Security".into(), tag: "security".into(), rooms: vec![4], restricted: false },
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
    let workspace = Workspace::new(config())
        .with_settings(scratch_store(settings))
        .with_audit(move |record| sink.lock().unwrap().push(record.clone()))
        .with_clock(now);
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
    let new = NewCard {
        title: "x".into(),
        description: String::new(),
        severity: None,
        departments: vec![],
        tags: vec![],
        steps: vec![],
        source: None,
    };
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
        WriteRecord {
            at: now(),
            user_id: 7,
            card: Some(12),
            action: "tag -sev-low".into(),
            identity: "workspace",
            outcome: "ok".into(),
            via: "workspace",
            reference: None
        }
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
        message_path: "/rooms/3/@55".into(),
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
        comment, "<p>Karim: Created from Maya’s message in engineering (in MeshDuty at /rooms/3/@55).</p>",
        "no CAMPFIRE_PUBLIC_URL: the path, as text"
    );
    assert_eq!(records.lock().unwrap().len(), 4, "create, two tags, comment");

    let snapshot = workspace.snapshot();
    assert!(snapshot.open.contains(&number) && snapshot.card(number).is_some(), "cached before the next poll");
    let panel = workspace.room_panel(&karim(), 3).unwrap();
    assert_eq!(panel.cards[0].number, number, "most severe first in the room's panel");
    assert!(workspace.chip(number).unwrap().contains("Pool pump knocking"));
}

#[test]
fn the_link_back_to_the_message_uses_the_configured_url() {
    let source = crate::actions::CardSource { message_path: "/rooms/3/@55".into(), room_name: "#ops".into(), author_name: "Maya".into() };
    let configured = WorkspaceConfig { campfire_url: Some("https://chat.example".into()), ..config() };
    assert_eq!(
        source.comment(&configured),
        ("Created from Maya’s message in #ops:".to_string(), Some("https://chat.example/rooms/3/@55".to_string()))
    );
    assert_eq!(source.comment(&config()), ("Created from Maya’s message in #ops (in MeshDuty at /rooms/3/@55).".to_string(), None));
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
        tags: vec![],
        steps: vec![],
        source: None,
    };
    let created = workspace.create_card(&fizzy, &karim(), new).await.unwrap();
    let warning = created.warning.unwrap();
    assert!(warning.starts_with(&format!("Card #{} was created, but its tags couldn't be set", created.card.number)), "{warning}");
    assert!(workspace.snapshot().card(created.card.number).is_some());

    *fizzy.fail_writes.lock().unwrap() = Some(("/boards/b1/cards".into(), 500));
    let new = NewCard {
        title: "Other".into(),
        description: String::new(),
        severity: None,
        departments: vec![],
        tags: vec![],
        steps: vec![],
        source: None,
    };
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
    for expected in
        ["Lift B out of service", "Call the lift company", "Called them &#60;script&#62;", "Stuck on 6.", "data-ws-change=\"severity\""]
    {
        assert!(html.contains(expected), "{expected} {html}");
    }
    assert!(!html.contains("<script>"));

    // Fizzy links: duty managers and administrators, on the LAN only; staff never.
    assert!(!sheet.fizzy_links && !html.contains("fizzy.example") && !html.contains("ws-more"), "{html}");
    assert!(!workspace.fizzy_links(&karim(), true), "not a duty manager");
    assert!(!workspace.fizzy_links(&manager(), false), "a duty manager, through the public address");
    assert!(workspace.fizzy_links(&manager(), true));
    let listed = Settings { duty_managers: Some(vec![karim().id]), ..departments() };
    workspace.settings_store().save(listed).unwrap();
    assert!(workspace.fizzy_links(&karim(), true) && workspace.fizzy_links(&manager(), true), "listed, and administrators");
    workspace.settings_store().save(departments()).unwrap();
    let managers = askama::Template::render(&CardSheet { fizzy_links: true, ..sheet.clone() }).unwrap();
    assert!(
        managers.contains(
            r#"<a class="ws-link" href="https://fizzy.example/897/cards/12" target="_blank" rel="noopener">Open in Fizzy (browser) ↗</a>"#
        ),
        "{managers}"
    );

    let settings = Settings { confirm_policy: Policy::DutyManagersOnly, ..departments() };
    workspace.settings_store().save(settings).unwrap();
    let read_only = askama::Template::render(&workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap()).unwrap();
    assert!(read_only.contains(r#"data-ws-change="severity" disabled"#) && !read_only.contains("data-ws-comment"));
}

#[tokio::test]
async fn the_sheet_shows_the_newest_comments() {
    let (fizzy, workspace, _) = working(departments()).await;
    let comment = |n: usize| {
        json!({"id": format!("c{n}"), "created_at": "2026-09-30T08:00:00Z", "creator": {"id": "u", "name": "Maya"},
            "body": {"plain_text": format!("Comment number {n}."), "html": ""}})
    };
    fizzy.comments.lock().unwrap().insert(12, (1..=250).map(comment).collect());
    let texts = |sheet: &CardSheet| sheet.comments.iter().map(|c| c.text.clone()).collect::<Vec<_>>();

    let sheet = workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap();
    let expected: Vec<String> = (151..=250).map(|n| format!("Comment number {n}.")).collect();
    assert_eq!(texts(&sheet), expected, "the newest 100, oldest first");
    assert!(sheet.earlier_comments);
    let pages: Vec<String> = fizzy.paths().into_iter().filter(|path| path.contains("/comments.json")).collect();
    assert_eq!(pages, ["/897/cards/12/comments.json", "/897/cards/12/comments.json?page=4", "/897/cards/12/comments.json?page=5"]);
    let html = askama::Template::render(&sheet).unwrap();
    assert!(
        html.contains(r#"<a class="ws-link" href="/workspace/cards/12?comments=all" data-ws-earlier-comments>Show earlier comments</a>"#),
        "{html}"
    );
    assert!(html.find("data-ws-earlier-comments").unwrap() < html.find("Comment number 151.").unwrap(), "above the thread");
    assert!(html.contains("Comment number 250.") && !html.contains("Comment number 150."));
    assert!(!html.contains("fizzy.example"), "no way out to Fizzy for staff");

    // "Show earlier comments": all of them, through the workspace's own client.
    let all = workspace.card_sheet_with(&fizzy, &karim(), 12, true).await.unwrap();
    assert_eq!(texts(&all), (1..=250).map(|n| format!("Comment number {n}.")).collect::<Vec<_>>());
    assert!(!all.earlier_comments && all.all_comments);
    let html = askama::Template::render(&all).unwrap();
    assert!(!html.contains("data-ws-earlier-comments") && html.contains("Comment number 1."), "{html}");
    // Capped: a longer thread says only the last ones are shown.
    fizzy.comments.lock().unwrap().insert(12, (1..=1030).map(comment).collect());
    let capped = workspace.card_sheet_with(&fizzy, &karim(), 12, true).await.unwrap();
    assert_eq!((capped.comments.len(), capped.earlier_comments), (crate::actions::SHEET_ALL_COMMENTS, true));
    assert_eq!(capped.comments[0].text, "Comment number 31.");
    let html = askama::Template::render(&capped).unwrap();
    assert!(html.contains("Only the last 1000 comments are shown.") && !html.contains("data-ws-earlier-comments"), "{html}");
    fizzy.comments.lock().unwrap().insert(12, (1..=250).map(comment).collect());
    assert!(html.contains(r#"data-ws-comments="all""#) && !askama::Template::render(&sheet).unwrap().contains("data-ws-comments"));

    // At most ALL_COMMENTS_AT_ONCE such reads at a time: one more is busy, before any request.
    let held = workspace.all_comments.try_acquire_many(crate::actions::ALL_COMMENTS_AT_ONCE as u32).unwrap();
    fizzy.requests.lock().unwrap().clear();
    let busy = workspace.card_sheet_with(&fizzy, &karim(), 12, true).await.unwrap_err();
    assert_eq!((busy.status(), busy.code()), (429, "busy"));
    assert!(fizzy.paths().is_empty(), "{:?}", fizzy.paths());
    assert_eq!(workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap().comments.len(), 100, "the newest 100 still read");
    drop(held);
    assert!(workspace.card_sheet_with(&fizzy, &karim(), 12, true).await.is_ok(), "free again");

    // Without X-Total-Count, the pages are walked.
    *fizzy.no_total_count.lock().unwrap() = true;
    let walked = workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap();
    assert_eq!(texts(&walked), expected);
    assert!(walked.earlier_comments);

    // A short thread is all there, with no note.
    fizzy.comments.lock().unwrap().insert(12, (1..=40).map(comment).collect());
    let short = workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap();
    assert_eq!((short.comments.len(), short.earlier_comments), (40, false));
    assert!(!askama::Template::render(&short).unwrap().contains("earlier comments"));
    assert!(!short.comments_truncated);

    // Without X-Total-Count, a thread longer than the page cap: flagged, and the sheet says so.
    fizzy.comments.lock().unwrap().insert(12, (1..=2000).map(comment).collect());
    let truncated = workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap();
    assert!(truncated.comments_truncated && truncated.earlier_comments);
    assert!(askama::Template::render(&truncated).unwrap().contains("the newest comments may be missing"));
    *fizzy.no_total_count.lock().unwrap() = false;
    assert!(!workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap().comments_truncated, "with the count, the newest are read");
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
    // Fizzy: the header's "⋯" menu, with `fizzy_links` only (an administrator on the LAN).
    assert!(!html.contains("fizzy.example") && !html.contains("ws-more"), "{html}");
    let mut board = workspace.board(&admin, &security);
    board.fizzy_links = workspace.fizzy_links(&admin, true);
    let html = askama::Template::render(&board).unwrap();
    assert!(
        html.contains(r#"href="https://fizzy.example/897/boards/b1" target="_blank" rel="noopener">Open in Fizzy (browser) ↗</a>"#),
        "{html}"
    );
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
    let page = crate::pages::SettingsPage { hermes_user_learned: Some("fz-hermes".into()), ..page };
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains(r#"value="engineering""#) && html.contains("security &#60;desk&#62;"));
    assert!(html.contains(r#"value="admins" checked"#) && html.contains(r#"value="anyone" checked"#));
    assert!(html.contains("data-ws-policy-scope"));
    // Phase 2: the dial, D5's defaults checked, the fixed Never, and the honest note (D7).
    assert!(html.contains(r#"name="autonomy-create" value="ask_first" aria-label="Create a card: Ask first" checked"#), "{html}");
    assert!(html.contains(r#"name="autonomy-comment" value="alone" aria-label="Comment on a card: Alone" checked"#));
    assert!(html.contains(r#"name="autonomy-close" value="ask_first" aria-label="Close a card: Ask first" checked"#));
    assert!(html.contains("Delete or reassign a card") && html.contains("Never (fixed)"));
    assert!(html.contains("Sky keeps its own Fizzy account.") && html.contains("fz-hermes (from HERMES_FIZZY_TOKEN)"));
    // Phase 2.5–2.7: visibility (everyone by default, with the honest note), alerts, the handover.
    assert!(html.contains(r#"name="visibility_mode" value="everyone" checked"#), "{html}");
    assert!(html.contains("This only hides cards in MeshDuty") && html.contains("Anyone with a Fizzy login sees the whole board"));
    assert!(html.contains("Only administrators can create rooms"));
    assert!(html.contains(r#"<input type="checkbox" name="restricted">"#), "{html}");
    assert!(html.contains(r#"name="alerts_enabled" checked"#) && html.contains(r#"name="alert_severities" value="critical" checked"#));
    assert!(
        html.contains(r#"name="alert_severities" value="medium">"#)
            && html.contains(r#"name="new_reminder_min" min="0" max="1440" value="15""#)
    );
    assert!(html.contains(r#"<option value="" selected>None yet"#) && html.contains(r#"<option value="3">engineering</option>"#), "{html}");
    assert!(html.contains(r#"name="shift_ends" value="07:00, 15:00, 23:00""#) && html.contains(r#"name="time_zone" value="Europe/Paris""#));
    assert!(!html.contains("No department is marked restricted"));
    let mut restricting = departments();
    restricting.visibility.mode = crate::visibility::Mode::ByDepartmentRoom;
    let page = crate::pages::settings_page(workspace.config(), &snapshot, &restricting, &rooms, &[], &[], None);
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains("No department is marked restricted"), "a restriction that hides nothing says so");

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
    assert_eq!(
        json["departments"][0],
        json!({"name": "Engineering", "tag": "engineering", "restricted": false, "rooms": [{"id": 3, "name": "engineering"}]})
    );
    assert_eq!(json["severity_tags"], json!(["sev-low", "sev-medium", "sev-high", "sev-critical"]));
    assert_eq!(json["duty_managers"], json!([{"id": 1, "name": "Ann"}]));
    assert_eq!(json["confirm_policy"], "anyone");
    assert_eq!(json["autonomy"]["create"], "ask_first");
    assert_eq!(json["autonomy"]["tag"], "alone");
}

// --- Phase 2: supervising Hermes -------------------------------------------------------------------

use crate::drafts::Decision;
use crate::hermes::Proposed;
use crate::hermes_log::{Kind, Reverse, Via};
use crate::proposals::{Context, LIVE_REPORT_OPENING, SourceMessage, Status};
use crate::settings::Dial;

const HERMES_TOKEN: &str = "h3rmes-token";
const HERMES_FEED: &str = "/897/activities.json?creator_ids%5B%5D=fz-hermes&board_ids%5B%5D=b1";
const BOARD_FEED: &str = "/897/activities.json?board_ids%5B%5D=b1";

type Clock = std::sync::Arc<Mutex<Timestamp>>;

fn hermes_bot() -> Bot {
    Bot { id: 9, name: "Hermes".into(), sgid: "sg".into() }
}

fn maya() -> Viewer {
    Viewer { id: 5, name: "Maya".into(), email: None, administrator: false }
}

/// An administrator: a duty manager while none are listed.
fn manager() -> Viewer {
    Viewer { id: 1, name: "Manager".into(), email: None, administrator: true }
}

/// What the bridge knew: Maya's voice note in front-desk (room 3).
fn for_maya() -> Context {
    Context {
        source: Some("voice_note".into()),
        room_id: Some(3),
        room_name: Some("front-desk".into()),
        user_id: Some(5),
        user_name: Some("Maya".into()),
        message_id: Some(55),
        live_report: false,
    }
}

fn all_alone() -> Settings {
    let mut settings = departments();
    settings.autonomy = crate::settings::Autonomy {
        create: Dial::Alone,
        comment: Dial::Alone,
        tag: Dial::Alone,
        move_card: Dial::Alone,
        close: Dial::Alone,
        step: Dial::Alone,
    };
    settings
}

/// A polled workspace for phase 2: the workspace's token is the "Campfire" Fizzy user, Hermes's
/// token (when given) is Hermes's; card 12 is live; the clock can be moved.
async fn phase2(settings: Settings, hermes_token: bool) -> (FakeFizzy, Workspace, Clock) {
    let fizzy = fizzy();
    let mut live = card(12, "Lift B out of service", &["engineering", "incident", "sev-low"], None);
    live["steps"] = json!([{"id": "s1", "content": "Call the lift company", "completed": false}]);
    fizzy.live(live);
    fizzy.live(elsewhere(card(78, "Payroll export", &[], None)));
    fizzy.identities.lock().unwrap().insert(TOKEN.into(), "fz-campfire".into());
    fizzy.identities.lock().unwrap().insert(HERMES_TOKEN.into(), "fz-hermes".into());
    fizzy.reply(HERMES_FEED, json!([]));
    fizzy.reply(BOARD_FEED, json!([]));
    let config = if hermes_token { config_with(&[("HERMES_FIZZY_TOKEN", HERMES_TOKEN)]) } else { config() };
    let clock: Clock = std::sync::Arc::new(Mutex::new(now()));
    let time = clock.clone();
    let workspace = Workspace::new(config).with_settings(scratch_store(settings)).with_clock(move || *time.lock().unwrap());
    workspace.set_bots(vec![hermes_bot()]);
    workspace.poll(&fizzy, now()).await.unwrap();
    fizzy.requests.lock().unwrap().clear();
    (fizzy, workspace, clock)
}

async fn propose(fizzy: &FakeFizzy, workspace: &Workspace, body: Value) -> Result<Proposed, ActionError> {
    workspace.propose(fizzy, &hermes_bot(), &body, for_maya(), None).await
}

fn pending_id(proposed: Proposed) -> String {
    match proposed {
        Proposed::Pending { proposal, duplicate: false } => proposal.id,
        other => panic!("{other:?}"),
    }
}

fn done(proposed: Proposed) -> crate::proposals::Proposal {
    match proposed {
        Proposed::Done { proposal } => proposal,
        other => panic!("{other:?}"),
    }
}

/// The token of every write, in order.
fn write_tokens(fizzy: &FakeFizzy) -> Vec<String> {
    fizzy
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, ..)| method != "GET")
        .map(|(_, _, headers, _)| headers.iter().find(|(n, _)| *n == "Authorization").unwrap().1.trim_start_matches("Bearer ").to_string())
        .collect()
}

#[tokio::test]
async fn proposals_follow_the_dial() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    assert_eq!(workspace.fizzy_users().hermes.as_deref(), Some("fz-hermes"), "learned from HERMES_FIZZY_TOKEN");
    assert_eq!(workspace.fizzy_users().workspace.as_deref(), Some("fz-campfire"));
    assert!(!workspace.shares_hermes_token());

    // Comment: Alone (D5). Run at once, as Hermes, logged with its way back.
    let proposal =
        done(propose(&fizzy, &workspace, json!({"action": "comment", "card": 12, "body": "Lift company called"})).await.unwrap());
    assert_eq!((proposal.status, proposal.result_card), (Status::Done, Some(12)));
    let (_, path, body) = fizzy.writes().pop().unwrap();
    assert_eq!(
        (path.as_str(), body.unwrap()["comment"]["body"].clone()),
        ("/897/cards/12/comments.json", json!("<p>Lift company called</p>"))
    );
    assert_eq!(write_tokens(&fizzy), [HERMES_TOKEN], "Hermes's own token: Fizzy shows Hermes as the author");
    let entry = workspace.hermes_log().get(&format!("p-{}-done", proposal.id)).unwrap();
    assert_eq!((entry.kind, entry.via, entry.card, entry.for_user_id), (Kind::Commented, Via::Campfire, Some(12), Some(5)));
    assert_eq!(entry.source.as_deref(), Some("voice note"));
    assert_eq!(entry.reverse, Some(Reverse::DeleteComment { comment_id: "cm1".into(), identity: "hermes".into() }));
    let write = workspace.journal().recent().pop().unwrap();
    assert_eq!(
        (write.via.as_str(), write.identity.as_str(), write.reference.as_deref()),
        ("proposal", "hermes", Some(proposal.id.as_str()))
    );
    let stored = std::fs::read_to_string(workspace.config().storage_file("actions.jsonl")).unwrap();
    assert!(stored.contains(r#""via":"proposal""#) && !stored.contains(HERMES_TOKEN) && !stored.contains(TOKEN));

    // Create: Ask first. Nothing written; kept, logged, a draft to post.
    fizzy.requests.lock().unwrap().clear();
    let create = json!({"action": "create", "title": "Lost property — laptop bag", "severity": "low", "department": "engineering",
        "tags": ["incident"], "steps": ["Call the guest"], "description": "Bag left in the lobby"});
    let id = pending_id(propose(&fizzy, &workspace, create.clone()).await.unwrap());
    assert!(fizzy.writes().is_empty(), "nothing written before a confirmation");
    let kept = std::fs::read_to_string(workspace.config().storage_file("proposals.json")).unwrap();
    assert!(kept.contains(&id) && kept.contains(r#""status": "pending""#), "{kept}");
    assert_eq!(workspace.hermes_log().get(&format!("p-{id}-proposed")).unwrap().kind, Kind::Proposed);
    match propose(&fizzy, &workspace, create).await.unwrap() {
        Proposed::Pending { proposal, duplicate: true } => assert_eq!(proposal.id, id, "the same proposal twice is one"),
        other => panic!("{other:?}"),
    }
    assert_eq!(workspace.pending_proposals().len(), 1);

    // Never: refused, nothing written, logged.
    let mut settings = (*workspace.settings()).clone();
    settings.autonomy.close = Dial::Never;
    workspace.settings_store().save(settings).unwrap();
    match propose(&fizzy, &workspace, json!({"action": "close", "card": 12})).await.unwrap() {
        Proposed::Refused { message } => assert!(message.contains("Never"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(fizzy.writes().is_empty());
    assert!(workspace.hermes_log().entries().iter().any(|entry| entry.kind == Kind::Refused));

    // What a person couldn't ask for, or a card elsewhere: refused before anything is kept.
    let before = workspace.proposals().all().len();
    let unknown = propose(&fizzy, &workspace, json!({"action": "delete", "card": 12})).await.unwrap_err();
    assert_eq!((unknown.status(), unknown.code()), (422, "unknown_action"));
    let reassign = propose(&fizzy, &workspace, json!({"action": "assign", "card": 12})).await.unwrap_err();
    assert_eq!(reassign.status(), 422);
    let elsewhere = propose(&fizzy, &workspace, json!({"action": "comment", "card": 78, "body": "hi"})).await.unwrap_err();
    assert_eq!(elsewhere, ActionError::NotFound);
    assert_eq!(workspace.proposals().all().len(), before);
    assert!(fizzy.writes().is_empty());
}

#[tokio::test]
async fn a_confirmed_proposal_runs_as_hermes_and_says_who_confirmed() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    let create = json!({"action": "create", "title": "Lost property — laptop bag", "severity": "low", "department": "engineering",
        "tags": ["Incident"], "steps": ["Call the guest"], "description": "Bag left in the lobby"});
    let id = pending_id(propose(&fizzy, &workspace, create).await.unwrap());
    let proposal = workspace.proposals().get(&id).unwrap();

    // The draft Campfire posts as the bot gets buttons that decide this proposal; the same
    // marker from a person gets nothing.
    let draft = crate::proposals::draft_html(&proposal, None);
    assert!(
        workspace.decorate_message(100, 9, &draft).is_none_or(|html| !html.contains("data-ws-proposal")),
        "not before Campfire posted it"
    );
    workspace.set_draft_message(&id, 100);
    assert!(
        workspace.decorate_message(102, 9, &draft).is_none_or(|html| !html.contains("data-ws-proposal")),
        "the same marker in another message of the bot gets nothing"
    );
    let decorated = workspace.decorate_message(100, 9, &draft).unwrap();
    assert!(decorated.contains(&format!(r#"data-ws-proposal="{id}""#)), "{decorated}");
    assert!(decorated.contains(&format!(r#"data-ws-draft-url="/workspace/hermes/proposals/{id}/decision""#)));
    assert!(decorated.contains(">File</button>"));
    assert!(workspace.decorate_message(101, 5, &draft).is_none_or(|html| !html.contains("data-ws-proposal")));
    assert!(
        drafts::pending(
            &[ChatMessage {
                id: 100,
                room_id: 3,
                room_name: "front-desk".into(),
                url: "/rooms/3/@100".into(),
                creator_id: 9,
                creator_name: "Hermes".into(),
                creator_is_bot: true,
                created_at: now(),
                body_html: draft.clone()
            }],
            now()
        )
        .is_empty(),
        "not a text draft too"
    );

    let karim = karim();
    let filed = workspace.decide(&fizzy, &karim, &id, Decision::Confirm).await.unwrap();
    assert_eq!((filed.status, filed.decided_by_name.as_deref()), (Status::Done, Some("Karim")));
    let number = filed.result_card.unwrap();
    assert_eq!(tags(&fizzy, number), ["engineering", "incident", "sev-low"]);
    assert_eq!(fizzy.live_card(number)["steps"][0]["content"], "Call the guest");
    let comment = fizzy.writes().last().unwrap().2.clone().unwrap()["comment"]["body"].as_str().unwrap().to_string();
    assert_eq!(comment, "<p>Created from Maya’s message in front-desk (in MeshDuty at /rooms/3/@55).</p>", "Hermes's own token: no prefix");
    assert!(write_tokens(&fizzy).iter().all(|token| token == HERMES_TOKEN));
    let entry = workspace.hermes_log().get(&format!("p-{id}-done")).unwrap();
    assert_eq!((entry.kind, entry.card, entry.by_name.as_deref()), (Kind::Created, Some(number), Some("Karim")));
    assert_eq!(entry.reverse, Some(Reverse::CloseCreated));
    assert!(workspace.snapshot().card(number).is_some(), "cached at once");

    let again = workspace.decide(&fizzy, &karim, &id, Decision::Confirm).await.unwrap_err();
    assert_eq!(again.code(), "not_pending");
    assert!(again.message().contains("Confirmed by Karim"), "{again}");
    assert_eq!(workspace.proposal_status(&id).unwrap()["status"], "done");

    let other = pending_id(propose(&fizzy, &workspace, json!({"action": "move", "card": 12, "column": "In progress"})).await.unwrap());
    fizzy.requests.lock().unwrap().clear();
    let dismissed = workspace.decide(&fizzy, &karim, &other, Decision::Dismiss).await.unwrap();
    assert_eq!(dismissed.status, Status::Dismissed);
    assert!(fizzy.writes().is_empty());
    assert_eq!(workspace.hermes_log().get(&format!("p-{other}-dismissed")).unwrap().by_name.as_deref(), Some("Karim"));
    assert_eq!(workspace.decide(&fizzy, &karim, "nope", Decision::Confirm).await.unwrap_err(), ActionError::NotFound);
}

#[tokio::test]
async fn proposals_expire_and_the_policy_says_who_confirms() {
    let settings = Settings { confirm_policy: Policy::AuthorOrDutyManager, duty_managers: Some(vec![1]), ..departments() };
    let (fizzy, workspace, clock) = phase2(settings, true).await;
    let id = pending_id(propose(&fizzy, &workspace, json!({"action": "close", "card": 12})).await.unwrap());
    let refused = workspace.decide(&fizzy, &karim(), &id, Decision::Confirm).await.unwrap_err();
    assert_eq!(refused.status(), 403, "not the person it's for, not a duty manager");
    assert!(workspace.decide(&fizzy, &maya(), &id, Decision::Dismiss).await.is_ok(), "the person it's for");

    let late = pending_id(propose(&fizzy, &workspace, json!({"action": "close", "card": 12})).await.unwrap());
    *clock.lock().unwrap() = now() + SignedDuration::from_hours(25);
    assert!(workspace.pending_proposals().is_empty());
    assert_eq!(workspace.proposals().get(&late).unwrap().status, Status::Expired);
    let entry = workspace.hermes_log().get(&format!("p-{late}-expired")).unwrap();
    assert!(entry.text.starts_with("Dismissed (timed out): Close #12"), "{}", entry.text);
    let error = workspace.decide(&fizzy, &manager(), &late, Decision::Confirm).await.unwrap_err();
    assert!(error.message().contains("timed out"), "{error}");
    assert!(fizzy.writes().is_empty());
    assert!(!fizzy.live_card(12)["closed"].as_bool().unwrap());
}

#[tokio::test]
async fn a_confirmed_live_report_is_filed_at_once_but_only_once() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    let report = SourceMessage {
        id: 55,
        room_id: 3,
        creator_id: 5,
        creator_is_bot: false,
        created_at: now() - SignedDuration::from_mins(2),
        body_html: format!("<p>{LIVE_REPORT_OPENING}.</p><p>Chute dans le hall</p>"),
    };
    let create = json!({"action": "create", "title": "Chute — hall", "severity": "high"});
    let filed = workspace.propose(&fizzy, &hermes_bot(), &create, for_maya(), Some(report.clone())).await.unwrap();
    let proposal = done(filed);
    assert!(proposal.context.live_report);
    let entry = workspace.hermes_log().get(&format!("p-{}-done", proposal.id)).unwrap();
    assert_eq!(entry.source.as_deref(), Some("live voice report"));
    let second = json!({"action": "create", "title": "Chute — hall (2)"});
    let second = workspace.propose(&fizzy, &hermes_bot(), &second, for_maya(), Some(report.clone())).await.unwrap();
    assert!(matches!(second, Proposed::Pending { .. }), "a second card from the same report asks first");
    let someone_else = Context { user_id: Some(6), ..for_maya() };
    let other = json!({"action": "create", "title": "x"});
    let other = workspace.propose(&fizzy, &hermes_bot(), &other, someone_else, Some(report)).await.unwrap();
    assert!(matches!(other, Proposed::Pending { .. }), "not the reporter's own confirmation");
}

#[tokio::test]
async fn without_hermes_s_token_campfire_writes_for_hermes_as_the_workspace() {
    let (fizzy, workspace, _) = phase2(departments(), false).await;
    let proposal =
        done(propose(&fizzy, &workspace, json!({"action": "comment", "card": 12, "body": "Lift company called"})).await.unwrap());
    let (.., body) = fizzy.writes().pop().unwrap();
    assert_eq!(body.unwrap()["comment"]["body"], "<p>Hermes: Lift company called</p>");
    assert_eq!(write_tokens(&fizzy), [TOKEN]);
    let entry = workspace.hermes_log().get(&format!("p-{}-done", proposal.id)).unwrap();
    assert_eq!(entry.reverse, Some(Reverse::DeleteComment { comment_id: "cm1".into(), identity: "workspace".into() }));
}

#[tokio::test]
async fn undo_takes_back_what_campfire_ran_for_hermes() {
    let (fizzy, workspace, clock) = phase2(all_alone(), true).await;
    let undo_last = |proposal: &crate::proposals::Proposal| format!("p-{}-done", proposal.id);

    let severity = done(propose(&fizzy, &workspace, json!({"action": "severity", "card": 12, "severity": "high"})).await.unwrap());
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident", "sev-high"]);
    *clock.lock().unwrap() = now() + SignedDuration::from_mins(1);
    let undone = workspace.undo(&fizzy, &manager(), &undo_last(&severity)).await.unwrap();
    assert_eq!(tags(&fizzy, 12), ["engineering", "incident", "sev-low"], "back to what it was");
    assert_eq!((undone.kind, undone.by_name.as_deref()), (Kind::Undone, Some("Manager")));
    let again = workspace.undo(&fizzy, &manager(), &undo_last(&severity)).await.unwrap_err();
    assert_eq!((again.code(), again.message()), ("already_undone", "Already undone by Manager.".to_string()));
    assert!(
        workspace
            .journal()
            .recent()
            .iter()
            .any(|write| write.via == "undo" && write.reference.as_deref() == Some(undo_last(&severity).as_str()))
    );

    let moved = done(propose(&fizzy, &workspace, json!({"action": "move", "card": 12, "column": "In progress"})).await.unwrap());
    assert_eq!(fizzy.live_card(12)["column"]["id"], "c1");
    workspace.undo(&fizzy, &maya(), &undo_last(&moved)).await.unwrap();
    assert!(fizzy.live_card(12)["column"].is_null(), "back to New; the person it was for may undo");

    let comment = done(propose(&fizzy, &workspace, json!({"action": "comment", "card": 12, "body": "Wrong card, sorry"})).await.unwrap());
    fizzy.requests.lock().unwrap().clear();
    workspace.undo(&fizzy, &manager(), &undo_last(&comment)).await.unwrap();
    assert!(fizzy.comments.lock().unwrap()[&12].iter().all(|c| c["body"]["plain_text"] != "Wrong card, sorry"), "deleted");
    assert_eq!(write_tokens(&fizzy), [HERMES_TOKEN], "with the token that wrote it: Fizzy lets only its creator delete it");

    let created = done(propose(&fizzy, &workspace, json!({"action": "create", "title": "Duplicate report"})).await.unwrap());
    let number = created.result_card.unwrap();
    fizzy.requests.lock().unwrap().clear();
    workspace.undo(&fizzy, &manager(), &undo_last(&created)).await.unwrap();
    let card = fizzy.live_card(number);
    assert!(card["closed"].as_bool().unwrap(), "closed, not deleted");
    assert!(fizzy.writes().iter().all(|(method, ..)| method != "DELETE"));
    let comment = fizzy.comments.lock().unwrap()[&number].last().unwrap()["body"]["html"].as_str().unwrap().to_string();
    assert!(comment.starts_with("<p>Manager: Undone from MeshDuty: Sky created this card by mistake."), "{comment}");
    assert!(workspace.snapshot().card(number).unwrap().closed);
}

#[tokio::test]
async fn undo_is_refused_when_it_isn_t_safe_or_allowed() {
    let (fizzy, workspace, clock) = phase2(all_alone(), true).await;
    let severity = done(propose(&fizzy, &workspace, json!({"action": "severity", "card": 12, "severity": "high"})).await.unwrap());
    let id = format!("p-{}-done", severity.id);

    let forbidden = workspace.undo(&fizzy, &karim(), &id).await.unwrap_err();
    assert_eq!(forbidden.status(), 403, "not a duty manager, not the person it was for (D8)");

    // Someone commented from Campfire since: the card was touched.
    *clock.lock().unwrap() = now() + SignedDuration::from_mins(1);
    workspace.change_card(&fizzy, &karim(), 12, Change::Comment("On it".into())).await.unwrap();
    let touched = workspace.undo(&fizzy, &maya(), &id).await.unwrap_err();
    assert_eq!(touched.code(), "changed_since");
    assert!(touched.message().contains("from MeshDuty"), "{touched}");

    // Someone changed the severity: the state isn't what Hermes left.
    let step = done(propose(&fizzy, &workspace, json!({"action": "step", "card": 12, "step_id": "s1", "completed": true})).await.unwrap());
    let step_id = format!("p-{}-done", step.id);
    fizzy.live({
        let mut card = fizzy.live_card(12);
        card["steps"][0]["completed"] = json!(false);
        card
    });
    assert_eq!(workspace.undo(&fizzy, &manager(), &step_id).await.unwrap_err().code(), "changed_since");

    // Fizzy's feed shows a change since, by someone in Fizzy.
    let comment = done(propose(&fizzy, &workspace, json!({"action": "comment", "card": 12, "body": "x"})).await.unwrap());
    fizzy.reply(
        BOARD_FEED,
        json!([{"id": "later", "action": "card_triaged", "created_at": "2026-09-30T09:05:00Z",
            "eventable_type": "Card", "eventable": {"number": 12}, "creator": {"id": "fz-karim", "name": "Karim"}}]),
    );
    let in_fizzy = workspace.undo(&fizzy, &manager(), &format!("p-{}-done", comment.id)).await.unwrap_err();
    assert_eq!(in_fizzy.code(), "changed_since");
    assert!(in_fizzy.message().starts_with("Karim changed the card in Fizzy since"), "{in_fizzy}");

    // Too old.
    *clock.lock().unwrap() = now() + SignedDuration::from_hours(25);
    assert_eq!(workspace.undo(&fizzy, &manager(), &format!("p-{}-done", comment.id)).await.unwrap_err().code(), "too_old");
    assert_eq!(workspace.undo(&fizzy, &manager(), "p-nope-done").await.unwrap_err(), ActionError::NotFound);
    let undone: Vec<_> = workspace.hermes_log().entries().into_iter().filter(|entry| entry.kind == Kind::Undone).collect();
    assert!(undone.is_empty(), "nothing was undone");
}

#[tokio::test]
async fn the_hermes_log_reads_hermes_s_direct_actions() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    fizzy.reply(
        HERMES_FEED,
        json!([
            {"id": "d4", "action": "card_closed", "created_at": "2026-09-30T08:40:00Z", "eventable_type": "Card",
             "eventable": card(40, "Elsewhere", &[], None), "board": {"id": "b2", "name": "Engineering"}, "creator": {"id": "fz-hermes", "name": "Hermes"}},
            {"id": "d3", "action": "card_triaged", "created_at": "2026-09-30T08:30:00Z", "particulars": {"column": "In progress"},
             "eventable_type": "Card", "eventable": card(12, "Lift B out of service", &[], Some("In progress")),
             "board": {"id": "b1", "name": "Incident Log"}, "creator": {"id": "fz-hermes", "name": "Hermes"}},
            {"id": "d2", "action": "comment_created", "created_at": "2026-09-30T08:20:00Z", "url": "http://localhost:8484/897/cards/13",
             "eventable_type": "Comment", "eventable": {"id": "cm9", "body": {"plain_text": "Filed for Maya", "html": "<p>Filed for Maya</p>"}},
             "board": {"id": "b1", "name": "Incident Log"}, "creator": {"id": "fz-hermes", "name": "Hermes"}},
            {"id": "d1", "action": "card_published", "created_at": "2026-09-30T08:10:00Z", "eventable_type": "Card",
             "eventable": card(13, "Guest slip in lobby", &["sev-critical"], None), "board": {"id": "b1", "name": "Incident Log"},
             "creator": {"id": "fz-hermes", "name": "Hermes"}},
            {"id": "d0", "action": "card_closed", "created_at": "2026-09-30T08:00:00Z", "eventable_type": "Card",
             "eventable": card(12, "Lift B", &[], None), "board": {"id": "b1", "name": "Incident Log"}, "creator": {"id": "fz-karim", "name": "Karim"}}
        ]),
    );
    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(fizzy.paths().contains(&HERMES_FEED.to_string()), "only Hermes's activities, on the incident board");
    let ids: Vec<String> = workspace.hermes_log().entries().iter().map(|entry| entry.id.clone()).collect();
    assert_eq!(ids, ["a-d3", "a-d2", "a-d1"], "not another board's, not someone else's");
    let created = workspace.hermes_log().get("a-d1").unwrap();
    assert_eq!((created.via, created.kind, created.text.as_str()), (Via::Direct, Kind::Created, "Created #13 Guest slip in lobby"));
    workspace.poll(&fizzy, now()).await.unwrap();
    assert_eq!(workspace.hermes_log().entries().len(), 3, "deduplicated by activity id");

    let restarted = Workspace::new(workspace.config().clone());
    assert_eq!(restarted.hermes_log().entries().len(), 3, "kept across a restart");

    // The page: direct lines say so; undo where it's safely derivable, for those allowed.
    let page = workspace.hermes_page(&manager(), "all", &Messages(Vec::new())).await.unwrap();
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains(r#"<span class="ws-via ws-via--direct">direct</span>"#), "{html}");
    assert!(html.contains(r#"data-ws-undo="/workspace/hermes/actions/a-d1/undo""#), "a card Hermes created: close + comment");
    assert!(html.contains(r#"data-ws-undo="/workspace/hermes/actions/a-d2/undo""#), "its comment: deleted with its token");
    assert!(!html.contains("actions/a-d3/undo"), "a move: Fizzy doesn't say from where");
    assert!(!html.contains("isn't known, so what it does directly"));
    // A line about a card opens its sheet; Fizzy only with `fizzy_links`.
    assert!(html.contains(r#"<a href="/workspace/cards/13" data-ws-open-sheet="13">Created #13 Guest slip in lobby</a>"#), "{html}");
    assert!(!html.contains("fizzy.example"), "{html}");
    let managers = askama::Template::render(&crate::hermes::HermesPage { fizzy_links: true, ..page.clone() }).unwrap();
    assert!(managers.contains(r#"href="https://fizzy.example/897/cards/13" target="_blank" rel="noopener""#), "{managers}");
    let for_karim = askama::Template::render(&workspace.hermes_page(&karim(), "all", &Messages(Vec::new())).await.unwrap()).unwrap();
    assert!(!for_karim.contains("data-ws-undo") && for_karim.contains("Duty managers and the person it was for can undo this."));

    // Undo of a direct creation: nobody touched it since (Hermes's own follow-ups don't count).
    fizzy.live(card(13, "Guest slip in lobby", &["sev-critical"], None));
    fizzy.reply(
        BOARD_FEED,
        json!([{"id": "d5", "action": "comment_created", "created_at": "2026-09-30T08:12:00Z", "url": "http://localhost:8484/897/cards/13",
            "eventable_type": "Comment", "eventable": {"id": "cm8"}, "creator": {"id": "fz-hermes", "name": "Hermes"}}]),
    );
    let triaged = workspace.undo(&fizzy, &manager(), "a-d3").await.unwrap_err();
    assert_eq!(triaged.code(), "cannot_undo");
    workspace.undo(&fizzy, &manager(), "a-d1").await.unwrap();
    assert!(fizzy.live_card(13)["closed"].as_bool().unwrap());
}

#[tokio::test]
async fn people_s_workspace_actions_aren_t_logged_as_hermes_s() {
    // No "Campfire" user yet: the workspace writes with Hermes's own token.
    let settings = Settings { hermes_fizzy_user_id: Some("fz-hermes".into()), ..departments() };
    let (fizzy, workspace, _) = phase2(settings, false).await;
    fizzy.identities.lock().unwrap().insert(TOKEN.into(), "fz-hermes".into());
    let workspace = Workspace::new(workspace.config().clone())
        .with_settings(scratch_store(Settings { hermes_fizzy_user_id: Some("fz-hermes".into()), ..departments() }));
    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(workspace.shares_hermes_token());
    workspace.change_card(&fizzy, &karim(), 12, Change::Move(Target::Closed)).await.unwrap();
    let closed_at = workspace.journal().recent().last().unwrap().at;
    let at = |offset: i64| (closed_at + SignedDuration::from_secs(offset)).to_string();
    fizzy.reply(
        HERMES_FEED,
        json!([
            {"id": "k1", "action": "card_closed", "created_at": at(20), "eventable_type": "Card", "eventable": card(12, "Lift B", &[], None),
             "board": {"id": "b1", "name": "Incident Log"}, "creator": {"id": "fz-hermes", "name": "Hermes"}},
            {"id": "h1", "action": "card_closed", "created_at": at(10), "eventable_type": "Card", "eventable": card(13, "Guest slip", &[], None),
             "board": {"id": "b1", "name": "Incident Log"}, "creator": {"id": "fz-hermes", "name": "Hermes"}}
        ]),
    );
    workspace.poll(&fizzy, now()).await.unwrap();
    let ids: Vec<String> = workspace.hermes_log().entries().iter().map(|entry| entry.id.clone()).collect();
    assert_eq!(ids, ["a-h1"], "Karim's close through the workspace isn't Hermes's");
    let html = askama::Template::render(&workspace.hermes_page(&manager(), "all", &Messages(Vec::new())).await.unwrap()).unwrap();
    assert!(html.contains("left out of this log"));
}

#[tokio::test]
async fn the_hermes_tab_lists_pending_proposals_and_filters_the_log() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    let id = pending_id(propose(&fizzy, &workspace, json!({"action": "close", "card": 12})).await.unwrap());
    workspace.set_draft_message(&id, 100);
    done(propose(&fizzy, &workspace, json!({"action": "comment", "card": 12, "body": "Seen"})).await.unwrap());
    let question = ChatMessage {
        id: 7,
        room_id: 3,
        room_name: "front-desk".into(),
        url: "/rooms/3/@7".into(),
        creator_id: 9,
        creator_name: "Hermes".into(),
        creator_is_bot: true,
        created_at: now() - SignedDuration::from_mins(5),
        body_html: "<p>Which room was it: 103 or 113?</p>".into(),
    };
    let page = workspace.hermes_page(&maya(), "all", &Messages(vec![question.clone()])).await.unwrap();
    assert_eq!(page.pending.len(), 1);
    assert_eq!(page.pending[0].message_url.as_deref(), Some("/rooms/3/@100"));
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains(&format!(r#"id="proposal-{id}""#)) && html.contains("Close #12 Lift B out of service"), "{html}");
    assert!(html.contains(r#"data-ws-draft-action="confirm">Confirm</button>"#));
    assert!(html.contains(r#"<span class="ws-via ws-via--campfire">via MeshDuty</span>"#));
    assert!(html.contains("Commented on #12 Lift B out of service: “Seen”"));
    assert!(html.contains("Asked in front-desk: Which room was it: 103 or 113?"));
    assert!(html.contains("data-ws-undo=\"/workspace/hermes/actions/p-"), "Maya may undo what was done for her");

    let questions = workspace.hermes_page(&maya(), "questions", &Messages(vec![question.clone()])).await.unwrap();
    assert_eq!(questions.items.len(), 1);
    let comments = workspace.hermes_page(&maya(), "comments", &Messages(vec![question.clone()])).await.unwrap();
    assert!(comments.items.iter().all(|item| item.text.starts_with("Commented")) && comments.items.len() == 1);

    // Home lists it to confirm too, and the page asks for its state.
    let home = workspace.home(&maya(), &Messages(vec![question.clone()]), now()).await.unwrap();
    assert_eq!(home.proposals.len(), 1);
    let states = workspace.proposal_states(&[id.clone(), "zzz".into()]);
    assert_eq!(states[&id]["status"], "pending");
    assert!(states.get("zzz").is_none());
    assert_eq!(crate::hermes::parse_ids("b1,a2, bad id,A3,b1"), ["a2", "b1"]);

    // Hermes's Fizzy user unknown: the page says its direct actions can't be shown.
    let (_, unknown, _) = phase2(departments(), false).await;
    let html = askama::Template::render(&unknown.hermes_page(&maya(), "all", &Messages(Vec::new())).await.unwrap()).unwrap();
    assert!(html.contains("isn't known, so what it does directly in Fizzy isn't shown"), "{html}");
}

#[tokio::test]
async fn chips_of_cards_nothing_refreshes_are_read_again() {
    let fizzy = fizzy();
    let workspace = Workspace::new(config());
    workspace.poll(&fizzy, now()).await.unwrap();
    workspace.chips(&[77]);
    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(workspace.snapshot().card(77).is_some());
    let reads = |fizzy: &FakeFizzy| fizzy.paths().iter().filter(|path| *path == "/897/cards/77.json").count();
    fizzy.requests.lock().unwrap().clear();
    workspace.poll(&fizzy, now() + SignedDuration::from_mins(5)).await.unwrap();
    assert_eq!(reads(&fizzy), 0, "fresh enough");
    fizzy.reply("/897/cards/77.json", card(77, "Pool pump knocking", &[], Some("Done")));
    workspace.poll(&fizzy, now() + SignedDuration::from_mins(16)).await.unwrap();
    assert_eq!(reads(&fizzy), 1, "read again after 15 minutes");
    assert_eq!(workspace.snapshot().card(77).unwrap().column.as_ref().unwrap().name, "Done", "the chip shows its new column");
    fizzy.replies.lock().unwrap().remove("/897/cards/77.json");
    workspace.poll(&fizzy, now() + SignedDuration::from_mins(32)).await.unwrap();
    assert!(workspace.snapshot().card(77).is_none(), "deleted in Fizzy: gone");
}

#[tokio::test]
async fn what_campfire_ran_for_hermes_isn_t_logged_twice_as_direct() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    done(propose(&fizzy, &workspace, json!({"action": "comment", "card": 12, "body": "Lift company called"})).await.unwrap());
    // Fizzy's feed shows that comment as Hermes's (Campfire wrote it with Hermes's token).
    fizzy.reply(
        HERMES_FEED,
        json!([{"id": "c1", "action": "comment_created", "created_at": "2026-09-30T09:00:03Z", "url": "http://localhost:8484/897/cards/12",
            "eventable_type": "Comment", "eventable": {"id": "cm1", "body": {"plain_text": "Lift company called"}},
            "board": {"id": "b1", "name": "Incident Log"}, "creator": {"id": "fz-hermes", "name": "Hermes"}}]),
    );
    workspace.poll(&fizzy, now()).await.unwrap();
    let entries = workspace.hermes_log().entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].via, Via::Campfire);
}

fn live_report() -> SourceMessage {
    SourceMessage {
        id: 55,
        room_id: 3,
        creator_id: 5,
        creator_is_bot: false,
        created_at: now() - SignedDuration::from_mins(2),
        body_html: format!("<p>{LIVE_REPORT_OPENING}.</p><p>Chute dans le hall</p>"),
    }
}

#[tokio::test]
async fn a_live_report_being_filed_counts_as_used() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    // A card from this report is being filed right now (Running).
    let create = json!({"action": "create", "title": "Chute — hall"});
    let first = workspace.propose(&fizzy, &hermes_bot(), &create, for_maya(), Some(live_report())).await.unwrap();
    let first = done(first);
    workspace.proposals().update(now(), |items| {
        items.iter_mut().find(|p| p.id == first.id).unwrap().status = Status::Running;
    });
    let second = json!({"action": "create", "title": "Chute (2)"});
    let second = workspace.propose(&fizzy, &hermes_bot(), &second, for_maya(), Some(live_report())).await.unwrap();
    assert!(matches!(second, Proposed::Pending { .. }), "a running card from the same report counts");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_proposals_from_one_live_report_file_one_card() {
    let (fizzy, workspace, _) = phase2(departments(), true).await;
    let (fizzy, workspace) = (std::sync::Arc::new(fizzy), std::sync::Arc::new(workspace));
    let tasks: Vec<_> = (0..8)
        .map(|n| {
            let (fizzy, workspace) = (fizzy.clone(), workspace.clone());
            tokio::spawn(async move {
                let body = json!({"action": "create", "title": format!("Chute — hall ({n})")});
                workspace.propose(&*fizzy, &hermes_bot(), &body, for_maya(), Some(live_report())).await.unwrap()
            })
        })
        .collect();
    let mut filed = 0;
    for task in tasks {
        if matches!(task.await.unwrap(), Proposed::Done { .. }) {
            filed += 1;
        }
    }
    assert_eq!(filed, 1, "one card per confirmed live report, the others ask first");
    assert_eq!(workspace.pending_proposals().len(), 7);
}

#[tokio::test]
async fn proposals_details_are_for_their_room_and_the_duty_managers() {
    let settings = Settings { duty_managers: Some(vec![1]), ..departments() };
    let (fizzy, workspace, _) = phase2(settings, true).await;
    let in_room = pending_id(propose(&fizzy, &workspace, json!({"action": "create", "title": "Guest fell — Room 12"})).await.unwrap());
    workspace.decide(&fizzy, &manager(), &in_room, Decision::Dismiss).await.unwrap();
    let pending = pending_id(propose(&fizzy, &workspace, json!({"action": "create", "title": "Leak — Room 3"})).await.unwrap());
    // No context (the bridge couldn't say which call it was): no room, no person.
    let roomless = json!({"action": "create", "title": "Roomless"});
    let roomless = pending_id(workspace.propose(&fizzy, &hermes_bot(), &roomless, Context::default(), None).await.unwrap());
    let in_front_desk = |id: i64| ChatMessage {
        id,
        room_id: 3,
        room_name: "front-desk".into(),
        url: format!("/rooms/3/@{id}"),
        creator_id: 5,
        creator_name: "Maya".into(),
        creator_is_bot: false,
        created_at: now(),
        body_html: "<p>hi</p>".into(),
    };
    let text = |page: &crate::hermes::HermesPage| askama::Template::render(page).unwrap();

    // Maya is in room 3: its proposals, not the roomless one.
    let page = workspace.hermes_page(&maya(), "all", &Messages(vec![in_front_desk(1)])).await.unwrap();
    assert_eq!(page.pending.iter().map(|item| item.id.clone()).collect::<Vec<_>>(), [pending]);
    let html = text(&page);
    assert!(html.contains("Dismissed: Create a card: Guest fell — Room 12") && !html.contains("Roomless"), "{html}");

    // Karim isn't in room 3 and isn't a duty manager: nothing of them, on the tab or Home.
    let page = workspace.hermes_page(&karim(), "all", &Messages(Vec::new())).await.unwrap();
    assert!(page.pending.is_empty());
    let html = text(&page);
    assert!(!html.contains("Guest fell") && !html.contains("Leak") && !html.contains("Roomless"), "{html}");
    assert!(workspace.home(&karim(), &Messages(Vec::new()), now()).await.unwrap().proposals.is_empty());
    assert!(!workspace.sees_proposal(&karim(), Some(3), &[]) && workspace.sees_proposal(&karim(), Some(3), &[3]));

    // A duty manager sees them all, the roomless one too.
    let page = workspace.hermes_page(&manager(), "all", &Messages(Vec::new())).await.unwrap();
    assert_eq!(page.pending.len(), 2);
    assert!(page.pending.iter().any(|item| item.id == roomless));
}

#[tokio::test]
async fn only_hermes_s_bot_may_propose() {
    let (_, workspace, _) = phase2(departments(), true).await;
    assert_eq!(workspace.hermes_bot_id(None), Some(9), "the only active bot");
    let other = Bot { id: 12, name: "Deploy bot".into(), sgid: "sg2".into() };
    workspace.set_bots(vec![hermes_bot(), other]);
    assert_eq!(workspace.hermes_bot_id(None), None, "several bots and nothing says which: refused");
    assert_eq!(workspace.hermes_bot_id(Some("Hermes")), Some(9), "the live voice report's bot");
    assert_eq!(workspace.hermes_bot_id(Some("12")), Some(12));
    assert_eq!(workspace.hermes_bot_id(Some("Nobody")), None);
    let configured = Workspace::new(config_with(&[("HERMES_BOT", "9")]));
    configured.set_bots(vec![Bot { id: 12, name: "Deploy bot".into(), sgid: "sg2".into() }, hermes_bot()]);
    assert_eq!(configured.hermes_bot_id(Some("Deploy bot")), Some(9), "HERMES_BOT first");
}

#[tokio::test]
async fn undo_reads_the_feed_back_to_the_action_or_refuses() {
    let (fizzy, workspace, clock) = phase2(all_alone(), true).await;
    let severity = done(propose(&fizzy, &workspace, json!({"action": "severity", "card": 12, "severity": "high"})).await.unwrap());
    let id = format!("p-{}-done", severity.id);
    *clock.lock().unwrap() = now() + SignedDuration::from_hours(1);
    let page = |at: &str, card: u64, who: &str| {
        json!([{"id": format!("x{at}{card}"), "action": "comment_created", "created_at": at,
            "eventable_type": "Card", "eventable": {"number": card}, "creator": {"id": format!("fz-{who}"), "name": who}}])
    };
    let next = |page: u32| Some(format!("<http://localhost:8484{BOARD_FEED}&page={page}>; rel=\"next\""));
    let set = |page_number: u32, body: Value, link: Option<String>| {
        let path = if page_number == 1 { BOARD_FEED.to_string() } else { format!("{BOARD_FEED}&page={page_number}") };
        fizzy.replies.lock().unwrap().insert(path, (200, body, link));
    };
    // Ten busy pages about other cards, none reaching the action: too much to check.
    for n in 1..=10 {
        set(n, page("2026-09-30T09:30:00Z", 40 + n as u64, "Karim"), next(n + 1));
    }
    let busy = workspace.undo(&fizzy, &manager(), &id).await.unwrap_err();
    assert_eq!(busy.code(), "cannot_undo");
    assert!(busy.message().contains("too much activity"), "{busy}");
    // The change on card 12 is on page 2: found.
    set(2, page("2026-09-30T09:20:00Z", 12, "Karim"), next(3));
    let touched = workspace.undo(&fizzy, &manager(), &id).await.unwrap_err();
    assert_eq!(touched.code(), "changed_since");
    // Page 2 reaches back before the action, nothing on card 12 since: undone.
    set(2, page("2026-09-30T08:00:00Z", 12, "Karim"), next(3));
    fizzy.requests.lock().unwrap().clear();
    workspace.undo(&fizzy, &manager(), &id).await.unwrap();
    assert!(!fizzy.paths().iter().any(|path| path.ends_with("&page=3")), "no further than needed");
}

mod phase2b;
