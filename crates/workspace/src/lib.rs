//! Hermes fork: the Duty Manager Workspace, phases 0 and 1 (docs/hermes-workspace.md). Fizzy's
//! incident cards inside Campfire:
//!
//! - [`chips`]: a Fizzy card URL in a message renders as a live card chip;
//! - [`drafts`]: Hermes's incident drafts get File / Edit / Dismiss buttons;
//! - [`overlay`]: the phone tab bar (Home · Report · Boards · Chats) on every signed-in page, and a
//!   linked room's cards panel;
//! - [`home`]: the Home page (to confirm, open incidents, mentions, handover);
//! - [`pages`]: the board, the card sheet, the room panel, the new-card form, the settings page;
//! - [`actions`] and [`writes`]: working cards (move, close, severity, departments, steps,
//!   comments, create), through an acting identity, audited, policy-checked;
//! - [`settings`]: departments, duty managers, the policy and Hermes's autonomy, in a JSON file;
//! - [`cache`] and [`fizzy`]: the in-memory picture of Fizzy, refreshed by polling its JSON API;
//! - phase 2, supervising Hermes: [`journal`] (the durable write log), [`hermes_log`] (what Hermes
//!   did, direct or through Campfire), [`proposals`] (Hermes asks, Campfire decides by the dial) and
//!   [`hermes`] (the Hermes tab, proposals' decisions, undo); [`alerts`] (critical incidents and
//!   reminders, as Hermes's direct messages), [`handover`] (the end-of-shift summary, in
//!   [`shifts`]) and [`visibility`] (restricted departments' cards).
//!
//! This crate knows nothing of Campfire's own crates, so upstream merges can't break it and its
//! tests run anywhere (`cargo test -p campfire_workspace`). The app plugs it in through a thin
//! adapter (`crates/campfire/src/controllers/workspace.rs`) that provides what it needs from
//! Campfire: an [`HttpClient`](fizzy::HttpClient) for Fizzy, a [`ChatSource`] for the user's recent
//! messages, the bots ([`drafts::Bot`]), and the rendering hooks.

pub mod actions;
pub mod alerts;
pub mod cache;
pub mod chips;
pub mod config;
pub mod drafts;
pub mod fizzy;
pub mod handover;
pub mod hermes;
pub mod hermes_log;
pub mod home;
mod html;
pub mod journal;
pub mod overlay;
pub mod pages;
pub mod proposals;
pub mod settings;
pub mod shifts;
pub mod sky;
mod store;
pub mod visibility;
pub mod writes;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use jiff::{SignedDuration, Timestamp};

use fizzy::Card;

pub use cache::Snapshot;
pub use config::{ConfigError, WorkspaceConfig};
pub use drafts::{Bot, ChatMessage};
pub use home::{HomeView, Viewer};
pub use settings::{Act, Policy, Settings, SettingsStore};
pub use visibility::{Audience, Directory};
pub use writes::{ActionError, TokenSource, WriteRecord};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the Home page needs from Campfire's database.
pub trait ChatSource: Send + Sync {
    /// The messages of the viewer's rooms created since `since`, bodies included.
    fn recent_messages(&self, since: Timestamp) -> BoxFuture<'_, Result<Vec<ChatMessage>, String>>;
    /// The rooms the viewer is a member of (a proposal's details are for its room's members and
    /// the duty managers).
    fn room_ids(&self) -> BoxFuture<'_, Result<Vec<i64>, String>>;
}

/// The workspace's state, shared by the poll task, the render hooks and the routes.
pub struct Workspace {
    config: WorkspaceConfig,
    snapshot: RwLock<Arc<Snapshot>>,
    /// Card numbers chips asked for that the workspace doesn't know yet.
    wanted: Mutex<BTreeSet<u64>>,
    poll_state: Mutex<Option<cache::PollState>>,
    bots: RwLock<Arc<Vec<Bot>>>,
    settings: SettingsStore,
    /// Who writes to Fizzy for whom.
    tokens: Box<dyn TokenSource>,
    /// Every write, for the log.
    audit: Box<writes::Audit>,
    /// One lock per card being written to ([`Workspace::lock_card`]); an entry lives only while
    /// someone holds or waits for it.
    card_locks: CardLocks,
    /// Every write, durably (`actions.jsonl`).
    journal: journal::ActionLog,
    /// What Hermes did (`hermes-log.jsonl`).
    hermes_log: hermes_log::HermesLog,
    /// Hermes's proposals (`proposals.json`).
    proposals: proposals::ProposalStore,
    /// The Fizzy users behind the tokens, once learned.
    fizzy_users: RwLock<hermes::FizzyUsers>,
    /// People, rooms and memberships, as the app last read them (phase 2.5 and 2.7).
    directory: RwLock<Arc<Directory>>,
    /// What alerts were sent (`notified.json`), and the last handover.
    notified: alerts::NotifiedStore,
    /// When the first poll since the start ran: it records alerts without sending them.
    alerts_primed: Mutex<Option<Timestamp>>,
    /// The picture the last successful poll produced, as the alerts saw it. Not the live picture:
    /// [`Workspace::remember`] puts what Campfire writes or reads into that one at once, and a card
    /// created or raised through Campfire must still count as new or raised at the next poll.
    alerts_seen: Mutex<Option<Arc<Snapshot>>>,
    /// Alerts detected and claimed, waiting for the app to deliver them.
    outbox: Mutex<Vec<alerts::Event>>,
    clock: Box<dyn Fn() -> Timestamp + Send + Sync>,
    /// "Show earlier comments" reads at once ([`actions::ALL_COMMENTS_AT_ONCE`]): each walks up to a
    /// dozen Fizzy pages, and the app is reachable from the internet.
    pub(crate) all_comments: tokio::sync::Semaphore,
}

type CardLocks = Mutex<HashMap<u64, Arc<tokio::sync::Mutex<()>>>>;

/// Held while a sequence of writes to one card runs; dropping it lets the next one in, and
/// removes the card's lock once nobody waits for it.
pub(crate) struct CardLock<'a> {
    locks: &'a CardLocks,
    number: u64,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for CardLock<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        // Every holder or waiter has a clone, taken under this map's lock: only the map's is left.
        if locks.get(&self.number).is_some_and(|lock| Arc::strong_count(lock) == 1) {
            locks.remove(&self.number);
        }
    }
}

impl Workspace {
    /// Reads the settings file (`config.settings_path`) and, next to it, the write log, the Hermes
    /// log and the proposals; people's writes go through `FIZZY_TOKEN`.
    pub fn new(config: WorkspaceConfig) -> Self {
        let settings = SettingsStore::open(&config.settings_path);
        let tokens = Box::new(writes::SharedToken(config.token.clone()));
        let journal = journal::ActionLog::open(config.storage_file("actions.jsonl"));
        let hermes_log = hermes_log::HermesLog::open(config.storage_file("hermes-log.jsonl"), Timestamp::now());
        let proposals = proposals::ProposalStore::open(config.storage_file("proposals.json"));
        let notified = alerts::NotifiedStore::open(config.storage_file("notified.json"));
        Self {
            config,
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            wanted: Mutex::new(BTreeSet::new()),
            poll_state: Mutex::new(Some(cache::PollState::default())),
            bots: RwLock::new(Arc::new(Vec::new())),
            settings,
            tokens,
            audit: Box::new(|_| {}),
            card_locks: Mutex::new(HashMap::new()),
            journal,
            hermes_log,
            proposals,
            fizzy_users: RwLock::new(hermes::FizzyUsers::default()),
            directory: RwLock::new(Arc::new(Directory::default())),
            notified,
            alerts_primed: Mutex::new(None),
            alerts_seen: Mutex::new(None),
            outbox: Mutex::new(Vec::new()),
            clock: Box::new(Timestamp::now),
            all_comments: tokio::sync::Semaphore::new(actions::ALL_COMMENTS_AT_ONCE),
        }
    }

    /// The time writes and the Hermes log are stamped with (tests fix it).
    pub fn with_clock(mut self, clock: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn now(&self) -> Timestamp {
        (self.clock)()
    }

    /// One write: into the durable write log, then to the app's audit sink (the server log).
    pub fn record(&self, record: &WriteRecord) {
        self.journal.append(record);
        (self.audit)(record);
    }

    pub fn journal(&self) -> &journal::ActionLog {
        &self.journal
    }

    pub fn hermes_log(&self) -> &hermes_log::HermesLog {
        &self.hermes_log
    }

    pub fn proposals(&self) -> &proposals::ProposalStore {
        &self.proposals
    }

    /// The storage errors since the last call (write log, Hermes log, proposals), for the app to
    /// log.
    pub fn take_storage_errors(&self) -> Vec<String> {
        [self.journal.take_error(), self.hermes_log.take_error(), self.proposals.take_error(), self.notified.take_error()]
            .into_iter()
            .flatten()
            .collect()
    }

    /// Waits until nobody else is writing to card `number`, then holds it until the returned
    /// guard is dropped. Tag changes are read-diff-toggle sequences over Fizzy's toggles: two of
    /// them interleaved on one card could put back a tag the other removed.
    pub(crate) async fn lock_card(&self, number: u64) -> CardLock<'_> {
        let lock = self.card_locks.lock().unwrap_or_else(|e| e.into_inner()).entry(number).or_default().clone();
        let guard = lock.lock_owned().await;
        CardLock { locks: &self.card_locks, number, guard: Some(guard) }
    }

    /// Where every write's [`WriteRecord`] goes (the app logs them).
    pub fn with_audit(mut self, audit: impl Fn(&WriteRecord) + Send + Sync + 'static) -> Self {
        self.audit = Box::new(audit);
        self
    }

    /// Another source of write tokens (per-person tokens, later).
    pub fn with_tokens(mut self, tokens: impl TokenSource + 'static) -> Self {
        self.tokens = Box::new(tokens);
        self
    }

    pub fn with_settings(mut self, settings: SettingsStore) -> Self {
        self.settings = settings;
        self
    }

    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
    }

    pub fn settings(&self) -> Arc<Settings> {
        self.settings.get()
    }

    pub fn settings_store(&self) -> &SettingsStore {
        &self.settings
    }

    /// A card as Fizzy just returned it (after a write, or read for a sheet): into the picture at
    /// once, in the right list. A card on another board is dropped.
    pub fn remember(&self, card: Card) {
        let now = self.now();
        let mut snapshot = self.snapshot.write().unwrap_or_else(|e| e.into_inner());
        let mut next = (**snapshot).clone();
        let number = card.number;
        next.open.retain(|open| *open != number);
        next.recently_closed.retain(|closed| *closed != number);
        match next.board.clone() {
            Some(board) if card.board.as_ref().is_some_and(|on| on.id == board.id) => {
                if card.closed {
                    next.recently_closed.insert(0, number);
                } else {
                    next.open.push(number);
                }
                next.cards.insert(number, card);
                next.refreshed.insert(number, now);
            }
            _ => {
                next.cards.remove(&number);
                next.refreshed.remove(&number);
            }
        }
        *snapshot = Arc::new(next);
    }

    /// The latest picture of Fizzy.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The active bots, which can post drafts.
    pub fn set_bots(&self, bots: Vec<Bot>) {
        *self.bots.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(bots);
    }

    pub fn bots(&self) -> Arc<Vec<Bot>> {
        self.bots.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn bot(&self, id: i64) -> Option<Bot> {
        self.bots().iter().find(|bot| bot.id == id).cloned()
    }

    /// One poll of Fizzy. On failure the previous picture stays, marked with the error (which
    /// never contains the token); a missing board is looked up again next time. A single card or
    /// user lookup that fails isn't a failure: it's in [`Snapshot::lookup_errors`] and retried.
    pub async fn poll(&self, http: &dyn fizzy::HttpClient, now: Timestamp) -> Result<(), fizzy::FizzyError> {
        let client = fizzy::Client::new(http, &self.config);
        let previous = self.snapshot();
        let wanted = std::mem::take(&mut *self.wanted.lock().unwrap_or_else(|e| e.into_inner()));
        // Only the poll task polls; a concurrent call (a test) starts from a fresh state.
        let mut state = self.poll_state.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
        let result = cache::poll(&client, &previous, &self.config.incident_board, wanted, &mut state, now).await;
        *self.poll_state.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
        let result = match result {
            Ok(mut next) => {
                // Hermes's own actions, for its log; a failure here doesn't fail the poll.
                if let Err(error) = self.poll_hermes(&client, &next, now).await {
                    next.lookup_errors.push(format!("Sky's activities: {error}"));
                }
                Ok(next)
            }
            Err(error) => Err(error),
        };
        self.expire_proposals(now);
        let next = match &result {
            Ok(next) => next.clone(),
            Err(error) => {
                let mut kept = (*previous).clone();
                kept.last_error = Some(error.to_string());
                if matches!(error, fizzy::FizzyError::Status(404) | fizzy::FizzyError::Setup(_)) {
                    kept.board = None;
                }
                kept
            }
        };
        let next = Arc::new(next);
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = next.clone();
        if result.is_ok() {
            self.detect_alerts(&next, now);
        }
        result.map(|_| ())
    }

    /// Phase 2.5: what should alert after this poll, claimed in `notified.json` (so never twice)
    /// and queued for the app ([`Workspace::take_alerts`]). The first poll since the start, and any
    /// poll while alerts are off, only records. Compared with the previous poll's picture (never the live one, which
    /// [`Workspace::remember`] updates between polls).
    fn detect_alerts(&self, next: &Arc<Snapshot>, now: Timestamp) {
        let mut primed = self.alerts_primed.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = self.alerts_seen.lock().unwrap_or_else(|e| e.into_inner());
        let previous = seen.replace(next.clone()).unwrap_or_default();
        // While alerts are off, what they would send is recorded all the same, and not sent: turning
        // them back on doesn't send what happened meanwhile.
        let mut settings = (*self.settings()).clone();
        let enabled = std::mem::replace(&mut settings.notifications.enabled, true);
        let events = alerts::detect(&previous, next, &settings, &self.proposals.all(), now, *primed);
        let fresh = self.notified.claim(events, now);
        match *primed {
            None => *primed = Some(now),
            Some(_) if enabled => self.outbox.lock().unwrap_or_else(|e| e.into_inner()).extend(fresh),
            Some(_) => {}
        }
    }

    /// The alerts detected since the last call, as messages: one per person or room, posted by
    /// `bot_id` (Hermes's bot). `None`: no bot to post them; they're dropped (and counted).
    pub fn take_alerts(&self, bot_id: Option<i64>) -> Result<Vec<alerts::Delivery>, usize> {
        let events = std::mem::take(&mut *self.outbox.lock().unwrap_or_else(|e| e.into_inner()));
        let Some(bot_id) = bot_id else { return if events.is_empty() { Ok(Vec::new()) } else { Err(events.len()) } };
        let (snapshot, settings, directory, proposals) = (self.snapshot(), self.settings(), self.directory(), self.proposals.all());
        let plan = alerts::Plan {
            config: &self.config,
            snapshot: &snapshot,
            settings: &settings,
            directory: &directory,
            proposals: &proposals,
            bot_id,
        };
        Ok(alerts::plan(&events, &plan))
    }

    pub fn notified(&self) -> &alerts::NotifiedStore {
        &self.notified
    }

    /// People, rooms and memberships (the app reads them every poll).
    pub fn set_directory(&self, directory: Directory) {
        *self.directory.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Directory { loaded: true, ..directory });
    }

    pub fn directory(&self) -> Arc<Directory> {
        self.directory.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The rooms a user is in, as a request just read them (fresher than the last poll's).
    pub fn set_rooms_of(&self, user_id: i64, rooms: impl IntoIterator<Item = i64>) {
        let mut directory = self.directory.write().unwrap_or_else(|e| e.into_inner());
        let mut next = (**directory).clone();
        next.memberships.insert(user_id, rooms.into_iter().collect());
        *directory = Arc::new(next);
    }

    /// What `viewer` may see (phase 2.7), from the rooms they're a member of.
    pub fn audience_of(&self, viewer: &Viewer) -> Audience {
        self.settings().audience(viewer, self.directory().rooms_of(viewer.id))
    }

    /// Whether `audience` may see card `number` of the picture. A card the picture doesn't have is
    /// hidden while visibility is restricted (it can't be checked).
    pub fn sees_card(&self, audience: &Audience, number: u64) -> bool {
        let settings = self.settings();
        if audience.all || !settings.visibility.restricts() {
            return true;
        }
        self.snapshot().card(number).is_some_and(|card| settings.card_visible(&card.tags, audience))
    }

    /// The render hook for a message body: card chips, and the draft buttons on a bot's draft.
    /// `None` leaves the body as it is.
    pub fn decorate_message(&self, message_id: i64, creator_id: i64, body_html: &str) -> Option<String> {
        let snapshot = self.snapshot();
        // Message HTML is cached and shared by every viewer: while visibility is restricted, the
        // links are only marked, and the page fills them in from `cards.json`, per viewer.
        let fill = !self.settings().visibility.restricts();
        let chipped = chips::decorate(body_html, &self.config, &snapshot, fill);
        let buttons = self.bot(creator_id).and_then(|bot| match proposals::marker_in(body_html) {
            // A proposal's draft: its buttons decide that proposal. A marker alone does nothing:
            // only the message Campfire posted for it, by its bot, gets them.
            Some(id) => self
                .proposals
                .get(&id)
                .filter(|proposal| proposal.bot_id == bot.id && proposal.draft_message_id == Some(message_id))
                .map(|proposal| drafts::proposal_buttons(&proposal, &bot, drafts::Place::Room)),
            None => drafts::detect(body_html).is_some().then(|| drafts::buttons(message_id, &bot, None)),
        });
        if let Some(unknown) = chipped.as_deref().map(unknown_cards) {
            self.want(unknown.into_iter().filter(|number| snapshot.card(*number).is_none()).collect());
        }
        match (chipped, buttons) {
            (None, None) => None,
            (chipped, buttons) => Some(format!("{}{}", chipped.as_deref().unwrap_or(body_html), buttons.unwrap_or_default())),
        }
    }

    /// The chips `viewer` may see among `numbers` (phase 2.7); a hidden card gets none, like an
    /// unknown one.
    pub fn chips_for(&self, viewer: &Viewer, numbers: &[u64]) -> BTreeMap<u64, String> {
        let audience = self.audience_of(viewer);
        let mut chips = self.chips(numbers);
        chips.retain(|number, _| self.sees_card(&audience, *number));
        chips
    }

    /// A card's chip for `viewer`, for the page to swap in after a change.
    pub fn chip_for(&self, viewer: &Viewer, number: u64) -> Option<String> {
        self.chips_for(viewer, &[number]).remove(&number)
    }

    /// The chips of the incident-board cards the workspace knows among `numbers`; the others are
    /// fetched at the next poll (and a card on another board never gets one). Everyone's: the
    /// routes use [`Workspace::chips_for`].
    pub(crate) fn chips(&self, numbers: &[u64]) -> BTreeMap<u64, String> {
        let snapshot = self.snapshot();
        let mut chips = BTreeMap::new();
        let mut unknown = Vec::new();
        for &number in numbers {
            match snapshot.card(number) {
                Some(card) => {
                    chips.insert(number, chips::chip(card));
                }
                None => unknown.push(number),
            }
        }
        self.want(unknown);
        chips
    }

    fn want(&self, numbers: Vec<u64>) {
        if !numbers.is_empty() {
            cache::want(&mut self.wanted.lock().unwrap_or_else(|e| e.into_inner()), numbers);
        }
    }

    /// The Home page for `viewer`.
    pub async fn home(&self, viewer: &Viewer, source: &dyn ChatSource, now: Timestamp) -> Result<HomeView, String> {
        let messages = source.recent_messages(now - drafts::DRAFT_TTL - SignedDuration::from_mins(5)).await?;
        let rooms = source.room_ids().await?;
        self.set_rooms_of(viewer.id, rooms.iter().copied());
        let (snapshot, settings) = (self.snapshot(), self.settings());
        let mut view = home::build(&self.config, &snapshot, viewer, &messages, &self.bots(), now);
        let audience = self.audience_of(viewer);
        view.retain_cards(|number| self.sees_card(&audience, number));
        view.proposals = self.pending_items(viewer, &rooms);
        if settings.is_duty_manager(viewer) {
            view.handover_url = Some(handover::HANDOVER_PATH.into());
            // Decision D3: cards with no department, for the duty managers to sort.
            if !settings.departments.is_empty() {
                view.no_department = snapshot
                    .open
                    .iter()
                    .filter_map(|number| snapshot.card(*number))
                    .filter(|card| !card.closed && settings.departments_of(&card.tags).is_empty())
                    .map(home::card_item)
                    .collect();
            }
        }
        Ok(view)
    }

    /// Whether `viewer` gets links to Fizzy on the pages (`fizzy_links`): duty managers and
    /// administrators, and only when `lan` (the adapter's call: the request didn't come through
    /// Campfire's public address, [`WorkspaceConfig::is_public_request`]). Everyone else stays in
    /// the app: most people have no Fizzy account, and Fizzy isn't reachable from outside.
    pub fn fizzy_links(&self, viewer: &Viewer, lan: bool) -> bool {
        lan && (viewer.administrator || self.settings().is_duty_manager(viewer))
    }

    /// The board page, from the last poll.
    pub fn board(&self, viewer: &Viewer, filter: &pages::BoardFilter) -> pages::BoardView {
        let (can_change, can_create) = (self.may(&Act::ChangeCard, viewer), self.may(&Act::CreateCard, viewer));
        let (settings, audience) = (self.settings(), self.audience_of(viewer));
        let visible = |card: &Card| settings.card_visible(&card.tags, &audience);
        let viewing = pages::Viewing { visible: &visible, administrator: viewer.administrator, can_change, can_create };
        pages::board(&self.config, &self.snapshot(), &settings, filter, &viewing)
    }

    /// A room's cards panel; `None` when the room isn't linked to a department.
    pub fn room_panel(&self, viewer: &Viewer, room_id: i64) -> Option<pages::RoomPanel> {
        let (settings, audience) = (self.settings(), self.audience_of(viewer));
        let visible = |card: &Card| settings.card_visible(&card.tags, &audience);
        pages::room_panel(&self.config, &self.snapshot(), &settings, room_id, &visible, self.may(&Act::CreateCard, viewer))
    }

    /// The new-card form, prefilled from a message's text and its room's department.
    pub fn new_card_form(&self, room_id: Option<i64>, message_text: Option<&str>, source: Option<pages::FormSource>) -> pages::NewCardForm {
        pages::new_card_form(&self.config, &self.snapshot(), &self.settings(), room_id, message_text, source)
    }

    /// A card's chip, whoever looks (tests; the routes use [`Workspace::chip_for`]).
    #[cfg(test)]
    pub(crate) fn chip(&self, number: u64) -> Option<String> {
        self.chips(&[number]).remove(&number)
    }
}

/// The text a reader sees in a message body (for prefilling a card from it).
pub fn text_of(body_html: &str) -> String {
    html::to_text(body_html)
}

/// Card numbers of the links [`chips::decorate`] marked but couldn't fill.
fn unknown_cards(html: &str) -> Vec<u64> {
    html.match_indices("<a data-ws-card=\"")
        .filter_map(|(at, marker)| {
            let rest = &html[at + marker.len()..];
            rest[..rest.find('"')?].parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests;
