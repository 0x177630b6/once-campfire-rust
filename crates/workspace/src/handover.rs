//! Phase 2.6: the end-of-shift handover (decision D10).
//!
//! **Prepare handover** (Home, duty managers) opens `GET /workspace/handover`: a summary built from
//! the last poll, in an editable text box: what is still open (by severity, with owners and where
//! it is), what's new this shift, what closed this shift, and what waits for a confirmation. The duty
//! manager edits it and posts it to the handover room (`settings.handover.room_id`) **under their own
//! name** (`POST /workspace/handover`, through Campfire's `create_message`). Nothing is written to
//! Fizzy and nothing is written by the AI: every line comes from the board.
//!
//! "This shift" is the shift whose end is nearest (07:00, 15:00, 23:00 in the settings' time zone
//! by default, [`crate::shifts`]), from the end before it; it starts at the last handover posted
//! instead when that's later, or when that handed over the shift before, early (posted at 14:50
//! for the shift ending at 15:00, the next one starts at 14:50). Only what every member of the handover room may see is listed
//! (phase 2.7): the rest is counted, not named, in a note above the box (not in the message). While
//! the room's members aren't known (the people and rooms not read yet, or nobody known to be in
//! it), only what a person in no room may see is listed, and no proposal.
//! Until a room is set the page shows the summary but can't post it. A summary longer than a message
//! can be (10,000 characters) is cut at a line, with a note saying so.

use askama::Template;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::cache::Snapshot;
use crate::chips::card_link;
use crate::config::WorkspaceConfig;
use crate::fizzy::{Card, Severity};
use crate::html::escape;
use crate::proposals::{Proposal, Status};
use crate::settings::{Settings, SettingsError};
use crate::shifts;
use crate::writes::ActionError;

pub const HANDOVER_PATH: &str = "/workspace/handover";
pub const MAX_HANDOVER_CHARS: usize = 10_000;

/// `settings.handover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoverSettings {
    /// Where the handover is posted; `None`: not set yet (the page can't post).
    pub room_id: Option<i64>,
    /// `"HH:MM"`, in `time_zone`.
    pub shift_ends: Vec<String>,
    /// An IANA name (`Europe/Paris`) or a POSIX TZ string ([`crate::shifts::time_zone`]).
    pub time_zone: String,
    /// A direct message to the duty managers at each shift end (when a room is set).
    pub reminder: bool,
}

impl Default for HandoverSettings {
    fn default() -> Self {
        Self {
            room_id: None,
            shift_ends: shifts::DEFAULT_SHIFT_ENDS.iter().map(|end| end.to_string()).collect(),
            time_zone: shifts::DEFAULT_TIME_ZONE.into(),
            reminder: true,
        }
    }
}

impl HandoverSettings {
    pub(crate) fn validated(mut self) -> Result<Self, SettingsError> {
        let mut ends: Vec<(i8, i8)> = Vec::new();
        for value in &self.shift_ends {
            let Some(end) = shifts::parse_time(value) else {
                return Err(SettingsError(format!("“{value}” isn't a time of day (HH:MM, e.g. 07:00).")));
            };
            if !ends.contains(&end) {
                ends.push(end);
            }
        }
        ends.sort();
        if ends.is_empty() || ends.len() > shifts::MAX_SHIFTS {
            return Err(SettingsError(format!("Give 1 to {} shift end times.", shifts::MAX_SHIFTS)));
        }
        self.shift_ends = ends.iter().map(|(hours, minutes)| format!("{hours:02}:{minutes:02}")).collect();
        self.time_zone = self.time_zone.trim().to_string();
        if shifts::time_zone(&self.time_zone).is_none() {
            return Err(SettingsError(format!(
                "“{}” isn't a time zone Meshduty knows (an IANA name such as Europe/Paris, or a POSIX TZ rule).",
                self.time_zone
            )));
        }
        if self.room_id.is_some_and(|id| id <= 0) {
            return Err(SettingsError("The handover room isn't valid.".into()));
        }
        Ok(self)
    }

    pub fn ends(&self) -> Vec<(i8, i8)> {
        self.shift_ends.iter().filter_map(|end| shifts::parse_time(end)).collect()
    }

    /// The time zone (UTC if it can't be resolved on this system).
    pub fn zone(&self) -> TimeZone {
        shifts::time_zone(&self.time_zone).unwrap_or(TimeZone::UTC)
    }
}

/// What the summary is built from.
pub struct Input<'a> {
    pub config: &'a WorkspaceConfig,
    pub snapshot: &'a Snapshot,
    pub settings: &'a Settings,
    pub proposals: &'a [Proposal],
    pub now: Timestamp,
    /// The last handover posted, if any.
    pub last_posted: Option<Timestamp>,
    /// Whether every member of the handover room may see a card with these tags.
    pub listable: &'a dyn Fn(&[String]) -> bool,
    /// Whether every member of the handover room may see this proposal.
    pub proposal_listable: &'a dyn Fn(&Proposal) -> bool,
}

/// The summary: its text, and what was left out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub text: String,
    pub since: Timestamp,
    pub shift_end: Timestamp,
    pub left_out: usize,
}

/// The prefilled summary. Plain text: the box edits it, [`message_html`] posts it.
pub fn summary(input: &Input<'_>) -> Summary {
    let (settings, snapshot, now) = (input.settings, input.snapshot, input.now);
    let zone = settings.handover.zone();
    let ends = settings.handover.ends();
    let (start, shift_end) = shifts::handed_over(now, &ends, &zone).unwrap_or((now, now));
    // Since the last handover posted, when it's within this shift, or when it handed over the shift
    // before this one, early (at 14:50 for the shift ending at 15:00): what came after it wasn't in it.
    let since = input
        .last_posted
        .filter(|posted| *posted <= now)
        .filter(|posted| *posted > start || shifts::handed_over(*posted, &ends, &zone).is_some_and(|(_, end)| end == start))
        .unwrap_or(start);
    let mut hidden = std::collections::BTreeSet::new();
    let mut listed = |card: &Card| {
        let ok = (input.listable)(&card.tags);
        if !ok {
            hidden.insert(card.number);
        }
        ok
    };
    let open: Vec<&Card> =
        snapshot.open.iter().filter_map(|n| snapshot.card(*n)).filter(|card| !card.closed).filter(|card| listed(card)).collect();
    let mut all: Vec<&Card> = snapshot.open.iter().chain(&snapshot.recently_closed).filter_map(|n| snapshot.card(*n)).collect();
    all.sort_by_key(|card| card.number);
    all.dedup_by_key(|card| card.number);
    let in_shift = |at: Option<Timestamp>| at.is_some_and(|at| at >= since && at <= now);
    let fresh: Vec<&Card> = all.iter().copied().filter(|card| in_shift(card.created_at)).filter(|card| listed(card)).collect();
    let closed: Vec<&Card> =
        all.iter().copied().filter(|card| card.closed && in_shift(card.last_active_at)).filter(|card| listed(card)).collect();
    let pending: Vec<&Proposal> = input.proposals.iter().filter(|proposal| proposal.status == Status::Pending).collect();
    let pending_listed: Vec<&Proposal> = pending.iter().copied().filter(|proposal| (input.proposal_listable)(proposal)).collect();
    let left_out = hidden.len() + pending.len() - pending_listed.len();

    let line = |card: &Card, with_severity: bool| {
        let url = card_link(input.config, snapshot, card.number).unwrap_or_else(|| card.url.clone());
        let owners = if card.assignees.is_empty() {
            "nobody assigned".to_string()
        } else {
            card.assignees.iter().map(|user| user.name.as_str()).collect::<Vec<_>>().join(", ")
        };
        let severity = match card.severity() {
            Some(severity) if with_severity => format!(" · {}", severity.as_str()),
            _ => String::new(),
        };
        let title = if card.title.trim().is_empty() { "Untitled" } else { card.title.trim() };
        format!("- #{} {title}{severity} · {} · {owners} · {url}", card.number, card.state().label())
    };
    let since_label = shifts::local_time(since, &zone);
    let mut text = format!("Handover: shift ending {}\n", shifts::local_label(shift_end, &zone));
    text.push_str(&format!(
        "Since {since_label}: {} new, {} closed, {} still open, {} waiting for a confirmation.\n",
        fresh.len(),
        closed.len(),
        open.len(),
        pending_listed.len()
    ));
    text.push_str(&format!("\nStill open ({})\n", open.len()));
    if open.is_empty() {
        text.push_str("- none\n");
    }
    let levels: Vec<Option<Severity>> = Severity::ALL.iter().copied().map(Some).chain([None]).collect();
    for level in levels {
        let mut cards: Vec<&Card> = open.iter().copied().filter(|card| card.severity() == level).collect();
        if cards.is_empty() {
            continue;
        }
        cards.sort_by_key(|card| (card.created_at, card.number));
        text.push_str(&format!("{}\n", level.map(|severity| capitalized(severity.as_str())).unwrap_or_else(|| "No severity".into())));
        for card in cards {
            text.push_str(&line(card, false));
            text.push('\n');
        }
    }
    text.push_str(&format!("\nNew this shift ({})\n", fresh.len()));
    if fresh.is_empty() {
        text.push_str("- none\n");
    }
    for card in &fresh {
        text.push_str(&line(card, true));
        text.push('\n');
    }
    text.push_str(&format!("\nClosed this shift ({})\n", closed.len()));
    if closed.is_empty() {
        text.push_str("- none\n");
    }
    for card in &closed {
        text.push_str(&line(card, true));
        text.push('\n');
    }
    text.push_str(&format!("\nWaiting for a confirmation ({})\n", pending_listed.len()));
    if pending_listed.is_empty() {
        text.push_str("- none\n");
    }
    for proposal in pending_listed {
        let whom = match (&proposal.context.user_name, &proposal.context.room_name) {
            (Some(person), Some(room)) => format!(" (for {person} in {room})"),
            (Some(person), None) => format!(" (for {person})"),
            (None, Some(room)) => format!(" (in {room})"),
            (None, None) => String::new(),
        };
        text.push_str(&format!("- {}{whom}\n", proposal.summary));
    }
    Summary { text: fit(text.trim_end()), since, shift_end, left_out }
}

/// Said at the end of a summary cut to fit a message.
const CUT_NOTE: &str = "… Cut here: longer than a handover can be; the rest is on the board.";

/// `text`, cut at a line to fit a message ([`MAX_HANDOVER_CHARS`]) with a note, when it's longer.
fn fit(text: &str) -> String {
    if text.chars().count() <= MAX_HANDOVER_CHARS {
        return text.to_string();
    }
    let room = MAX_HANDOVER_CHARS - CUT_NOTE.chars().count() - 2;
    let end = text.char_indices().nth(room).map_or(text.len(), |(at, _)| at);
    let kept = &text[..end];
    let kept = kept.rfind('\n').map_or(kept, |at| &kept[..at]).trim_end();
    format!("{kept}\n\n{CUT_NOTE}")
}

/// The posted message's HTML: paragraphs at blank lines, lines kept, links made clickable (a card's
/// link renders as its chip). `422` for a blank or too long text.
pub fn message_html(text: &str) -> Result<String, ActionError> {
    let text = text.replace("\r\n", "\n");
    if text.trim().is_empty() {
        return Err(ActionError::invalid("blank_text", "The handover is empty."));
    }
    if text.chars().count() > MAX_HANDOVER_CHARS {
        return Err(ActionError::invalid("text_too_long", format!("The handover is too long ({MAX_HANDOVER_CHARS} characters at most).")));
    }
    let mut html = String::new();
    for paragraph in text.trim().split("\n\n").map(str::trim).filter(|paragraph| !paragraph.is_empty()) {
        let lines: Vec<String> = paragraph.lines().map(|line| linked(line.trim_end())).collect();
        html.push_str(&format!("<p>{}</p>", lines.join("<br>")));
    }
    Ok(html)
}

/// A line, escaped, its `http(s)://` words as links.
fn linked(line: &str) -> String {
    line.split(' ')
        .map(|word| {
            let is_link = (word.starts_with("https://") || word.starts_with("http://")) && word.len() > 8;
            if is_link { format!(r#"<a href="{0}">{0}</a>"#, escape(word)) } else { escape(word) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl crate::Workspace {
    /// The handover page for `viewer` (a duty manager): the summary for the handover room's members,
    /// and whether `viewer` can post it (a room is set, and they're a member of it).
    pub fn handover_page(&self, viewer: &crate::Viewer) -> Result<HandoverPage, ActionError> {
        self.authorize(&crate::Act::Handover, viewer)?;
        let (settings, snapshot, directory) = (self.settings(), self.snapshot(), self.directory());
        let proposals = self.pending_proposals();
        let room = settings.handover.room_id;
        let members: Vec<crate::Viewer> = match room {
            Some(room) => directory.members_of(room).into_iter().filter_map(|id| directory.viewer(id)).collect(),
            None => vec![viewer.clone()],
        };
        // The room's members unknown (the people and rooms not read yet, or nobody known to be in
        // it): only what someone in no room at all may see is listed, and no proposal.
        let known = room.is_none() || (directory.loaded && !members.is_empty());
        let listable = |tags: &[String]| {
            if !known {
                return settings.card_visible(tags, &crate::Audience::default());
            }
            members.iter().all(|member| settings.card_visible(tags, &settings.audience(member, directory.rooms_of(member.id))))
        };
        let proposal_listable = |proposal: &Proposal| {
            known
                && members.iter().all(|member| {
                    let rooms: Vec<i64> = directory.rooms_of(member.id).into_iter().collect();
                    self.proposal_visible(member, proposal, &rooms)
                })
        };
        let posted = self.notified().handover_posted();
        let input = Input {
            config: self.config(),
            snapshot: &snapshot,
            settings: &settings,
            proposals: &proposals,
            now: self.now(),
            last_posted: posted.as_ref().map(|(at, _)| *at),
            listable: &listable,
            proposal_listable: &proposal_listable,
        };
        let summary = summary(&input);
        let zone = settings.handover.zone();
        let room_name = room.map(|id| directory.rooms.get(&id).cloned().unwrap_or_else(|| format!("room {id}")));
        let blocked = match room {
            None => Some("No handover room is set yet: an administrator picks it in the workspace settings.".to_string()),
            Some(id) if !directory.is_member(viewer.id, id) => {
                Some(format!("You aren’t a member of {}, so you can’t post there.", room_name.clone().unwrap_or_default()))
            }
            Some(_) => None,
        };
        Ok(HandoverPage {
            action: HANDOVER_PATH.into(),
            text: summary.text,
            room_name,
            can_post: blocked.is_none(),
            blocked,
            left_out: summary.left_out,
            since: shifts::local_label(summary.since, &zone),
            shift_end: shifts::local_label(summary.shift_end, &zone),
            last_posted: posted.as_ref().map(|(at, _)| at.to_string()),
            last_posted_by: posted.and_then(|(_, by)| by),
            stale: snapshot.last_error.is_some(),
            settings_url: viewer.administrator.then(|| crate::pages::SETTINGS_PATH.to_string()),
        })
    }

    /// Checks a handover `viewer` wants to post: they're a duty manager, a room is set and they're
    /// in it, the text isn't blank or too long. The room and the message's HTML.
    pub fn handover_message(&self, viewer: &crate::Viewer, text: &str) -> Result<(i64, String), ActionError> {
        self.authorize(&crate::Act::Handover, viewer)?;
        let Some(room) = self.settings().handover.room_id else {
            return Err(ActionError::invalid(
                "no_room",
                "No handover room is set yet: an administrator picks it in the workspace settings.",
            ));
        };
        if !self.directory().is_member(viewer.id, room) {
            return Err(ActionError::invalid("not_a_member", "You aren’t a member of the handover room, so you can’t post there."));
        }
        Ok((room, message_html(text)?))
    }

    /// The handover was posted: the next one starts from now.
    pub fn handover_posted(&self, viewer: &crate::Viewer) {
        self.notified().set_handover_posted(self.now(), &viewer.name);
    }
}

/// `GET /workspace/handover` (duty managers).
#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/handover.html")]
pub struct HandoverPage {
    pub action: String,
    pub text: String,
    /// `None`: no handover room set.
    pub room_name: Option<String>,
    /// The viewer can post it (a room is set and they're a member).
    pub can_post: bool,
    /// Why not, when they can't.
    pub blocked: Option<String>,
    pub left_out: usize,
    pub since: String,
    pub shift_end: String,
    pub last_posted: Option<String>,
    pub last_posted_by: Option<String>,
    pub stale: bool,
    pub settings_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handover_settings_are_validated() {
        let valid = HandoverSettings {
            shift_ends: vec!["23:00".into(), "07:00".into(), "15:00".into(), "07:00".into()],
            time_zone: " Europe/Paris ".into(),
            ..HandoverSettings::default()
        }
        .validated()
        .unwrap();
        assert_eq!(valid.shift_ends, ["07:00", "15:00", "23:00"]);
        assert_eq!(valid.time_zone, "Europe/Paris");
        for bad in [
            HandoverSettings { shift_ends: vec![], ..HandoverSettings::default() },
            HandoverSettings { shift_ends: vec!["25:00".into()], ..HandoverSettings::default() },
            HandoverSettings { time_zone: "Nowhere/Land".into(), ..HandoverSettings::default() },
            HandoverSettings { room_id: Some(0), ..HandoverSettings::default() },
        ] {
            assert!(bad.validated().is_err());
        }
    }

    #[test]
    fn the_message_keeps_lines_and_links() {
        let html = message_html("Handover <b>now</b>\n- #13 Slip · https://f.example/897/cards/13\n\n\nNew (0)\r\n- none").unwrap();
        assert_eq!(
            html,
            r#"<p>Handover &lt;b&gt;now&lt;/b&gt;<br>- #13 Slip · <a href="https://f.example/897/cards/13">https://f.example/897/cards/13</a></p><p>New (0)<br>- none</p>"#
        );
        assert_eq!(message_html("  \n ").unwrap_err().code(), "blank_text");
        assert_eq!(message_html(&"x".repeat(MAX_HANDOVER_CHARS + 1)).unwrap_err().code(), "text_too_long");
        assert!(message_html(r#"see https://x.example/"onmouseover="a"#).unwrap().contains("&quot;onmouseover=&quot;"));
    }
}
