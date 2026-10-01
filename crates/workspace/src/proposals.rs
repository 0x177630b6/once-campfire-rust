//! Hermes's proposals (phase 2.3): Hermes asks Campfire to act on the incident board instead of
//! writing to Fizzy itself. Its skill calls a `propose` helper, which POSTs to the Hermes bridge
//! (`/propose/<secret>`), which adds what it knows of the conversation (room, person, message) and
//! forwards it with the bot key to `POST /hermes/:bot_key/workspace/proposals`. Hermes never holds
//! the bot key.
//!
//! Campfire parses a proposal exactly like a person's request ([`Change::parse`],
//! [`NewCard::parse`]: Hermes can't ask for anything a person couldn't), then applies the dial of
//! its kind ([`crate::settings::Autonomy`]):
//!
//! - **Alone**: run at once through the [`Writer`](crate::writes::Writer) as Hermes (its own token,
//!   `HERMES_FIZZY_TOKEN`, when set), with the card's state before, for undo;
//! - **Ask first**: kept in `<CAMPFIRE_STORAGE_PATH>/hermes/proposals.json` and posted in the room
//!   as a draft carrying a structured marker (`title="hermes-proposal:<id>"`), whose File /
//!   Dismiss buttons decide it, per the confirm policy; nobody answering within 24 h dismisses it
//!   ("timed out");
//! - **Never**: refused, and Hermes says so.
//!
//! A live voice report the reporter confirmed on the voice page (the bridge names its message) counts
//! as confirmed: filed at once unless the dial says Never.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use jiff::{SignedDuration, Timestamp};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actions::{Change, NewCard, Target};
use crate::cache::Snapshot;
use crate::html::{self, escape};
use crate::settings::{ActionKind, Settings};
use crate::writes::ActionError;

/// A proposal nobody answered is dismissed after this.
pub const PROPOSAL_TTL: SignedDuration = SignedDuration::from_hours(24);
/// Decided proposals are forgotten after this (the Hermes log keeps what happened).
const KEEP_DECIDED: SignedDuration = SignedDuration::from_hours(7 * 24);
const MAX_KEPT: usize = 500;
/// A live voice report counts as confirmed for this long after it was posted.
pub const LIVE_REPORT_WINDOW: SignedDuration = SignedDuration::from_hours(2);
/// What the voice page posts under the reporter's name once they confirmed the recap
/// (`controllers/voice.rs`): a fixed English marker, whatever language the ticket is in.
pub const LIVE_REPORT_OPENING: &str = "Live voice ticket, confirmed by the reporter";
/// The opening images up to v0.1.2-hermes.16 posted (French only), still recognized so a report
/// posted just before an upgrade is still filed at once.
pub const LEGACY_LIVE_REPORT_OPENINGS: [&str; 1] = ["Compte rendu d’incident dicté en direct (voix), confirmé par l’auteur"];
const MAX_SUMMARY_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    /// Confirmed, being run.
    Running,
    Done,
    Dismissed,
    /// Nobody answered in 24 h.
    Expired,
    /// Hermes proposed something else instead (after "change: …").
    Superseded,
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Running => "running",
            Status::Done => "done",
            Status::Dismissed => "dismissed",
            Status::Expired => "expired",
            Status::Superseded => "superseded",
            Status::Failed => "failed",
        }
    }
}

/// What the bridge knew of the conversation, as Campfire checked it: the room (the bot is a
/// member), the person (an active user), their message (in that room, theirs). The app drops what
/// doesn't check out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// `chat`, `voice_note`, `fizzy_comment`, `live_voice_question`.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub room_id: Option<i64>,
    #[serde(default)]
    pub room_name: Option<String>,
    /// The person it's for (who asked Hermes).
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub message_id: Option<i64>,
    /// The message is a live voice report its author confirmed ([`LIVE_REPORT_OPENING`]).
    #[serde(default)]
    pub live_report: bool,
}

impl Context {
    /// "voice note", "live voice report"… for the log.
    pub fn source_label(&self) -> Option<String> {
        if self.live_report {
            return Some("live voice report".into());
        }
        Some(
            match self.source.as_deref()? {
                "chat" => "chat",
                "voice_note" => "voice note",
                "fizzy_comment" => "Fizzy comment",
                "live_voice_question" => "live voice question",
                _ => return None,
            }
            .into(),
        )
    }
}

/// The message the bridge said Hermes was answering, as the app found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMessage {
    pub id: i64,
    pub room_id: i64,
    pub creator_id: i64,
    pub creator_is_bot: bool,
    pub created_at: Timestamp,
    pub body_html: String,
}

/// Whether `message` is a live voice report its author confirmed, posted for `user_id` recently.
pub fn is_confirmed_live_report(message: &SourceMessage, user_id: Option<i64>, now: Timestamp) -> bool {
    let text = html::to_text(&message.body_html);
    !message.creator_is_bot
        && Some(message.creator_id) == user_id
        && now.duration_since(message.created_at) <= LIVE_REPORT_WINDOW
        && std::iter::once(LIVE_REPORT_OPENING).chain(LEGACY_LIVE_REPORT_OPENINGS).any(|opening| text.trim_start().starts_with(opening))
}

/// A proposal, from the moment Hermes made it to its outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// Lowercase letters and digits.
    pub id: String,
    pub created_at: Timestamp,
    pub status: Status,
    /// `create`, `move`, `close`, `severity`, `departments`, `step`, `comment`.
    pub action: String,
    #[serde(default)]
    pub card: Option<u64>,
    /// The request as parsed, parsed again (against the settings of the day) when it runs.
    pub request: Value,
    /// "Create a card: Lost property — laptop bag".
    pub summary: String,
    /// The draft's lines ("Severity: low", "Department: Front desk", the details).
    #[serde(default)]
    pub details: Vec<String>,
    #[serde(default)]
    pub context: Context,
    /// The bot that proposed it.
    pub bot_id: i64,
    pub bot_name: String,
    #[serde(default)]
    pub draft_message_id: Option<i64>,
    #[serde(default)]
    pub decided_at: Option<Timestamp>,
    #[serde(default)]
    pub decided_by_id: Option<i64>,
    #[serde(default)]
    pub decided_by_name: Option<String>,
    #[serde(default)]
    pub result_card: Option<u64>,
    #[serde(default)]
    pub result_url: Option<String>,
    /// What went wrong, or a warning.
    #[serde(default)]
    pub message: Option<String>,
    /// A new card's departments (tags), as its request resolved them when proposed (`department`,
    /// `departments`, and `tags` naming a department): who may see the proposal (phase 2.7).
    /// `None` for a change to a card (its card's tags decide), and for a proposal stored before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departments: Option<Vec<String>>,
}

impl Proposal {
    /// The tags that decide who may see a proposed new card: the departments stored when it was
    /// proposed, or (a proposal stored before they were) read again from its request.
    pub fn new_card_tags(&self) -> Vec<String> {
        self.departments.clone().unwrap_or_else(|| crate::visibility::proposed_tags(&self.request))
    }

    pub fn expires_at(&self) -> Timestamp {
        self.created_at + PROPOSAL_TTL
    }

    /// Where its buttons POST `{"decision": "confirm" | "dismiss"}`.
    pub fn decision_path(&self) -> String {
        decision_path(&self.id)
    }

    /// "Filed by Karim", "Dismissed (timed out)"…
    pub fn state_label(&self) -> String {
        let by = self.decided_by_name.as_deref().map(|name| format!(" by {name}")).unwrap_or_default();
        match self.status {
            Status::Pending => "Waiting for a confirmation".into(),
            Status::Running => "Being filed…".into(),
            Status::Done if self.decided_by_name.is_some() => format!("Confirmed{by}"),
            Status::Done => "Done (Sky may do this alone)".into(),
            Status::Dismissed => format!("Dismissed{by}"),
            Status::Expired => "Dismissed (timed out)".into(),
            Status::Superseded => "Replaced by a newer proposal".into(),
            Status::Failed => format!("Failed{}", self.message.as_deref().map(|m| format!(": {m}")).unwrap_or_default()),
        }
    }
}

pub fn decision_path(id: &str) -> String {
    format!("/workspace/hermes/proposals/{id}/decision")
}

/// What Hermes asked for, once parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Create(NewCard),
    Change { card: u64, change: Change },
}

impl Request {
    pub fn kind(&self) -> ActionKind {
        match self {
            Request::Create(_) => ActionKind::Create,
            Request::Change { change, .. } => match change {
                Change::Comment(_) => ActionKind::Comment,
                Change::Severity(_) | Change::Departments(_) => ActionKind::Tag,
                Change::Step { .. } => ActionKind::Step,
                Change::Move(Target::Closed) => ActionKind::Close,
                Change::Move(_) => ActionKind::Move,
            },
        }
    }

    pub fn card(&self) -> Option<u64> {
        match self {
            Request::Create(_) => None,
            Request::Change { card, .. } => Some(*card),
        }
    }
}

/// Parses a proposal's body: `{"action": …, …}` with the fields of the matching person's request
/// (`POST /workspace/cards` for `create`, `POST /workspace/cards/:n/<change>` for the others, plus
/// `"card": n`), `close` being `move` to `closed`, and `move` also taking a column by name
/// (`"column": "In progress"`). Returns the request, its action name and the body to keep.
pub fn parse(body: &Value, settings: &Settings, snapshot: &Snapshot) -> Result<(Request, String, Value), ActionError> {
    let Some(object) = body.as_object() else {
        return Err(ActionError::invalid("bad_request", "A proposal is a JSON object."));
    };
    let action = object.get("action").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    let mut kept: serde_json::Map<String, Value> = object.clone();
    kept.remove("context");
    kept.remove("replaces");
    kept.remove("summary");
    if action == "create" {
        let new = NewCard::parse(body, settings)?;
        return Ok((Request::Create(new), action, Value::Object(kept)));
    }
    let card = match object.get("card") {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(number)) => number.trim().trim_start_matches('#').parse().ok(),
        _ => None,
    }
    .filter(|number| *number > 0)
    .ok_or_else(|| ActionError::invalid("invalid_card", "Which card? (\"card\": its number)"))?;
    let (kind, params) = match action.as_str() {
        "close" => ("move", json!({ "to": "closed" })),
        "move" => {
            let column = object.get("column").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty());
            match column {
                Some(name) => {
                    let found = snapshot.columns.iter().find(|column| column.name.trim().eq_ignore_ascii_case(name) || column.id == name);
                    let Some(column) = found else {
                        return Err(ActionError::invalid("unknown_column", format!("There's no column “{name}” on the incident board.")));
                    };
                    kept.insert("to".into(), json!(format!("column:{}", column.id)));
                    kept.remove("column");
                    ("move", json!({ "to": format!("column:{}", column.id) }))
                }
                None => ("move", body.clone()),
            }
        }
        "severity" | "departments" | "step" | "comment" => (action.as_str(), body.clone()),
        "" => return Err(ActionError::invalid("unknown_action", "Say what to do (\"action\").")),
        other => {
            return Err(ActionError::invalid(
                "unknown_action",
                format!("Meshduty doesn't do “{other}” for Sky (create, move, close, severity, departments, step, comment)."),
            ));
        }
    };
    let change =
        Change::parse(kind, &params, settings).unwrap_or_else(|| Err(ActionError::invalid("unknown_action", "Unknown action.")))?;
    kept.insert("card".into(), json!(card));
    Ok((Request::Change { card, change }, action, Value::Object(kept)))
}

/// The one-line summary and the draft's lines.
pub fn describe(request: &Request, settings: &Settings, snapshot: &Snapshot) -> (String, Vec<String>) {
    let card_label = |number: u64| match snapshot.card(number) {
        Some(card) if !card.title.trim().is_empty() => format!("#{number} {}", html::truncate(card.title.trim(), 80)),
        _ => format!("#{number}"),
    };
    let department_names = |tags: &[String]| {
        let names: Vec<String> =
            tags.iter().map(|tag| settings.department_by_tag(tag).map(|d| d.name.clone()).unwrap_or_else(|| tag.clone())).collect();
        if names.is_empty() { "none".to_string() } else { names.join(", ") }
    };
    let (summary, details) = match request {
        Request::Create(new) => {
            let mut details = vec![
                format!("Severity: {}", new.severity.map(|severity| severity.as_str()).unwrap_or("not set")),
                format!("Department: {}", department_names(&new.departments)),
            ];
            if !new.tags.is_empty() {
                details.push(format!("Tags: {}", new.tags.join(", ")));
            }
            if !new.steps.is_empty() {
                details.push(format!("Steps: {}", new.steps.join(" · ")));
            }
            if !new.description.is_empty() {
                details.push(html::truncate(&new.description, 1200));
            }
            (format!("Create a card: {}", new.title), details)
        }
        Request::Change { card, change } => {
            let what = match change {
                Change::Move(Target::Closed) => format!("Close {}", card_label(*card)),
                Change::Move(Target::New) => format!("Move {} back to New", card_label(*card)),
                Change::Move(Target::NotNow) => format!("Move {} to Monitoring", card_label(*card)),
                Change::Move(Target::Column(id)) => {
                    let name = snapshot.columns.iter().find(|column| &column.id == id).map(|column| column.name.clone());
                    format!("Move {} to {}", card_label(*card), name.unwrap_or_else(|| "a column".into()))
                }
                Change::Severity(Some(severity)) => format!("Set the severity of {} to {}", card_label(*card), severity.as_str()),
                Change::Severity(None) => format!("Remove the severity of {}", card_label(*card)),
                Change::Departments(tags) => format!("Set the departments of {} to {}", card_label(*card), department_names(tags)),
                Change::Step { completed: true, .. } => format!("Tick a step of {}", card_label(*card)),
                Change::Step { completed: false, .. } => format!("Untick a step of {}", card_label(*card)),
                Change::Comment(_) => format!("Comment on {}", card_label(*card)),
            };
            let details = match change {
                Change::Comment(text) => vec![html::truncate(text, 1200)],
                _ => Vec::new(),
            };
            (what, details)
        }
    };
    (html::truncate(&summary, MAX_SUMMARY_CHARS), details)
}

/// The draft Campfire posts in the room as the bot for an "Ask first" proposal. Its marker (the
/// link's `title`, which Action Text keeps) is what gives it File / Dismiss buttons that decide this
/// proposal ([`marker_in`]).
pub fn draft_html(proposal: &Proposal, extra_note: Option<&str>) -> String {
    let mut html = format!("<p><strong>Sky proposes:</strong> {}</p>", escape(&proposal.summary));
    for detail in &proposal.details {
        let lines: Vec<String> = detail.lines().map(escape).collect();
        html.push_str(&format!("<p>{}</p>", lines.join("<br>")));
    }
    if let Some(note) = extra_note {
        html.push_str(&format!("<p>{}</p>", escape(note)));
    }
    let who = proposal.context.user_name.as_deref().map(|name| format!(" for {}", escape(name))).unwrap_or_default();
    html.push_str(&format!(
        r#"<p>Waiting for a confirmation{who} (24 hours). <a href="/workspace/hermes#proposal-{id}" title="hermes-proposal:{id}">Proposal {id}</a></p>"#,
        id = escape(&proposal.id),
    ));
    html
}

/// The proposal a bot's draft is for (its marker), if it is one.
pub fn marker_in(body_html: &str) -> Option<String> {
    static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"title="hermes-proposal:([a-z0-9]{1,40})""#).unwrap());
    MARKER.captures(body_html).map(|caps| caps[1].to_string())
}

/// A proposal id: the time, then a counter (lowercase, sortable enough, unique in one process).
pub fn new_id(now: Timestamp) -> String {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1296;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{}{:0>2}{:0>3}", base36(now.as_second().max(0) as u64), base36(counter as u64), base36((nanos % 46656) as u64))
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// `proposals.json`: pending proposals and those decided in the last week.
#[derive(Debug)]
pub struct ProposalStore {
    path: PathBuf,
    items: Mutex<Vec<Proposal>>,
    error: Mutex<Option<String>>,
}

#[derive(Serialize, Deserialize)]
struct File {
    #[serde(default)]
    proposals: Vec<Proposal>,
}

impl ProposalStore {
    /// Reads the file; a missing one is empty, and one that can't be read is empty too (with the
    /// error, which the app logs): its pending proposals are lost, nothing runs by itself.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (items, error) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<File>(&bytes) {
                Ok(file) => (file.proposals, None),
                Err(error) => (Vec::new(), Some(format!("{} isn't valid: {error}", path.display()))),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(error) => (Vec::new(), Some(format!("couldn't read {}: {error}", path.display()))),
        };
        // A proposal that was running when the server stopped didn't finish: say so.
        let items = items
            .into_iter()
            .map(|mut proposal| {
                if proposal.status == Status::Running {
                    proposal.status = Status::Failed;
                    proposal.message = Some("the server restarted while it was being filed; check the card".into());
                }
                proposal
            })
            .collect();
        Self { path, items: Mutex::new(items), error: Mutex::new(error) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self, id: &str) -> Option<Proposal> {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|proposal| proposal.id == id).cloned()
    }

    /// Newest first.
    pub fn all(&self) -> Vec<Proposal> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner()).clone();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        items
    }

    /// Changes the store under its lock and saves it; `change` returns what to hand back.
    pub fn update<T>(&self, now: Timestamp, change: impl FnOnce(&mut Vec<Proposal>) -> T) -> T {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let result = change(&mut items);
        items.retain(|proposal| {
            matches!(proposal.status, Status::Pending | Status::Running)
                || now.duration_since(proposal.decided_at.unwrap_or(proposal.created_at)) <= KEEP_DECIDED
        });
        evict(&mut items);
        if let Err(error) = crate::store::write_json_atomically(&self.path, &File { proposals: items.clone() }) {
            *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("couldn't write {}: {error}", self.path.display()));
        }
        result
    }

    /// The last read or write error, once (the app logs it).
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Past [`MAX_KEPT`], forgets the decided proposals decided longest ago; never a pending or running
/// one (the Hermes log keeps what happened to the others).
fn evict(items: &mut Vec<Proposal>) {
    let open = |proposal: &Proposal| matches!(proposal.status, Status::Pending | Status::Running);
    let excess = items.len().saturating_sub(MAX_KEPT);
    if excess == 0 {
        return;
    }
    let mut decided: Vec<(Timestamp, String)> =
        items.iter().filter(|p| !open(p)).map(|p| (p.decided_at.unwrap_or(p.created_at), p.id.clone())).collect();
    decided.sort();
    let dropped: std::collections::HashSet<String> = decided.into_iter().take(excess).map(|(_, id)| id).collect();
    items.retain(|proposal| open(proposal) || !dropped.contains(&proposal.id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fizzy::{Column, Severity};
    use crate::settings::Department;

    fn settings() -> Settings {
        Settings {
            departments: vec![Department { name: "Front desk".into(), tag: "front-desk".into(), rooms: vec![3], restricted: false }],
            ..Settings::default()
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot { columns: vec![Column { id: "c1".into(), name: "In progress".into(), color: None }], ..Snapshot::default() }
    }

    #[test]
    fn proposals_parse_like_a_person_s_request() {
        let (settings, snapshot) = (settings(), snapshot());
        let (request, action, kept) = parse(
            &json!({"action": "create", "title": "Lost property — laptop bag", "severity": "low", "department": "front-desk",
                    "tags": ["Incident", "front-desk"], "steps": ["Call the guest", " "], "context": {"user_id": 1}, "summary": "x"}),
            &settings,
            &snapshot,
        )
        .unwrap();
        assert_eq!(action, "create");
        let Request::Create(new) = &request else { panic!("{request:?}") };
        assert_eq!(
            (new.severity, new.departments.as_slice(), new.tags.as_slice()),
            (Some(Severity::Low), ["front-desk".to_string()].as_slice(), ["incident".to_string()].as_slice())
        );
        assert_eq!(new.steps, ["Call the guest"]);
        assert!(kept.get("context").is_none() && kept.get("summary").is_none(), "the context isn't part of the request");
        assert_eq!(request.kind(), ActionKind::Create);

        let (close, ..) = parse(&json!({"action": "close", "card": "#13"}), &settings, &snapshot).unwrap();
        assert_eq!(close, Request::Change { card: 13, change: Change::Move(Target::Closed) });
        assert_eq!(close.kind(), ActionKind::Close);
        let (moved, _, kept) = parse(&json!({"action": "move", "card": 13, "column": "in progress"}), &settings, &snapshot).unwrap();
        assert_eq!(moved, Request::Change { card: 13, change: Change::Move(Target::Column("c1".into())) });
        assert_eq!(kept["to"], "column:c1", "kept resolved, so it runs the same later");
        assert_eq!(moved.kind(), ActionKind::Move);
        let (tagged, ..) = parse(&json!({"action": "severity", "card": 13, "severity": "critical"}), &settings, &snapshot).unwrap();
        assert_eq!(tagged.kind(), ActionKind::Tag);
    }

    #[test]
    fn what_a_person_couldn_t_ask_is_refused() {
        let (settings, snapshot) = (settings(), snapshot());
        for (body, code) in [
            (json!({"action": "delete", "card": 13}), "unknown_action"),
            (json!({"action": "assign", "card": 13}), "unknown_action"),
            (json!({"card": 13}), "unknown_action"),
            (json!({"action": "close"}), "invalid_card"),
            (json!({"action": "move", "card": 13, "column": "Nowhere"}), "unknown_column"),
            (json!({"action": "move", "card": 13, "to": "sideways"}), "invalid_target"),
            (json!({"action": "departments", "card": 13, "tags": ["kitchen"]}), "unknown_department"),
            (json!({"action": "create", "title": " "}), "blank_title"),
            (json!({"action": "create", "title": "x", "tags": ["sev-high"]}), "invalid_tag"),
            (json!({"action": "create", "title": "x", "tags": ["a b"]}), "invalid_tag"),
            (json!({"action": "create", "title": "x", "steps": [1]}), "invalid_steps"),
            (json!({"action": "comment", "card": 13, "body": ""}), "blank_comment"),
            (json!(["create"]), "bad_request"),
        ] {
            let error = parse(&body, &settings, &snapshot).unwrap_err();
            assert_eq!((error.status(), error.code()), (422, code), "{body}");
        }
    }

    #[test]
    fn the_draft_carries_its_marker_and_escapes() {
        let (settings, snapshot) = (settings(), snapshot());
        let (request, action, kept) =
            parse(&json!({"action": "create", "title": "Leak <b>", "description": "Water\nunder <door>"}), &settings, &snapshot).unwrap();
        let (summary, details) = describe(&request, &settings, &snapshot);
        let proposal = Proposal {
            id: "abc123".into(),
            created_at: "2026-09-30T09:00:00Z".parse().unwrap(),
            status: Status::Pending,
            action,
            card: None,
            request: kept,
            summary,
            details,
            context: Context { user_name: Some("Maya".into()), ..Context::default() },
            bot_id: 9,
            bot_name: "Hermes".into(),
            draft_message_id: None,
            decided_at: None,
            decided_by_id: None,
            decided_by_name: None,
            result_card: None,
            result_url: None,
            message: None,
            departments: None,
        };
        let html = draft_html(&proposal, None);
        assert!(html.starts_with("<p><strong>Sky proposes:</strong> Create a card: Leak &lt;b&gt;</p>"), "{html}");
        assert!(html.contains("<p>Water<br>under &lt;door&gt;</p>"));
        assert!(html.contains("for Maya"));
        assert_eq!(marker_in(&html).as_deref(), Some("abc123"));
        assert_eq!(marker_in("<p>Reply confirm to file it</p>"), None);
        assert_eq!(proposal.expires_at().to_string(), "2026-10-01T09:00:00Z");
    }

    #[test]
    fn live_reports_are_recognized() {
        let now: Timestamp = "2026-09-30T09:00:00Z".parse().unwrap();
        let message = SourceMessage {
            id: 55,
            room_id: 3,
            creator_id: 5,
            creator_is_bot: false,
            created_at: now - SignedDuration::from_mins(3),
            body_html: format!(
                r#"<p><action-text-attachment sgid="x" content-type="application/vnd.campfire.mention"></action-text-attachment> {LIVE_REPORT_OPENING}.</p><p>Chute dans le hall</p>"#
            ),
        };
        assert!(is_confirmed_live_report(&message, Some(5), now));
        assert!(!is_confirmed_live_report(&message, Some(6), now), "someone else's");
        assert!(!is_confirmed_live_report(&message, Some(5), now + SignedDuration::from_hours(3)), "too old");
        let typed = SourceMessage { body_html: "<p>@Hermes please file it</p>".into(), ..message.clone() };
        assert!(!is_confirmed_live_report(&typed, Some(5), now));
        let quoted = SourceMessage { body_html: format!("<p>He said: {LIVE_REPORT_OPENING}</p>"), ..message.clone() };
        assert!(!is_confirmed_live_report(&quoted, Some(5), now), "only at the start");
        // What images up to v0.1.2-hermes.16 posted, in French.
        let legacy = SourceMessage {
            body_html: "<p>Compte rendu d’incident dicté en direct (voix), confirmé par l’auteur.</p><p>Chute</p>".into(),
            ..message
        };
        assert!(is_confirmed_live_report(&legacy, Some(5), now));
    }

    #[test]
    fn eviction_drops_decided_proposals_first_never_pending_ones() {
        let at: Timestamp = "2026-09-30T09:00:00Z".parse().unwrap();
        let proposal = |id: String, status: Status, minutes: i64| Proposal {
            id,
            created_at: at + SignedDuration::from_mins(minutes),
            status,
            action: "close".into(),
            card: Some(12),
            request: json!({"action": "close", "card": 12}),
            summary: "Close #12".into(),
            details: Vec::new(),
            context: Context::default(),
            bot_id: 9,
            bot_name: "Hermes".into(),
            draft_message_id: None,
            decided_at: (status != Status::Pending).then(|| at + SignedDuration::from_mins(minutes)),
            decided_by_id: None,
            decided_by_name: None,
            result_card: None,
            result_url: None,
            message: None,
            departments: None,
        };
        // The oldest are pending; decided ones fill the rest, and one more.
        let mut items: Vec<Proposal> = (0..3).map(|n| proposal(format!("pending{n}"), Status::Pending, n)).collect();
        items.extend((0..MAX_KEPT - 1).map(|n| proposal(format!("done{n}"), Status::Done, 10 + n as i64)));
        evict(&mut items);
        assert_eq!(items.len(), MAX_KEPT);
        assert!((0..3).all(|n| items.iter().any(|p| p.id == format!("pending{n}"))), "pending ones are never dropped");
        assert!(!items.iter().any(|p| p.id == "done0" || p.id == "done1"), "the oldest decided ones go");
        assert!(items.iter().any(|p| p.id == "done2"));
    }

    #[test]
    fn ids_are_short_and_unique() {
        let now: Timestamp = "2026-09-30T09:00:00Z".parse().unwrap();
        let (a, b) = (new_id(now), new_id(now));
        assert_ne!(a, b);
        assert!(a.len() <= 40 && a.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "{a}");
    }
}
