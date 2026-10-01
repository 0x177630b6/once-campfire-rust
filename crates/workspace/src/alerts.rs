//! Phase 2.5: alerts for serious incidents, and reminders (decision D9).
//!
//! At every poll, [`detect`] compares the new picture with the one the previous poll produced (not
//! the live picture, which what Campfire writes or reads updates between polls):
//!
//! - a card that **becomes** critical or high (`notifications.severities`): filed since the first
//!   poll, or its severity raised to one of them. Each duty manager gets a direct message from
//!   Hermes's bot ("Critical: Guest slip in lobby · #13 · nobody assigned", with the card's link,
//!   which renders as a chip), and, with `department_rooms`, the rooms of the card's departments
//!   where everyone may see it get a short notice ([`Settings::notice_rooms`]);
//! - such a card still in **New** `new_reminder_min` minutes after it was created: a reminder to the
//!   duty managers;
//! - a Hermes **proposal** still waiting `draft_reminder_min` minutes: a reminder to the person it's
//!   for (when they may see it), then, as long again later, to the duty managers;
//! - the **handover** (phase 2.6): at each shift end, a reminder to the duty managers, when a
//!   handover room is set.
//!
//! - a ticket's **owner** changed ([`crate::owners`]): to the person concerned.
//!
//! What was sent is kept in `<CAMPFIRE_STORAGE_PATH>/hermes/notified.json` ([`NotifiedStore`]), so
//! nothing is sent twice, restarts included. An alert is recorded there once its messages are
//! posted ([`crate::Workspace::settle_alerts`]); one that couldn't be posted is tried again after
//! the next poll, to the recipients who missed it, [`MAX_ATTEMPTS`] times in all. The **first poll
//! after a start alerts on nothing** (owner pings excepted: they compare with the owners known
//! before the restart):
//! whatever would alert then is recorded as sent without sending it. Likewise while alerts are off:
//! what would be sent is recorded, not sent, so turning them back on sends nothing stale; and a
//! reminder is sent only while it's recently due (3 times its delay after it fell due, 10 minutes
//! at least), so a shorter delay doesn't send every reminder long overdue at once. Each person (and room) gets at
//! most one message per poll: several alerts at once are one message (10 lines at most, new serious
//! incidents first, the most severe first, then the reminders). Recipients
//! follow the visibility settings (phase 2.7). Delivery is the app's (the bot's direct room with each
//! person, so Campfire's own Web Push applies).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::cache::Snapshot;
use crate::chips::card_link;
use crate::config::WorkspaceConfig;
use crate::fizzy::{Card, CardState, Severity};
use crate::html::escape;
use crate::owners::{KnownOwners, OwnerChange, OwnerPing};
use crate::proposals::{Proposal, Status};
use crate::settings::{Settings, SettingsError};
use crate::visibility::Directory;

/// What was sent is forgotten after this.
const KEEP_SENT: SignedDuration = SignedDuration::from_hours(14 * 24);
/// A handover reminder is due for this long after the shift end.
const HANDOVER_DUE_FOR: SignedDuration = SignedDuration::from_mins(30);
/// Lines of one message, at most.
const MAX_LINES: usize = 10;
/// A reminder is sent only while it's recently due: for 3 times its delay after it fell due (10
/// minutes at least), so that a shorter delay, or reminders turned back on, don't send at once every
/// reminder long overdue.
const MIN_REMINDER_WINDOW: SignedDuration = SignedDuration::from_mins(10);

/// Whether something `elapsed` old, due at `due` (`delay` being the reminder's delay), is recently due.
fn recently_due(elapsed: SignedDuration, due: SignedDuration, delay: SignedDuration) -> bool {
    elapsed >= due && elapsed < due + (delay * 3).max(MIN_REMINDER_WINDOW)
}
pub const MAX_REMINDER_MINUTES: u32 = 24 * 60;

/// `settings.notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Off: no alert and no reminder at all.
    pub enabled: bool,
    /// The severities that alert (`critical`, `high`, `medium`, `low`).
    pub severities: Vec<String>,
    /// Also a notice in the rooms of the card's departments.
    pub department_rooms: bool,
    /// A critical/high card still in New this many minutes after it was filed: a reminder (0: none).
    pub new_reminder_min: u32,
    /// A proposal waiting this many minutes: a reminder to its person, then as long again later to
    /// the duty managers (0: none).
    pub draft_reminder_min: u32,
    /// Owner pings ([`crate::owners`]): a ticket's new owner, its removed owner and, on a
    /// reassignment, its previous owner get a direct message from Sky. Only reaches people who
    /// have both a Fizzy user and a Campfire user with the same email address.
    pub owner_pings: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            severities: vec!["critical".into(), "high".into()],
            department_rooms: true,
            new_reminder_min: 15,
            draft_reminder_min: 10,
            owner_pings: true,
        }
    }
}

impl Notifications {
    pub(crate) fn validated(mut self) -> Result<Self, SettingsError> {
        let mut severities = Vec::new();
        for value in &self.severities {
            let Some(severity) = Severity::parse(value) else {
                return Err(SettingsError(format!("“{value}” isn't a severity (critical, high, medium, low).")));
            };
            if !severities.contains(&severity) {
                severities.push(severity);
            }
        }
        severities.sort_by(|a, b| b.cmp(a));
        self.severities = severities.iter().map(|severity| severity.as_str().to_string()).collect();
        if self.new_reminder_min > MAX_REMINDER_MINUTES || self.draft_reminder_min > MAX_REMINDER_MINUTES {
            return Err(SettingsError(format!("Reminders are at most {MAX_REMINDER_MINUTES} minutes (0 turns them off).")));
        }
        Ok(self)
    }

    pub fn alerts_on(&self, severity: Option<Severity>) -> bool {
        severity.is_some_and(|severity| self.severities.iter().any(|value| Severity::parse(value) == Some(severity)))
    }
}

/// Something to tell people about, once (`key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub key: String,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// Filed as, or raised to, an alerting severity.
    Raised { card: u64, severity: Severity, from: Option<Severity> },
    /// Still in New.
    StillNew { card: u64, severity: Severity, minutes: u32 },
    /// A proposal still waiting: to its person, or (`managers`) to the duty managers.
    ProposalWaiting { id: String, managers: bool, minutes: u32 },
    /// A shift ended: the handover is due.
    HandoverDue { end: Timestamp },
    /// A ticket's owner changed ([`crate::owners`]): to the person it's about.
    Owner(OwnerPing),
}

/// An event waiting to be delivered, and to whom (`only`: a retry, to the recipients whose message
/// couldn't be posted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub event: Event,
    pub only: Option<BTreeSet<To>>,
    /// Deliveries tried so far.
    pub attempts: u32,
}

impl Pending {
    pub fn new(event: Event) -> Self {
        Self { event, only: None, attempts: 0 }
    }
}

/// An event whose messages couldn't be posted is tried again at the next polls, this many times in
/// all, then given up (and recorded as sent: it's stale by then).
pub const MAX_ATTEMPTS: u32 = 10;
/// A delivery the app took but didn't report back on within this (hung) is tried again.
pub const IN_FLIGHT_FOR: SignedDuration = SignedDuration::from_mins(10);

/// What should alert now, comparing `previous` and `next`. `primed_at`: when the first poll since
/// the start ran (`None` during it: every card counts as new, to be recorded without being sent).
/// Reminders are only for what is [recently due](recently_due).
pub fn detect(
    previous: &Snapshot,
    next: &Snapshot,
    settings: &Settings,
    proposals: &[Proposal],
    now: Timestamp,
    primed_at: Option<Timestamp>,
) -> Vec<Event> {
    let notifications = &settings.notifications;
    if !notifications.enabled {
        return Vec::new();
    }
    let mut events = Vec::new();
    let open: Vec<&Card> = next.open.iter().filter_map(|number| next.card(*number)).filter(|card| !card.closed).collect();
    for card in &open {
        let severity = card.severity();
        let Some(level) = severity.filter(|_| notifications.alerts_on(severity)) else { continue };
        let before = previous.card(card.number);
        let raised = match before {
            Some(before) => before.severity() < severity,
            // Unknown until now: filed since the first poll (not an old card a chip asked for).
            None => primed_at.is_none_or(|primed| card.created_at.is_some_and(|created| created > primed)),
        };
        if raised {
            let from = before.and_then(Card::severity);
            events.push(Event {
                key: format!("sev:{}:{}", card.number, level.as_str()),
                kind: EventKind::Raised { card: card.number, severity: level, from },
            });
        }
        let minutes = notifications.new_reminder_min;
        let delay = SignedDuration::from_mins(minutes.into());
        if minutes > 0
            && card.state() == CardState::New
            && card.created_at.is_some_and(|created| recently_due(now.duration_since(created), delay, delay))
        {
            events.push(Event {
                key: format!("new:{}", card.number),
                kind: EventKind::StillNew { card: card.number, severity: level, minutes },
            });
        }
    }
    let minutes = notifications.draft_reminder_min;
    if minutes > 0 {
        let after = SignedDuration::from_mins(minutes.into());
        for proposal in proposals.iter().filter(|proposal| proposal.status == Status::Pending) {
            let waited = now.duration_since(proposal.created_at);
            let has_person = proposal.context.user_id.is_some();
            if has_person && recently_due(waited, after, after) {
                events.push(Event {
                    key: format!("proposal:{}:person", proposal.id),
                    kind: EventKind::ProposalWaiting { id: proposal.id.clone(), managers: false, minutes },
                });
            }
            let managers_after = if has_person { after * 2 } else { after };
            if recently_due(waited, managers_after, after) {
                events.push(Event {
                    key: format!("proposal:{}:managers", proposal.id),
                    kind: EventKind::ProposalWaiting {
                        id: proposal.id.clone(),
                        managers: true,
                        minutes: (managers_after.as_secs() / 60) as u32,
                    },
                });
            }
        }
    }
    let handover = &settings.handover;
    if handover.reminder
        && handover.room_id.is_some()
        && let Some(end) = crate::shifts::last_end(now, &handover.ends(), &handover.zone())
        && now.duration_since(end) <= HANDOVER_DUE_FOR
    {
        events.push(Event { key: format!("handover:{}", end.as_second()), kind: EventKind::HandoverDue { end } });
    }
    events
}

/// Where a message goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum To {
    /// A person, in their direct room with Hermes's bot.
    Person(i64),
    /// A room, as Hermes's bot.
    Room(i64),
}

/// One message to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub to: To,
    /// The message's HTML.
    pub html: String,
    /// What it says, for the log (no card title).
    pub summary: String,
    /// The events it carries: recorded as sent once it's posted ([`crate::Workspace::settle_alerts`]).
    pub keys: Vec<String>,
}

/// What the planner needs besides the events.
pub struct Plan<'a> {
    pub config: &'a WorkspaceConfig,
    pub snapshot: &'a Snapshot,
    pub settings: &'a Settings,
    pub directory: &'a Directory,
    pub proposals: &'a [Proposal],
    /// Hermes's bot, which posts: it must be in a room to post a notice there.
    pub bot_id: i64,
}

/// A line's place in its message ([`plan`]).
type Rank = (u8, std::cmp::Reverse<Option<Severity>>);

/// The messages for `events`: one per person or room, whatever the number of events. In each, new
/// serious incidents come first (critical, then high…), then the reminders, before the 10-line cap.
pub fn plan(events: &[Event], plan: &Plan<'_>) -> Vec<Delivery> {
    let pending: Vec<Pending> = events.iter().cloned().map(Pending::new).collect();
    plan_pending(&pending, plan)
}

/// [`plan`], each event to its recipients, or only to those of [`Pending::only`].
pub fn plan_pending(events: &[Pending], plan: &Plan<'_>) -> Vec<Delivery> {
    let settings = plan.settings;
    let managers = plan.directory.duty_managers(settings);
    let sees = |person: i64, tags: &[String]| {
        plan.directory
            .viewer(person)
            .is_some_and(|viewer| settings.card_visible(tags, &settings.audience(&viewer, plan.directory.rooms_of(person))))
    };
    // Each line with its rank: new serious incidents first, the most severe first, then reminders.
    let mut lines: BTreeMap<To, Vec<(Rank, String, String)>> = BTreeMap::new();
    for pending in events {
        let event = &pending.event;
        let rank = match &event.kind {
            EventKind::Raised { severity, .. } => (0, std::cmp::Reverse(Some(*severity))),
            _ => (1, std::cmp::Reverse(None)),
        };
        let mut add = |to: To, line: String| {
            if pending.only.as_ref().is_none_or(|only| only.contains(&to)) {
                lines.entry(to).or_default().push((rank, line, event.key.clone()));
            }
        };
        match &event.kind {
            EventKind::Raised { card, severity, from } => {
                let Some(card) = plan.snapshot.card(*card) else { continue };
                let link = link_html(plan, card);
                let owners = owners(card);
                let raised = from.map(|from| format!(" (was {})", from.as_str())).unwrap_or_default();
                let line = format!(
                    "<strong>{}</strong>{raised}: {} · #{} · {owners}<br>{link}",
                    capitalized(severity.as_str()),
                    escape(title(card)),
                    card.number
                );
                for person in &managers {
                    if sees(*person, &card.tags) {
                        add(To::Person(*person), line.clone());
                    }
                }
                if settings.notifications.department_rooms {
                    let departments: Vec<String> = settings.departments_of(&card.tags).iter().map(|d| d.name.clone()).collect();
                    let notice = format!(
                        "<strong>{} ticket</strong> ({}): {} · #{}<br>{link}",
                        capitalized(severity.as_str()),
                        escape(&departments.join(", ")),
                        escape(title(card)),
                        card.number
                    );
                    for room in settings.notice_rooms(&card.tags) {
                        if plan.directory.is_member(plan.bot_id, room) {
                            add(To::Room(room), notice.clone());
                        }
                    }
                }
            }
            EventKind::StillNew { card, severity, minutes } => {
                let Some(card) = plan.snapshot.card(*card) else { continue };
                let line = format!(
                    "<strong>Still in New after {minutes} min</strong>: {} · {} · #{} · {}<br>{}",
                    capitalized(severity.as_str()),
                    escape(title(card)),
                    card.number,
                    owners(card),
                    link_html(plan, card)
                );
                for person in &managers {
                    if sees(*person, &card.tags) {
                        add(To::Person(*person), line.clone());
                    }
                }
            }
            EventKind::ProposalWaiting { id, managers: to_managers, minutes } => {
                let Some(proposal) = plan.proposals.iter().find(|proposal| &proposal.id == id && proposal.status == Status::Pending) else {
                    continue;
                };
                let tags = proposal_tags(proposal, plan.snapshot);
                let place = match (proposal.context.room_id, proposal.draft_message_id) {
                    (Some(room), Some(message)) => format!(r#"<a href="/rooms/{room}/@{message}">the draft</a>"#),
                    _ => format!(r#"<a href="/workspace/hermes#proposal-{}">the Sky tab</a>"#, escape(&proposal.id)),
                };
                if *to_managers {
                    let whom = match (&proposal.context.user_name, &proposal.context.room_name) {
                        (Some(person), Some(room)) => format!(" (for {} in {})", escape(person), escape(room)),
                        (Some(person), None) => format!(" (for {})", escape(person)),
                        (None, Some(room)) => format!(" (in {})", escape(room)),
                        (None, None) => String::new(),
                    };
                    let line = format!(
                        "<strong>Sky has been waiting {minutes} min</strong> for a confirmation: {}{whom} · {place}",
                        escape(&proposal.summary)
                    );
                    for person in &managers {
                        if sees(*person, &tags) {
                            add(To::Person(*person), line.clone());
                        }
                    }
                } else if let Some(person) = proposal.context.user_id {
                    let member = proposal.context.room_id.is_some_and(|room| plan.directory.is_member(person, room));
                    if member && sees(person, &tags) {
                        let line = format!(
                            "<strong>Sky is waiting for your confirmation</strong> ({minutes} min): {} · {place}",
                            escape(&proposal.summary)
                        );
                        add(To::Person(person), line);
                    }
                }
            }
            EventKind::HandoverDue { end } => {
                let zone = settings.handover.zone();
                let line = format!(
                    r#"<strong>Handover due</strong> (shift ending {}): <a href="{}">prepare it</a>"#,
                    crate::shifts::local_time(*end, &zone),
                    crate::handover::HANDOVER_PATH
                );
                for person in &managers {
                    add(To::Person(*person), line.clone());
                }
            }
            EventKind::Owner(ping) => {
                let Some(person) = ping.person.email.as_deref().and_then(|email| plan.directory.person_by_email(email)) else { continue };
                if ping.actor_user == Some(person) {
                    continue;
                }
                let Some(card) = plan.snapshot.card(ping.card) else { continue };
                add(To::Person(person), owner_line(ping, card, sees(person, &card.tags)));
            }
        }
    }
    lines
        .into_iter()
        .map(|(to, mut lines)| {
            // Stable: in the order they were found within a rank.
            lines.sort_by_key(|(rank, ..)| *rank);
            let mut keys: Vec<String> = Vec::new();
            for (_, _, key) in &lines {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
            let lines: Vec<String> = lines.into_iter().map(|(_, line, _)| line).collect();
            let count = lines.len();
            let mut html = String::new();
            if count > 1 {
                html.push_str(&format!("<p><strong>{count} alerts</strong> from the incident board</p>"));
            }
            for line in lines.iter().take(MAX_LINES) {
                html.push_str(&format!("<p>{line}</p>"));
            }
            if count > MAX_LINES {
                html.push_str(&format!(r#"<p>… and {} more: see <a href="/workspace">Home</a>.</p>"#, count - MAX_LINES));
            }
            Delivery { to, html, summary: format!("{count} alert(s)"), keys }
        })
        .collect()
}

/// An owner ping's line ([`crate::owners`]). `visible`: the recipient may see the card (else no
/// title and no link: the sheet would answer 404).
fn owner_line(ping: &OwnerPing, card: &Card, visible: bool) -> String {
    let number = card.number;
    let ticket = if visible { format!(r#"<a href="{}">#{number}</a>"#, crate::pages::sheet_path(number)) } else { format!("#{number}") };
    let actor = ping.actor.as_deref().map(escape);
    match &ping.change {
        OwnerChange::Added => {
            let lead = match &actor {
                Some(actor) => format!("<strong>{actor} made you owner</strong> of #{number}"),
                None => format!("<strong>You’re now the owner</strong> of #{number}"),
            };
            if visible {
                format!(r#"{lead} — {}<br><a href="{}">Open ticket #{number}</a>"#, escape(title(card)), crate::pages::sheet_path(number))
            } else {
                format!("{lead}. You can’t open it in Meshduty: ask a duty manager.")
            }
        }
        OwnerChange::Removed { now, taken } => {
            let names = escape(&now.join(", "));
            match (&actor, now.is_empty(), taken) {
                (Some(actor), false, true) => format!("<strong>{actor} took over</strong> {ticket} from you."),
                (Some(actor), false, false) => format!("<strong>{actor} gave</strong> {ticket} <strong>to {names}</strong>."),
                (None, false, _) => format!("{ticket} <strong>now belongs to {names}</strong>."),
                (Some(actor), true, _) => format!("<strong>{actor} removed you as owner</strong> of {ticket}."),
                (None, true, _) => format!("<strong>You’re no longer the owner</strong> of {ticket}."),
            }
        }
    }
}

/// The tags that decide who may see a proposal: its card's, or those it would give a new card.
pub fn proposal_tags(proposal: &Proposal, snapshot: &Snapshot) -> Vec<String> {
    match proposal.card {
        Some(number) => snapshot.card(number).map(|card| card.tags.clone()).unwrap_or_default(),
        None => proposal.new_card_tags(),
    }
}

fn link_html(plan: &Plan<'_>, card: &Card) -> String {
    let link = card_link(plan.config, plan.snapshot, card.number).unwrap_or_else(|| card.url.clone());
    format!(r#"<a href="{0}">{0}</a>"#, escape(&link))
}

fn owners(card: &Card) -> String {
    if card.assignees.is_empty() {
        "nobody assigned".into()
    } else {
        escape(&card.assignees.iter().map(|user| user.name.as_str()).collect::<Vec<_>>().join(", "))
    }
}

fn title(card: &Card) -> &str {
    let title = card.title.trim();
    if title.is_empty() { "Untitled" } else { title }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

// --- notified.json ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NotifiedFile {
    /// Event key → when it was sent (or recorded without sending, at the first poll).
    #[serde(default)]
    sent: BTreeMap<String, Timestamp>,
    /// The last handover posted (phase 2.6): the next one starts from there.
    #[serde(default)]
    handover_posted_at: Option<Timestamp>,
    #[serde(default)]
    handover_posted_by: Option<String>,
    /// The open cards' owners, as last known (owner pings, [`crate::owners`]); absent until the
    /// first poll with them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owners: Option<KnownOwners>,
}

/// `notified.json`: what was sent (kept 14 days), and when the last handover was posted.
#[derive(Debug)]
pub struct NotifiedStore {
    path: PathBuf,
    state: Mutex<NotifiedFile>,
    error: Mutex<Option<String>>,
}

impl NotifiedStore {
    /// A missing file is empty; one that can't be read is empty too (with the error, which the app
    /// logs): since the first poll records without sending, nothing old is sent again.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (state, error) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<NotifiedFile>(&bytes) {
                Ok(file) => (file, None),
                Err(error) => (NotifiedFile::default(), Some(format!("{} isn't valid: {error}", path.display()))),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (NotifiedFile::default(), None),
            Err(error) => (NotifiedFile::default(), Some(format!("couldn't read {}: {error}", path.display()))),
        };
        Self { path, state: Mutex::new(state), error: Mutex::new(error) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn contains(&self, key: &str) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sent.contains_key(key)
    }

    /// Records events as sent (one write), without sending them: the first poll, alerts off, and
    /// messages posted ([`crate::Workspace::settle_alerts`]).
    pub fn record<'a>(&self, keys: impl IntoIterator<Item = &'a str>, now: Timestamp) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let before = state.sent.len();
        for key in keys {
            state.sent.entry(key.to_string()).or_insert(now);
        }
        if state.sent.len() == before {
            return;
        }
        state.sent.retain(|_, at| now.duration_since(*at) <= KEEP_SENT);
        self.save(&state);
    }

    /// The open cards' owners, as last known.
    pub fn owners(&self) -> Option<KnownOwners> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).owners.clone()
    }

    /// Compares with the known owners and replaces them, in one go (a change from the sheet,
    /// [`NotifiedStore::update_owners`], can't slip in between). Written only when they changed.
    pub fn swap_owners<T>(&self, compare: impl FnOnce(Option<&KnownOwners>) -> (T, KnownOwners)) -> T {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (found, owners) = compare(state.owners.as_ref());
        if state.owners.as_ref() != Some(&owners) {
            state.owners = Some(owners);
            self.save(&state);
        }
        found
    }

    /// Changes the known owners (nothing while none are known: the next poll records them).
    pub fn update_owners(&self, change: impl FnOnce(&mut KnownOwners)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(owners) = state.owners.as_mut() else { return };
        change(owners);
        self.save(&state);
    }

    /// Keeps only the events not sent yet, and records them as sent (one write).
    pub fn claim(&self, events: Vec<Event>, now: Timestamp) -> Vec<Event> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let fresh: Vec<Event> = events.into_iter().filter(|event| !state.sent.contains_key(&event.key)).collect();
        if fresh.is_empty() {
            return fresh;
        }
        for event in &fresh {
            state.sent.insert(event.key.clone(), now);
        }
        state.sent.retain(|_, at| now.duration_since(*at) <= KEEP_SENT);
        self.save(&state);
        fresh
    }

    pub fn handover_posted(&self) -> Option<(Timestamp, Option<String>)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.handover_posted_at.map(|at| (at, state.handover_posted_by.clone()))
    }

    pub fn set_handover_posted(&self, at: Timestamp, by: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.handover_posted_at = Some(at);
        state.handover_posted_by = Some(by.to_string());
        self.save(&state);
    }

    fn save(&self, state: &NotifiedFile) {
        if let Err(error) = crate::store::write_json_atomically(&self.path, state) {
            *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("couldn't write {}: {error}", self.path.display()));
        }
    }

    /// The last read or write error, once (the app logs it).
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_are_validated() {
        let valid = Notifications { severities: vec!["High".into(), "critique".into(), "high".into()], ..Notifications::default() }
            .validated()
            .unwrap();
        assert_eq!(valid.severities, ["critical", "high"]);
        assert!(valid.alerts_on(Some(Severity::High)) && !valid.alerts_on(Some(Severity::Medium)) && !valid.alerts_on(None));
        assert!(Notifications { severities: vec!["urgent".into()], ..Notifications::default() }.validated().is_err());
        assert!(Notifications { new_reminder_min: 5000, ..Notifications::default() }.validated().is_err());
    }

    #[test]
    fn the_store_claims_each_event_once_across_restarts() {
        let dir = crate::store::scratch_dir("notified");
        let path = dir.join("notified.json");
        let now: Timestamp = "2026-09-30T09:00:00Z".parse().unwrap();
        let event = |key: &str| Event { key: key.into(), kind: EventKind::HandoverDue { end: now } };
        let store = NotifiedStore::open(&path);
        assert_eq!(store.claim(vec![event("a"), event("b")], now).len(), 2);
        assert!(store.claim(vec![event("a")], now).is_empty());
        let reopened = NotifiedStore::open(&path);
        assert!(reopened.contains("a") && reopened.contains("b"));
        assert_eq!(reopened.claim(vec![event("a"), event("c")], now), vec![event("c")]);
        // Forgotten after 14 days.
        let later = now + SignedDuration::from_hours(15 * 24);
        assert_eq!(reopened.claim(vec![event("d")], later).len(), 1);
        assert!(!reopened.contains("a") && reopened.contains("d"));
        reopened.set_handover_posted(now, "Karim");
        assert_eq!(NotifiedStore::open(&path).handover_posted(), Some((now, Some("Karim".into()))));

        std::fs::write(&path, "{ not json").unwrap();
        let broken = NotifiedStore::open(&path);
        assert!(broken.take_error().unwrap().contains("isn't valid"));
        assert!(!broken.contains("d"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
