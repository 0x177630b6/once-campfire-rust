//! Hermes fork: the Duty Manager Workspace, phase 0 (docs/hermes-workspace.md). Fizzy's incident
//! cards made visible inside Campfire, read-only:
//!
//! - [`chips`]: a Fizzy card URL in a message renders as a live card chip;
//! - [`drafts`]: Hermes's incident drafts get File / Edit / Dismiss buttons;
//! - [`overlay`]: the phone tab bar (Home · Report · Chats) on every signed-in page;
//! - [`home`]: the Home page (to confirm, open incidents, mentions, handover);
//! - [`cache`] and [`fizzy`]: the in-memory picture of Fizzy, refreshed by polling its JSON API.
//!
//! This crate knows nothing of Campfire's own crates, so upstream merges can't break it and its
//! tests run anywhere (`cargo test -p campfire_workspace`). The app plugs it in through a thin
//! adapter (`crates/campfire/src/controllers/workspace.rs`) that provides what it needs from
//! Campfire: an [`HttpClient`](fizzy::HttpClient) for Fizzy, a [`ChatSource`] for the user's recent
//! messages, the bots ([`drafts::Bot`]), and the rendering hooks.

pub mod cache;
pub mod chips;
pub mod config;
pub mod drafts;
pub mod fizzy;
pub mod home;
mod html;
pub mod overlay;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use jiff::{SignedDuration, Timestamp};

pub use cache::Snapshot;
pub use config::{ConfigError, WorkspaceConfig};
pub use drafts::{Bot, ChatMessage};
pub use home::{HomeView, Viewer};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the Home page needs from Campfire's database.
pub trait ChatSource: Send + Sync {
    /// The messages of the viewer's rooms created since `since`, bodies included.
    fn recent_messages(&self, since: Timestamp) -> BoxFuture<'_, Result<Vec<ChatMessage>, String>>;
}

/// The workspace's state, shared by the poll task, the render hooks and the routes.
pub struct Workspace {
    config: WorkspaceConfig,
    snapshot: RwLock<Arc<Snapshot>>,
    /// Card numbers chips asked for that the workspace doesn't know yet.
    wanted: Mutex<BTreeSet<u64>>,
    poll_state: Mutex<Option<cache::PollState>>,
    bots: RwLock<Arc<Vec<Bot>>>,
}

impl Workspace {
    pub fn new(config: WorkspaceConfig) -> Self {
        Self {
            config,
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            wanted: Mutex::new(BTreeSet::new()),
            poll_state: Mutex::new(Some(cache::PollState::default())),
            bots: RwLock::new(Arc::new(Vec::new())),
        }
    }

    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
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
    /// never contains the token); a missing board is looked up again next time.
    pub async fn poll(&self, http: &dyn fizzy::HttpClient, now: Timestamp) -> Result<(), fizzy::FizzyError> {
        let client = fizzy::Client::new(http, &self.config);
        let previous = self.snapshot();
        let wanted = std::mem::take(&mut *self.wanted.lock().unwrap_or_else(|e| e.into_inner()));
        // Only the poll task polls; a concurrent call (a test) starts from a fresh state.
        let mut state = self.poll_state.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
        let result = cache::poll(&client, &previous, &self.config.incident_board, wanted, &mut state, now).await;
        *self.poll_state.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
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
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
        result.map(|_| ())
    }

    /// The render hook for a message body: card chips, and the draft buttons on a bot's draft.
    /// `None` leaves the body as it is.
    pub fn decorate_message(&self, message_id: i64, creator_id: i64, body_html: &str) -> Option<String> {
        let snapshot = self.snapshot();
        let chipped = chips::decorate(body_html, &self.config, &snapshot);
        let buttons =
            self.bot(creator_id).filter(|_| drafts::detect(body_html).is_some()).map(|bot| drafts::buttons(message_id, &bot, None));
        if let Some(unknown) = chipped.as_deref().map(unknown_cards) {
            self.want(unknown);
        }
        match (chipped, buttons) {
            (None, None) => None,
            (chipped, buttons) => Some(format!("{}{}", chipped.as_deref().unwrap_or(body_html), buttons.unwrap_or_default())),
        }
    }

    /// The chips of the cards the workspace knows among `numbers`; the others are fetched at the
    /// next poll.
    pub fn chips(&self, numbers: &[u64]) -> BTreeMap<u64, String> {
        let snapshot = self.snapshot();
        let mut chips = BTreeMap::new();
        let mut unknown = Vec::new();
        for &number in numbers {
            match (snapshot.card(number), chips::card_link(&self.config, &snapshot, number)) {
                (Some(card), Some(link)) => {
                    chips.insert(number, chips::chip(card, &link));
                }
                (Some(card), None) => {
                    chips.insert(number, chips::chip(card, &card.url));
                }
                (None, _) => unknown.push(number),
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
        Ok(home::build(&self.config, &self.snapshot(), viewer, &messages, &self.bots(), now))
    }
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
