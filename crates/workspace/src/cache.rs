//! The in-memory picture of Fizzy the workspace renders from, and the poll that refreshes it.
//!
//! Fizzy can't send webhooks to private addresses, so the app polls every `FIZZY_POLL_S`: the
//! incident board's open, "Not Now" and recently closed cards, its columns, page 1 of
//! `/activities` (card changes on every board, and comments that @mention people), the users those
//! mentions name (for their email address, once each), and the few cards chips asked for that
//! nothing else brought in. A poll that fails keeps the previous picture and records the error, so
//! chips and the Home page keep working (marked stale) while Fizzy is down.

use std::collections::{BTreeSet, HashMap, HashSet};

use jiff::{SignedDuration, Timestamp};
use serde_json::Value;

use crate::fizzy::{self, Board, Card, Client, Column, FizzyError};
use crate::html;

/// Mentions older than this leave the Home page.
pub const MENTION_RETENTION: SignedDuration = SignedDuration::from_hours(7 * 24);
const MAX_MENTIONS: usize = 300;
/// Cards chips asked for, fetched one by one: at most this many per poll.
const WANTED_PER_POLL: usize = 10;
const MAX_WANTED: usize = 200;
/// A card number Fizzy answered 404 for isn't asked for again before this.
const MISSING_RETRY: SignedDuration = SignedDuration::from_mins(30);
/// New users looked up per poll (for their email address).
const USERS_PER_POLL: usize = 20;
const MAX_ORIGINS: usize = 8;

/// A comment that @mentions people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// The activity's id.
    pub id: String,
    pub card_number: u64,
    pub board_name: String,
    pub author_id: String,
    pub author_name: String,
    /// The comment's text, cut.
    pub excerpt: String,
    pub created_at: Timestamp,
    /// Fizzy user ids.
    pub mentioned: Vec<String>,
}

/// What the last successful poll saw (plus the last error, if the latest poll failed).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The account slug (digits).
    pub account: Option<String>,
    pub board: Option<Board>,
    /// The incident board's columns, in board order.
    pub columns: Vec<Column>,
    /// Every card seen, by number, in its latest known state.
    pub cards: HashMap<u64, Card>,
    /// The incident board's open cards (in a column, "Maybe?" or "Not Now").
    pub open: Vec<u64>,
    /// Its most recently closed cards.
    pub recently_closed: Vec<u64>,
    /// Newest first.
    pub mentions: Vec<Mention>,
    /// Fizzy user id → email address (lowercase), for the users mentions name.
    pub emails: HashMap<String, String>,
    /// Where card URLs point, as Fizzy writes them (`BASE_URL`), e.g. `http://localhost:8484`.
    pub origins: Vec<String>,
    pub last_success: Option<Timestamp>,
    pub last_error: Option<String>,
    pub successful_polls: u64,
}

impl Snapshot {
    pub fn card(&self, number: u64) -> Option<&Card> {
        self.cards.get(&number)
    }

    /// Fizzy's answers are older than the last poll attempt.
    pub fn is_stale(&self) -> bool {
        self.last_error.is_some()
    }
}

/// What the poll keeps between runs that isn't shown.
#[derive(Debug, Default)]
pub(crate) struct PollState {
    /// Card numbers Fizzy answered 404 for, and when.
    missing: HashMap<u64, Timestamp>,
    /// Users whose lookup failed or who have no address.
    unknown_users: HashSet<String>,
}

pub(crate) async fn poll(
    client: &Client<'_>,
    previous: &Snapshot,
    incident_board: &str,
    wanted: BTreeSet<u64>,
    state: &mut PollState,
    now: Timestamp,
) -> Result<Snapshot, FizzyError> {
    let mut next = previous.clone();
    let account = match &previous.account {
        Some(account) => account.clone(),
        None => client.account().await?,
    };
    next.account = Some(account.clone());

    let board = match &previous.board {
        Some(board) => board.clone(),
        None => find_board(&client.boards(&account).await?, incident_board)
            .ok_or_else(|| FizzyError::Setup(format!("no Fizzy board named “{incident_board}” (WORKSPACE_INCIDENT_BOARD)")))?,
    };
    let columns = client.columns(&account, &board.id).await?;
    let open = client.board_cards(&account, &board.id, "", fizzy::MAX_PAGES).await?;
    let postponed = client.board_cards(&account, &board.id, "indexed_by=not_now", 3).await?;
    let closed = client.board_cards(&account, &board.id, "indexed_by=closed", 2).await?;
    let activities = client.activities(&account, if previous.successful_polls == 0 { 3 } else { 1 }).await?;

    next.open = open.iter().chain(&postponed).map(|card| card.number).collect();
    next.recently_closed = closed.iter().map(|card| card.number).collect();
    for card in open.into_iter().chain(postponed).chain(closed) {
        upsert(&mut next, card);
    }
    next.board = Some(board);
    next.columns = columns;

    let mut mentions = Vec::new();
    for activity in &activities {
        if activity.eventable_type.as_deref() == Some("Card")
            && let Ok(card) = serde_json::from_value::<Card>(activity.eventable.clone())
        {
            upsert(&mut next, card);
        }
        if let Some(mention) = mention_in(activity) {
            mentions.push(mention);
        }
    }
    merge_mentions(&mut next, mentions, now);

    // Cards chips asked for that nothing above brought in.
    let wanted: Vec<u64> = wanted
        .into_iter()
        .filter(|number| !next.cards.contains_key(number))
        .filter(|number| state.missing.get(number).is_none_or(|at| now.duration_since(*at) > MISSING_RETRY))
        .take(WANTED_PER_POLL)
        .collect();
    for number in wanted {
        match client.card(&account, number).await? {
            Some(card) => upsert(&mut next, card),
            None => {
                state.missing.insert(number, now);
            }
        }
    }
    // Cards mentions are about, for their titles.
    let untitled: Vec<u64> = next.mentions.iter().map(|m| m.card_number).filter(|n| !next.cards.contains_key(n)).collect();
    for number in untitled.into_iter().collect::<BTreeSet<_>>().into_iter().take(WANTED_PER_POLL) {
        if state.missing.get(&number).is_some_and(|at| now.duration_since(*at) <= MISSING_RETRY) {
            continue;
        }
        match client.card(&account, number).await? {
            Some(card) => upsert(&mut next, card),
            None => {
                state.missing.insert(number, now);
            }
        }
    }

    // The mentioned users' addresses, for matching them to Campfire users.
    let unknown: BTreeSet<String> = next
        .mentions
        .iter()
        .flat_map(|mention| mention.mentioned.iter())
        .filter(|id| !next.emails.contains_key(*id) && !state.unknown_users.contains(*id))
        .cloned()
        .collect();
    for id in unknown.into_iter().take(USERS_PER_POLL) {
        match client.user(&account, &id).await? {
            Some(user) if user.email_address.as_deref().is_some_and(|email| !email.trim().is_empty()) => {
                next.emails.insert(id, user.email_address.unwrap_or_default().trim().to_lowercase());
            }
            _ => {
                state.unknown_users.insert(id);
            }
        }
    }

    next.last_success = Some(now);
    next.last_error = None;
    next.successful_polls = previous.successful_polls + 1;
    Ok(next)
}

/// The board named `wanted` (case-insensitive) or with that id.
fn find_board(boards: &[Board], wanted: &str) -> Option<Board> {
    let wanted = wanted.trim();
    boards
        .iter()
        .find(|board| board.id == wanted)
        .or_else(|| boards.iter().find(|board| board.name.trim().eq_ignore_ascii_case(wanted)))
        .cloned()
}

fn upsert(snapshot: &mut Snapshot, card: Card) {
    if let Some(origin) = snapshot.account.as_deref().and_then(|account| origin_of(&card.url, account))
        && !snapshot.origins.contains(&origin)
        && snapshot.origins.len() < MAX_ORIGINS
    {
        snapshot.origins.push(origin);
    }
    snapshot.cards.insert(card.number, card);
}

/// `http://localhost:8484` in `http://localhost:8484/897362094/cards/12`.
fn origin_of(url: &str, account: &str) -> Option<String> {
    let at = url.find(&format!("/{account}/cards/"))?;
    let base = &url[..at];
    (base.starts_with("http://") || base.starts_with("https://")).then(|| base.to_string())
}

fn mention_in(activity: &fizzy::Activity) -> Option<Mention> {
    if activity.action != "comment_created" {
        return None;
    }
    let comment = &activity.eventable;
    let mentioned = fizzy::mentioned_user_ids(comment["body"]["html"].as_str().unwrap_or(""));
    if mentioned.is_empty() {
        return None;
    }
    let card_url = activity.url.as_deref().or_else(|| comment["card"]["url"].as_str()).unwrap_or("");
    let creator = activity.creator.clone().unwrap_or_default();
    Some(Mention {
        id: activity.id.clone(),
        card_number: fizzy::card_number_in(card_url)?,
        board_name: activity.board.as_ref().map(|board| board.name.clone()).unwrap_or_default(),
        author_id: creator.id,
        author_name: creator.name,
        excerpt: html::truncate(&comment["body"]["plain_text"].as_str().map(str::to_string).unwrap_or_else(|| plain(comment)), 280),
        created_at: activity.created_at?,
        mentioned,
    })
}

fn plain(comment: &Value) -> String {
    html::to_text(comment["body"]["html"].as_str().unwrap_or("")).replace('\n', " ")
}

fn merge_mentions(snapshot: &mut Snapshot, new: Vec<Mention>, now: Timestamp) {
    for mention in new {
        if !snapshot.mentions.iter().any(|known| known.id == mention.id) {
            snapshot.mentions.push(mention);
        }
    }
    snapshot.mentions.retain(|mention| now.duration_since(mention.created_at) <= MENTION_RETENTION);
    snapshot.mentions.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
    snapshot.mentions.truncate(MAX_MENTIONS);
}

/// Adds card numbers chips asked for, bounded.
pub(crate) fn want(wanted: &mut BTreeSet<u64>, numbers: impl IntoIterator<Item = u64>) {
    for number in numbers {
        if wanted.len() >= MAX_WANTED {
            break;
        }
        wanted.insert(number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_board_by_id_or_name() {
        let boards = vec![
            Board { id: "03a".into(), name: "Engineering".into(), url: String::new() },
            Board { id: "03b".into(), name: "Incident Log".into(), url: String::new() },
        ];
        assert_eq!(find_board(&boards, "incident log").unwrap().id, "03b");
        assert_eq!(find_board(&boards, "03a").unwrap().name, "Engineering");
        assert!(find_board(&boards, "Security").is_none());
    }

    #[test]
    fn origins_come_from_card_urls() {
        assert_eq!(origin_of("http://localhost:8484/897/cards/12", "897").as_deref(), Some("http://localhost:8484"));
        assert_eq!(origin_of("https://h/fizzy/897/cards/1", "897").as_deref(), Some("https://h/fizzy"));
        assert_eq!(origin_of("/897/cards/12", "897"), None);
        assert_eq!(origin_of("http://h/1/cards/12", "897"), None);
    }
}
