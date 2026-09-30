//! The Hermes log (phase 2.2): what Hermes did to the incident board, kept 90 days in
//! `<CAMPFIRE_STORAGE_PATH>/hermes/hermes-log.jsonl` (one JSON line per entry, deduplicated by id,
//! so it survives restarts; entries older than [`RETENTION`] are dropped when the file is
//! compacted, at boot and once a day).
//!
//! Two sources, marked in every entry ([`Via`]):
//!
//! - **direct**: what Hermes did in Fizzy with its own token (its `fizzy` CLI), read from Fizzy's
//!   activity feed (`/activities?creator_ids[]=<Hermes's Fizzy user>&board_ids[]=<incident board>`).
//!   Fizzy's feed has no event for tags or steps, so Hermes's direct tag and step changes aren't
//!   there, and it only gives the new column of a move. Writes Campfire made itself (found in the
//!   durable write log, [`crate::journal`]) are left out: they're logged as "via Campfire", or are a
//!   person's own workspace actions when the workspace writes with Hermes's token.
//! - **via Campfire**: what Hermes proposed through Campfire ([`crate::proposals`]) and what came of
//!   it (run alone, confirmed, dismissed, expired, refused, failed), with the card's state before
//!   the change, which is what makes [`Reverse`] (undo) possible.
//!
//! Undo itself is an entry too ([`Kind::Undone`], naming its target).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use jiff::{SignedDuration, Timestamp};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::fizzy::{Activity, Card};
use crate::html;
use crate::journal;

/// Entries older than this leave the log.
pub const RETENTION: SignedDuration = SignedDuration::from_hours(90 * 24);
/// How long an action can be undone.
pub const UNDO_WINDOW: SignedDuration = SignedDuration::from_hours(24);
/// A write in the durable log and an activity in Fizzy's feed within this are the same change.
const SAME_CHANGE: SignedDuration = SignedDuration::from_secs(120);
/// The most entries a page shows.
pub const MAX_SHOWN: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// Hermes's own token, seen in Fizzy's activity feed.
    Direct,
    /// Proposed through Campfire, run by Campfire.
    Campfire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Created,
    Tagged,
    Moved,
    Closed,
    Reopened,
    Commented,
    Renamed,
    Assigned,
    Step,
    /// Waiting for someone to confirm (Ask first).
    Proposed,
    Dismissed,
    /// Nobody answered within 24 h.
    Expired,
    /// Replaced by a newer proposal from Hermes.
    Superseded,
    /// The dial says Never.
    Refused,
    Failed,
    Undone,
    /// Anything else Fizzy reports.
    Other,
}

impl Kind {
    /// The filter it's under on the Hermes tab.
    pub fn filter(self) -> &'static str {
        match self {
            Kind::Created => "created",
            Kind::Tagged => "tags",
            Kind::Moved | Kind::Closed | Kind::Reopened => "moves",
            Kind::Commented => "comments",
            Kind::Failed | Kind::Refused => "failures",
            _ => "other",
        }
    }
}

/// How to take an action back (the card is the entry's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reverse {
    /// A card Hermes created: close it, with a comment saying why. Never deleted.
    CloseCreated,
    /// The card's tags among `managed` go back to `before` (they're `after` now).
    Tags { managed: Vec<String>, before: Vec<String>, after: Vec<String> },
    /// Back where it was: `new`, `column:<id>`, `not_now` or `closed` (it's `after` now).
    Place { before: String, after: String },
    /// The step back to `before`.
    Step { id: String, before: bool },
    /// Delete the comment, with the token that wrote it (`hermes` or `workspace`): Fizzy lets
    /// only a comment's creator delete it.
    DeleteComment { comment_id: String, identity: String },
    /// The title back to `before` (it's `after` now).
    Title { before: String, after: String },
    /// Hermes closed it: reopen it (Fizzy puts it back in its column).
    Reopen,
    /// Hermes reopened it: close it again.
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `a-<Fizzy activity id>`, `p-<proposal id>-<event>`, `u-<undone entry id>`.
    pub id: String,
    pub at: Timestamp,
    pub kind: Kind,
    pub via: Via,
    #[serde(default)]
    pub card: Option<u64>,
    /// "Created #13 Guest slip in lobby".
    pub text: String,
    /// Who it was for: a Campfire user when Campfire knows (the person who asked Hermes).
    #[serde(default)]
    pub for_user_id: Option<i64>,
    /// Their name; for a direct action, the report's "Reported by" when it says (unverified).
    #[serde(default)]
    pub for_name: Option<String>,
    /// "voice note", "chat", "live voice report", "Fizzy comment"… `None`: source unknown.
    #[serde(default)]
    pub source: Option<String>,
    /// Who confirmed, dismissed or undid it.
    #[serde(default)]
    pub by_user_id: Option<i64>,
    #[serde(default)]
    pub by_name: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub reverse: Option<Reverse>,
    /// Why there's no undo, when there isn't.
    #[serde(default)]
    pub no_undo: Option<String>,
    /// For [`Kind::Undone`]: the entry undone.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub proposal: Option<String>,
    /// For a proposal's entry: the room it came from (its details are for that room's members and
    /// the duty managers). `None`: no room, or an entry older than this field.
    #[serde(default)]
    pub room_id: Option<i64>,
    /// The Fizzy activity, for a direct entry.
    #[serde(default)]
    pub activity: Option<String>,
}

impl Entry {
    pub fn new(id: impl Into<String>, at: Timestamp, kind: Kind, via: Via, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            at,
            kind,
            via,
            card: None,
            text: text.into(),
            for_user_id: None,
            for_name: None,
            source: None,
            by_user_id: None,
            by_name: None,
            error: None,
            reverse: None,
            no_undo: None,
            target: None,
            proposal: None,
            room_id: None,
            activity: None,
        }
    }
}

/// `hermes-log.jsonl` and its entries.
#[derive(Debug)]
pub struct HermesLog {
    path: PathBuf,
    state: RwLock<State>,
}

#[derive(Debug, Default)]
struct State {
    /// Oldest first.
    entries: Vec<Entry>,
    ids: HashSet<String>,
    compacted: Option<Timestamp>,
    error: Option<String>,
}

impl HermesLog {
    /// Reads the log, dropping entries older than [`RETENTION`] (and rewriting the file if any).
    pub fn open(path: impl Into<PathBuf>, now: Timestamp) -> Self {
        let log = Self { path: path.into(), state: RwLock::new(State::default()) };
        let mut state = State::default();
        match crate::store::read_json_lines::<Entry>(&log.path) {
            Ok(entries) => {
                for entry in entries {
                    if state.ids.insert(entry.id.clone()) {
                        state.entries.push(entry);
                    }
                }
            }
            Err(error) => state.error = Some(format!("couldn't read {}: {error}", log.path.display())),
        }
        state.entries.sort_by_key(|entry| entry.at);
        *log.state.write().unwrap_or_else(|e| e.into_inner()) = state;
        log.compact(now);
        log
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds an entry unless one with its id is there; `false` for a duplicate.
    pub fn add(&self, entry: Entry) -> bool {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if !state.ids.insert(entry.id.clone()) {
            return false;
        }
        if let Err(error) = crate::store::append_json_line(&self.path, &entry, u64::MAX) {
            state.error = Some(format!("couldn't write {}: {error}", self.path.display()));
        }
        let at = state.entries.partition_point(|known| known.at <= entry.at);
        state.entries.insert(at, entry);
        true
    }

    pub fn contains(&self, id: &str) -> bool {
        self.state.read().unwrap_or_else(|e| e.into_inner()).ids.contains(id)
    }

    pub fn get(&self, id: &str) -> Option<Entry> {
        self.state.read().unwrap_or_else(|e| e.into_inner()).entries.iter().find(|entry| entry.id == id).cloned()
    }

    /// Newest first.
    pub fn entries(&self) -> Vec<Entry> {
        self.state.read().unwrap_or_else(|e| e.into_inner()).entries.iter().rev().cloned().collect()
    }

    /// The undo of entry `id`, if it was undone.
    pub fn undo_of(&self, id: &str) -> Option<Entry> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.entries.iter().find(|entry| entry.kind == Kind::Undone && entry.target.as_deref() == Some(id)).cloned()
    }

    /// Drops what's older than [`RETENTION`], at most once a day (and at boot).
    pub fn compact(&self, now: Timestamp) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state.compacted.is_some_and(|at| now.duration_since(at) < SignedDuration::from_hours(24)) {
            return;
        }
        state.compacted = Some(now);
        let before = state.entries.len();
        state.entries.retain(|entry| now.duration_since(entry.at) <= RETENTION);
        if state.entries.len() == before {
            return;
        }
        let kept: HashSet<String> = state.entries.iter().map(|entry| entry.id.clone()).collect();
        state.ids = kept;
        if let Err(error) = crate::store::write_json_lines_atomically(&self.path, &state.entries) {
            state.error = Some(format!("couldn't rewrite {}: {error}", self.path.display()));
        }
    }

    /// The last read or write error, once (the app logs it).
    pub fn take_error(&self) -> Option<String> {
        self.state.write().unwrap_or_else(|e| e.into_inner()).error.take()
    }
}

// --- Hermes's direct actions, from Fizzy's activity feed --------------------------------------------

/// The card an activity is about.
pub fn activity_card(activity: &Activity) -> Option<u64> {
    if activity.eventable_type.as_deref() == Some("Card")
        && let Some(number) = activity.eventable["number"].as_u64()
    {
        return Some(number);
    }
    let url = activity.url.as_deref().or_else(|| activity.eventable["card"]["url"].as_str()).unwrap_or("");
    crate::fizzy::card_number_in(url)
}

/// The write-log actions a Fizzy activity can come from.
fn same_kind(activity: &str, write: &str) -> bool {
    match activity {
        "card_published" => write == "create card",
        "comment_created" => write == "comment",
        "card_triaged" => write.starts_with("move column:"),
        "card_sent_back_to_triage" => write == "move new",
        "card_postponed" => write == "move not_now",
        "card_closed" => write == "close",
        // Moving a closed card reopens it too.
        "card_reopened" => write == "reopen" || write.starts_with("move "),
        "card_title_changed" => write == "title",
        _ => false,
    }
}

/// Whether Campfire made this change itself: a write in the durable log on the same card, of the
/// same kind, within [`SAME_CHANGE`], through a token that is Hermes's Fizzy user (`hermes`, or
/// `workspace` when the workspace's token is Hermes's too: `shared`).
pub fn made_by_campfire(activity: &Activity, writes: &[journal::Entry], shared: bool) -> bool {
    let (Some(card), Some(at)) = (activity_card(activity), activity.created_at) else { return false };
    writes.iter().any(|write| {
        write.card == Some(card)
            && write.outcome == "ok"
            && (write.identity == "hermes" || (shared && write.identity == "workspace"))
            && same_kind(&activity.action, &write.action)
            && at.duration_since(write.at).abs() <= SAME_CHANGE
    })
}

/// "Reported by: Maya" in a report's text (the incident-report template's field), unverified.
fn reported_by(description: &str) -> Option<String> {
    static REPORTED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?im)^\W{0,3}(?:reported by|signalé par|rapporté par)\W{0,3}\s*[:：]\s*\**\s*([^\n*]{1,60})").unwrap()
    });
    let name = REPORTED.captures(description)?[1].trim().to_string();
    (!name.is_empty() && !name.eq_ignore_ascii_case("not stated")).then_some(name)
}

/// Where a report came from, when its text says (the skill's and the voice page's wording).
fn source_of_report(description: &str) -> Option<String> {
    let lower = description.to_lowercase();
    if lower.contains("dicté en direct") || lower.contains("live voice") {
        Some("live voice report".into())
    } else if lower.contains("voice note") || lower.contains("message vocal") {
        Some("voice note".into())
    } else {
        None
    }
}

fn card_title(activity: &Activity, card: Option<&Card>) -> String {
    let title =
        activity.eventable["title"].as_str().map(str::to_string).or_else(|| card.map(|card| card.title.clone())).unwrap_or_default();
    html::truncate(title.trim(), 80)
}

/// The log entry of one of Hermes's own Fizzy activities. `card` is the card as the workspace knows
/// it (for its title); `can_delete_comments`: Campfire holds Hermes's token.
pub fn direct_entry(activity: &Activity, card: Option<&Card>, can_delete_comments: bool) -> Option<Entry> {
    let at = activity.created_at?;
    let number = activity_card(activity);
    let title = card_title(activity, card);
    let on = |verb: &str| match number {
        Some(number) if title.is_empty() => format!("{verb} #{number}"),
        Some(number) => format!("{verb} #{number} {title}"),
        None => verb.to_string(),
    };
    let unknown_before = "Fizzy doesn't record where the card was before: move it back from the card.";
    let particulars = |key: &str| activity_particular(activity, key);
    let (kind, text, reverse, no_undo): (Kind, String, Option<Reverse>, Option<&str>) = match activity.action.as_str() {
        "card_published" => (Kind::Created, on("Created"), Some(Reverse::CloseCreated), None),
        "card_triaged" => {
            let column = particulars("column").or_else(|| activity.eventable["column"]["name"].as_str().map(str::to_string));
            (Kind::Moved, format!("{} to {}", on("Moved"), column.unwrap_or_else(|| "a column".into())), None, Some(unknown_before))
        }
        "card_sent_back_to_triage" => (Kind::Moved, format!("{} back to New", on("Moved")), None, Some(unknown_before)),
        "card_postponed" => (Kind::Moved, format!("{} to Monitoring", on("Moved")), None, Some(unknown_before)),
        "card_closed" => (Kind::Closed, on("Closed"), Some(Reverse::Reopen), None),
        "card_reopened" => (Kind::Reopened, on("Reopened"), Some(Reverse::Close), None),
        "card_title_changed" => match (particulars("old_title"), particulars("new_title")) {
            (Some(before), Some(after)) => (
                Kind::Renamed,
                format!(
                    "Renamed #{} from “{}” to “{}”",
                    number.unwrap_or_default(),
                    html::truncate(&before, 60),
                    html::truncate(&after, 60)
                ),
                Some(Reverse::Title { before, after }),
                None,
            ),
            _ => (Kind::Renamed, on("Renamed"), None, Some("Fizzy didn't say what the title was.")),
        },
        "comment_created" => {
            let excerpt = activity.eventable["body"]["plain_text"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| html::to_text(activity.eventable["body"]["html"].as_str().unwrap_or("")));
            let text = format!("{}: “{}”", on("Commented on"), html::truncate(&excerpt.replace('\n', " "), 120));
            match activity.eventable["id"].as_str().filter(|id| !id.is_empty()) {
                Some(id) if can_delete_comments => {
                    (Kind::Commented, text, Some(Reverse::DeleteComment { comment_id: id.into(), identity: "hermes".into() }), None)
                }
                _ => (
                    Kind::Commented,
                    text,
                    None,
                    Some("Only Hermes's own token can delete its comment, and Campfire doesn't hold it (HERMES_FIZZY_TOKEN)."),
                ),
            }
        }
        "card_assigned" | "card_unassigned" => (Kind::Assigned, on("Changed the owners of"), None, Some("Campfire never reassigns cards.")),
        "card_board_changed" => (Kind::Moved, on("Moved to this board"), None, Some("Moving between boards isn't undone from Campfire.")),
        "card_auto_postponed" => return None,
        other => (Kind::Other, format!("{} ({other})", on("Changed")), None, Some("Not an action Campfire can take back.")),
    };
    let description = activity.eventable["description"].as_str().map(str::to_string).or_else(|| card.map(|card| card.description.clone()));
    let mut entry = Entry::new(format!("a-{}", activity.id), at, kind, Via::Direct, text);
    entry.card = number;
    entry.reverse = reverse;
    entry.no_undo = no_undo.map(str::to_string);
    entry.activity = Some(activity.id.clone());
    if kind == Kind::Created {
        entry.for_name = description.as_deref().and_then(reported_by);
        entry.source = description.as_deref().and_then(source_of_report);
    }
    Some(entry)
}

/// `particulars.<key>` (in `/activities` only, not in webhooks).
fn activity_particular(activity: &Activity, key: &str) -> Option<String> {
    activity.particulars[key].as_str().map(str::to_string).filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(time: &str) -> Timestamp {
        time.parse().unwrap()
    }

    fn activity(value: serde_json::Value) -> Activity {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn the_log_dedups_by_id_survives_a_restart_and_forgets_after_ninety_days() {
        let dir = crate::store::scratch_dir("hermes-log");
        let path = dir.join("hermes-log.jsonl");
        let now = at("2026-09-30T09:00:00Z");
        let log = HermesLog::open(&path, now);
        assert!(log.add(Entry::new("a-1", now, Kind::Created, Via::Direct, "Created #13")));
        assert!(!log.add(Entry::new("a-1", now, Kind::Created, Via::Direct, "again")), "deduplicated by id");
        let old = now - SignedDuration::from_hours(91 * 24);
        assert!(log.add(Entry::new("a-0", old, Kind::Closed, Via::Direct, "Closed #2")));
        assert_eq!(log.entries().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a-1", "a-0"], "newest first");

        let reopened = HermesLog::open(&path, now);
        assert_eq!(reopened.entries().len(), 1, "older than 90 days: dropped");
        assert!(reopened.contains("a-1") && !reopened.contains("a-0"));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1, "the file was compacted");
        assert!(!reopened.add(Entry::new("a-1", now, Kind::Created, Via::Direct, "x")), "still deduplicated after a restart");
        assert_eq!(reopened.take_error(), None);

        let mut undo = Entry::new("u-a-1", now, Kind::Undone, Via::Campfire, "Undone");
        undo.target = Some("a-1".into());
        reopened.add(undo);
        assert_eq!(reopened.undo_of("a-1").unwrap().id, "u-a-1");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_entries_say_what_hermes_did() {
        let published = activity(json!({
            "id": "act9", "action": "card_published", "created_at": "2026-09-30T08:00:00Z",
            "eventable_type": "Card",
            "eventable": {"number": 13, "title": "Guest slip in lobby", "description": "Type: incident\nReported by: Maya\nOriginal transcript (voice note, transcribed): …"},
            "creator": {"id": "fz-hermes", "name": "Hermes"}
        }));
        let entry = direct_entry(&published, None, true).unwrap();
        assert_eq!((entry.id.as_str(), entry.kind, entry.via, entry.card), ("a-act9", Kind::Created, Via::Direct, Some(13)));
        assert_eq!(entry.text, "Created #13 Guest slip in lobby");
        assert_eq!((entry.for_name.as_deref(), entry.source.as_deref()), (Some("Maya"), Some("voice note")));
        assert_eq!(entry.reverse, Some(Reverse::CloseCreated));

        let triaged = activity(json!({
            "id": "act10", "action": "card_triaged", "created_at": "2026-09-30T08:01:00Z", "particulars": {"column": "In progress"},
            "eventable_type": "Card", "eventable": {"number": 13, "title": "Guest slip"}
        }));
        let entry = direct_entry(&triaged, None, true).unwrap();
        assert_eq!(entry.text, "Moved #13 Guest slip to In progress");
        assert!(entry.reverse.is_none() && entry.no_undo.unwrap().contains("where the card was before"));

        let comment = activity(json!({
            "id": "act11", "action": "comment_created", "created_at": "2026-09-30T08:02:00Z",
            "url": "http://localhost:8484/897/cards/13", "eventable_type": "Comment",
            "eventable": {"id": "cm7", "body": {"plain_text": "Security is on the way", "html": "<p>Security is on the way</p>"}}
        }));
        let entry = direct_entry(&comment, None, true).unwrap();
        assert_eq!(entry.text, "Commented on #13: “Security is on the way”");
        assert_eq!(entry.reverse, Some(Reverse::DeleteComment { comment_id: "cm7".into(), identity: "hermes".into() }));
        let without_token = direct_entry(&comment, None, false).unwrap();
        assert!(without_token.reverse.is_none() && without_token.no_undo.unwrap().contains("HERMES_FIZZY_TOKEN"));

        let renamed = activity(json!({
            "id": "act12", "action": "card_title_changed", "created_at": "2026-09-30T08:03:00Z",
            "particulars": {"old_title": "Slip", "new_title": "Guest slip"}, "eventable_type": "Card", "eventable": {"number": 13}
        }));
        assert_eq!(
            direct_entry(&renamed, None, true).unwrap().reverse,
            Some(Reverse::Title { before: "Slip".into(), after: "Guest slip".into() })
        );
        let closed = activity(
            json!({"id": "a13", "action": "card_closed", "created_at": "2026-09-30T08:04:00Z", "eventable_type": "Card", "eventable": {"number": 13}}),
        );
        assert_eq!(direct_entry(&closed, None, true).unwrap().reverse, Some(Reverse::Reopen));
        let assigned = activity(
            json!({"id": "a14", "action": "card_assigned", "created_at": "2026-09-30T08:04:00Z", "eventable_type": "Card", "eventable": {"number": 13}}),
        );
        assert!(direct_entry(&assigned, None, true).unwrap().no_undo.unwrap().contains("never reassigns"));
    }

    #[test]
    fn campfire_s_own_writes_are_recognized() {
        let write = |identity: &str, action: &str, at_time: &str| journal::Entry {
            at: at(at_time),
            user_id: 7,
            card: Some(13),
            action: action.into(),
            identity: identity.into(),
            via: "workspace".into(),
            reference: None,
            outcome: "ok".into(),
        };
        let closed = activity(
            json!({"id": "a1", "action": "card_closed", "created_at": "2026-09-30T08:00:30Z", "eventable_type": "Card", "eventable": {"number": 13}}),
        );
        let people = [write("workspace", "close", "2026-09-30T08:00:00Z")];
        assert!(made_by_campfire(&closed, &people, true), "the workspace writes with Hermes's token: a person's close");
        assert!(!made_by_campfire(&closed, &people, false), "a separate workspace user: Fizzy wouldn't show it as Hermes's");
        assert!(made_by_campfire(&closed, &[write("hermes", "close", "2026-09-30T08:01:00Z")], false), "a proposal Campfire ran");
        assert!(!made_by_campfire(&closed, &[write("hermes", "close", "2026-09-30T08:05:00Z")], false), "too far apart");
        assert!(!made_by_campfire(&closed, &[write("hermes", "comment", "2026-09-30T08:00:00Z")], false), "another kind");
    }
}
