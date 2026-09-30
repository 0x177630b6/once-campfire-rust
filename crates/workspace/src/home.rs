//! The read-only Home page (`GET /workspace`): what's waiting for the signed-in user.
//!
//! - **To confirm**: Hermes drafts in the user's rooms that nobody answered yet ([`drafts::pending`]).
//! - **Open incidents**: the incident board's cards that aren't closed, by column ("New" first,
//!   "Monitoring" last), most severe first, then oldest first.
//! - **Mentions**: comments on the incident board's cards that @mention the user (matched by email
//!   address), last 7 days.
//! - **Handover**: incident cards that changed in the last 12 hours, closed ones included.
//!
//! Every card links to Fizzy. The page renders from the last poll; while Fizzy is unreachable it
//! says so and shows what it last saw.

use std::cmp::Reverse;
use std::collections::HashSet;

use askama::Template;
use jiff::{SignedDuration, Timestamp};

use crate::cache::Snapshot;
use crate::chips::card_link;
use crate::config::WorkspaceConfig;
use crate::drafts::{self, Bot, ChatMessage, PendingDraft};
use crate::fizzy::{Card, CardState};

pub const HANDOVER_WINDOW: SignedDuration = SignedDuration::from_hours(12);
const MAX_MENTIONS: usize = 20;

/// Who's looking (or acting): a signed-in Campfire user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    /// A Campfire administrator (`Role::Administrator`): edits the settings, and is a duty manager
    /// while none are listed.
    pub administrator: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardItem {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub tone: &'static str,
    pub state: String,
    pub severity: Option<&'static str>,
    pub assignees: String,
    pub closed: bool,
    /// RFC 3339, for `<time>` (the layout's `local-time` controller formats it).
    pub created_at: Option<String>,
    pub last_active_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardGroup {
    pub label: String,
    pub tone: &'static str,
    pub cards: Vec<CardItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftItem {
    pub title: String,
    pub severity: Option<&'static str>,
    pub room_name: String,
    pub author_name: String,
    pub url: String,
    pub created_at: String,
    /// The draft's File / Edit / Dismiss buttons (Edit opens the message).
    pub buttons: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionItem {
    pub card_number: u64,
    pub card_title: String,
    pub card_url: String,
    pub author_name: String,
    pub board_name: String,
    pub excerpt: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FizzyStatus {
    Ok,
    /// Not polled successfully yet; `failed` once a poll failed (the error is in the server log
    /// only).
    Waiting {
        failed: bool,
    },
    /// The last poll failed; what's shown is from `since`.
    Stale {
        since: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/home.html")]
pub struct HomeView {
    pub viewer_name: String,
    pub to_confirm: Vec<DraftItem>,
    pub open: Vec<CardGroup>,
    pub open_count: usize,
    pub mentions: Vec<MentionItem>,
    pub handover: Vec<CardItem>,
    pub board_name: String,
    pub board_url: Option<String>,
    pub fizzy: FizzyStatus,
    pub retry_seconds: u64,
    /// `/workspace/board`
    pub board_page: String,
    /// Administrators only.
    pub settings_url: Option<String>,
}

pub fn build(
    config: &WorkspaceConfig,
    snapshot: &Snapshot,
    viewer: &Viewer,
    messages: &[ChatMessage],
    bots: &[Bot],
    now: Timestamp,
) -> HomeView {
    let fizzy = match (&snapshot.last_success, &snapshot.last_error) {
        (Some(_), None) => FizzyStatus::Ok,
        (Some(since), Some(_)) => FizzyStatus::Stale { since: since.to_string() },
        (None, error) => FizzyStatus::Waiting { failed: error.is_some() },
    };
    let board = snapshot.board.as_ref();
    let incident_cards: Vec<&Card> = snapshot.open.iter().filter_map(|n| snapshot.card(*n)).filter(|card| !card.closed).collect();
    HomeView {
        viewer_name: viewer.name.clone(),
        to_confirm: drafts::pending(messages, now).into_iter().filter_map(|pending| draft_item(pending, bots)).collect(),
        open_count: incident_cards.len(),
        open: groups(config, snapshot, &incident_cards),
        mentions: mentions(config, snapshot, viewer, now),
        handover: handover(config, snapshot, now),
        board_name: board.map(|board| board.name.clone()).unwrap_or_else(|| config.incident_board.clone()),
        board_url: board.and_then(|board| {
            let account = snapshot.account.as_deref()?;
            Some(format!("{}/{account}/boards/{}", config.link_base(), board.id))
        }),
        fizzy,
        retry_seconds: config.poll_interval.as_secs(),
        board_page: crate::pages::BOARD_PATH.into(),
        settings_url: viewer.administrator.then(|| crate::pages::SETTINGS_PATH.into()),
    }
}

fn draft_item(pending: PendingDraft, bots: &[Bot]) -> Option<DraftItem> {
    let message = pending.message;
    let bot = bots.iter().find(|bot| bot.id == message.creator_id)?;
    Some(DraftItem {
        title: pending.draft.title,
        severity: pending.draft.severity.map(|severity| severity.as_str()),
        room_name: message.room_name,
        author_name: message.creator_name,
        buttons: drafts::buttons(message.id, bot, Some(&message.url)),
        url: message.url,
        created_at: message.created_at.to_string(),
    })
}

pub fn card_item(config: &WorkspaceConfig, snapshot: &Snapshot, card: &Card) -> CardItem {
    let state = card.state();
    CardItem {
        number: card.number,
        title: if card.title.trim().is_empty() { "Untitled".into() } else { card.title.trim().to_string() },
        url: card_link(config, snapshot, card.number).unwrap_or_else(|| card.url.clone()),
        tone: state.tone(),
        state: state.label().to_string(),
        severity: card.severity().map(|severity| severity.as_str()),
        assignees: card.assignees.iter().map(|user| user.name.as_str()).collect::<Vec<_>>().join(", "),
        closed: card.closed,
        created_at: card.created_at.map(|at| at.to_string()),
        last_active_at: card.last_active_at.map(|at| at.to_string()),
    }
}

/// "New", then the board's columns in order, then any other column, then "Monitoring".
fn groups(config: &WorkspaceConfig, snapshot: &Snapshot, cards: &[&Card]) -> Vec<CardGroup> {
    let rank = |state: &CardState| match state {
        CardState::New => (0, 0),
        CardState::Column(name, _) => (1, snapshot.columns.iter().position(|column| &column.name == name).unwrap_or(usize::MAX)),
        CardState::Monitoring => (2, 0),
        CardState::Closed => (3, 0),
    };
    let mut sorted: Vec<&Card> = cards.to_vec();
    sorted.sort_by_key(|card| {
        let state = card.state();
        (rank(&state), state.label().to_string(), Reverse(card.severity()), card.created_at, card.number)
    });
    let mut groups: Vec<CardGroup> = Vec::new();
    for card in sorted {
        let item = card_item(config, snapshot, card);
        match groups.last_mut() {
            Some(group) if group.label == item.state => group.cards.push(item),
            _ => groups.push(CardGroup { label: item.state.clone(), tone: item.tone, cards: vec![item] }),
        }
    }
    groups
}

/// Matched by email address, which Campfire users can change without verification: someone who
/// takes a colleague's address sees their mentions. Hence incident-board cards only, whose content
/// the workspace shows every signed-in user anyway (docs/hermes-workspace.md).
fn mentions(config: &WorkspaceConfig, snapshot: &Snapshot, viewer: &Viewer, now: Timestamp) -> Vec<MentionItem> {
    let Some(board) = snapshot.board.as_ref() else { return Vec::new() };
    let Some(email) = viewer.email.as_deref().map(|email| email.trim().to_lowercase()).filter(|email| !email.is_empty()) else {
        return Vec::new();
    };
    let ids: HashSet<&str> = snapshot.emails.iter().filter(|(_, address)| **address == email).map(|(id, _)| id.as_str()).collect();
    snapshot
        .mentions
        .iter()
        .filter(|mention| mention.board_id == board.id)
        .filter(|mention| mention.mentioned.iter().any(|id| ids.contains(id.as_str())) && !ids.contains(mention.author_id.as_str()))
        .filter(|mention| now.duration_since(mention.created_at) <= crate::cache::MENTION_RETENTION)
        .take(MAX_MENTIONS)
        .map(|mention| MentionItem {
            card_number: mention.card_number,
            card_title: snapshot.card(mention.card_number).map(|card| card.title.clone()).unwrap_or_default(),
            card_url: card_link(config, snapshot, mention.card_number).unwrap_or_default(),
            author_name: mention.author_name.clone(),
            board_name: mention.board_name.clone(),
            excerpt: mention.excerpt.clone(),
            created_at: mention.created_at.to_string(),
        })
        .collect()
}

fn handover(config: &WorkspaceConfig, snapshot: &Snapshot, now: Timestamp) -> Vec<CardItem> {
    let mut cards: Vec<&Card> = snapshot
        .open
        .iter()
        .chain(&snapshot.recently_closed)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|number| snapshot.card(*number))
        .filter(|card| card.last_active_at.is_some_and(|at| now.duration_since(at) <= HANDOVER_WINDOW))
        .collect();
    cards.sort_by_key(|card| Reverse(card.last_active_at));
    cards.into_iter().map(|card| card_item(config, snapshot, card)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Mention;
    use crate::fizzy::{Board, BoardRef, Column, UserRef};

    fn config() -> WorkspaceConfig {
        WorkspaceConfig::from_lookup(|name| match name {
            "FIZZY_URL" => Some("http://fizzy".into()),
            "FIZZY_TOKEN" => Some("t".into()),
            "FIZZY_PUBLIC_URL" => Some("https://fizzy.example".into()),
            _ => None,
        })
        .unwrap()
        .unwrap()
    }

    fn now() -> Timestamp {
        "2026-09-30T09:00:00Z".parse().unwrap()
    }

    fn card(number: u64, title: &str, tags: &[&str], column: Option<&str>, hours_ago: i64) -> Card {
        Card {
            number,
            title: title.into(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            column: column.map(|name| Column { id: name.into(), name: name.into(), color: None }),
            board: Some(BoardRef { id: "b1".into(), name: "Incident Log".into() }),
            created_at: Some(now() - SignedDuration::from_hours(hours_ago + 1)),
            last_active_at: Some(now() - SignedDuration::from_hours(hours_ago)),
            ..Card::default()
        }
    }

    fn snapshot() -> Snapshot {
        let mut snapshot = Snapshot {
            account: Some("897".into()),
            board: Some(Board { id: "b1".into(), name: "Incident Log".into(), url: String::new() }),
            columns: vec![
                Column { id: "c1".into(), name: "In progress".into(), color: None },
                Column { id: "c2".into(), name: "Waiting on vendor".into(), color: None },
            ],
            last_success: Some(now()),
            ..Snapshot::default()
        };
        let mut closed = card(5, "Fire alarm, 3rd floor", &["sev-medium"], None, 2);
        closed.closed = true;
        let mut postponed = card(7, "Detector check", &[], None, 40);
        postponed.postponed = true;
        let mut assigned = card(9, "VIP arrival — suite 1501", &["sev-high"], Some("In progress"), 20);
        assigned.assignees = vec![UserRef { id: "u1".into(), name: "Maya".into(), email_address: None }];
        for card in [
            card(12, "Lift B out of service", &["sev-high"], None, 30),
            card(13, "Guest slip in lobby", &["sev-critical"], None, 1),
            card(11, "Noise complaint — room 1204", &["sev-medium"], Some("In progress"), 3),
            assigned,
            postponed,
            closed,
        ] {
            snapshot.cards.insert(card.number, card);
        }
        snapshot.open = vec![12, 13, 11, 9, 7];
        snapshot.recently_closed = vec![5];
        snapshot.emails.insert("fz-maya".into(), "maya@hotel.test".into());
        snapshot.emails.insert("fz-sophie".into(), "sophie@hotel.test".into());
        snapshot.mentions = vec![
            Mention {
                id: "a2".into(),
                card_number: 11,
                board_id: "b1".into(),
                board_name: "Incident Log".into(),
                author_id: "fz-sophie".into(),
                author_name: "Sophie".into(),
                excerpt: "@Maya 1204 wants a goodwill gesture".into(),
                created_at: now() - SignedDuration::from_mins(20),
                mentioned: vec!["fz-maya".into()],
            },
            Mention {
                id: "a1".into(),
                card_number: 12,
                board_id: "b1".into(),
                board_name: "Incident Log".into(),
                author_id: "fz-maya".into(),
                author_name: "Maya".into(),
                excerpt: "@Sophie can you call the lift company?".into(),
                created_at: now() - SignedDuration::from_mins(40),
                mentioned: vec!["fz-sophie".into()],
            },
        ];
        snapshot
    }

    fn maya() -> Viewer {
        Viewer { id: 5, name: "Maya".into(), email: Some("Maya@Hotel.test".into()), administrator: false }
    }

    #[test]
    fn open_incidents_by_column_most_severe_first() {
        let home = build(&config(), &snapshot(), &maya(), &[], &[], now());
        let groups: Vec<(&str, Vec<u64>)> =
            home.open.iter().map(|group| (group.label.as_str(), group.cards.iter().map(|card| card.number).collect())).collect();
        assert_eq!(groups, vec![("New", vec![13, 12]), ("In progress", vec![9, 11]), ("Monitoring", vec![7])]);
        assert_eq!(home.open_count, 5);
        assert_eq!(home.open[0].cards[0].url, "https://fizzy.example/897/cards/13");
        assert_eq!(home.open[1].cards[0].assignees, "Maya");
        assert_eq!(home.board_url.as_deref(), Some("https://fizzy.example/897/boards/b1"));
    }

    #[test]
    fn mentions_of_the_viewer_by_email() {
        let home = build(&config(), &snapshot(), &maya(), &[], &[], now());
        assert_eq!(home.mentions.len(), 1);
        assert_eq!((home.mentions[0].card_number, home.mentions[0].author_name.as_str()), (11, "Sophie"));
        assert_eq!(home.mentions[0].card_title, "Noise complaint — room 1204");
        let nobody = Viewer { email: None, ..maya() };
        assert!(build(&config(), &snapshot(), &nobody, &[], &[], now()).mentions.is_empty());
    }

    #[test]
    fn mentions_on_other_boards_are_not_shown() {
        let mut snapshot = snapshot();
        for mention in &mut snapshot.mentions {
            mention.board_id = "b2".into();
        }
        assert!(build(&config(), &snapshot, &maya(), &[], &[], now()).mentions.is_empty());
    }

    #[test]
    fn handover_is_the_last_twelve_hours() {
        let home = build(&config(), &snapshot(), &maya(), &[], &[], now());
        let numbers: Vec<u64> = home.handover.iter().map(|card| card.number).collect();
        assert_eq!(numbers, vec![13, 5, 11]);
        assert!(home.handover[1].closed && home.handover[1].state == "Closed");
    }

    #[test]
    fn drafts_to_confirm_need_a_known_bot() {
        let draft = "<p><b>Incident — AC leak — room 103</b></p><p>Reply <b>confirm</b> to file it in Fizzy.</p>";
        let message = ChatMessage {
            id: 42,
            room_id: 3,
            room_name: "front-desk".into(),
            url: "/rooms/3/@42".into(),
            creator_id: 9,
            creator_name: "Hermes".into(),
            creator_is_bot: true,
            created_at: now() - SignedDuration::from_mins(5),
            body_html: draft.into(),
        };
        let bots = [Bot { id: 9, name: "Hermes".into(), sgid: "sg".into() }];
        let home = build(&config(), &snapshot(), &maya(), std::slice::from_ref(&message), &bots, now());
        assert_eq!(home.to_confirm.len(), 1);
        assert_eq!(home.to_confirm[0].title, "Incident — AC leak — room 103");
        assert!(home.to_confirm[0].buttons.contains("/workspace/drafts/42/reply"));
        assert!(build(&config(), &snapshot(), &maya(), &[message], &[], now()).to_confirm.is_empty());
    }

    #[test]
    fn renders_every_section_and_escapes() {
        let mut snapshot = snapshot();
        snapshot.cards.get_mut(&13).unwrap().title = "<script>alert(1)</script>".into();
        let html = build(&config(), &snapshot, &maya(), &[], &[], now()).render().unwrap();
        for expected in ["To confirm", "Open incidents", "Mentions", "Handover", "ws-card ws-cc--new", "https://fizzy.example/897/cards/12"]
        {
            assert!(html.contains(expected), "{expected}");
        }
        assert!(html.contains("alert(1)") && !html.contains("<script>alert"));
    }

    #[test]
    fn says_when_fizzy_is_unreachable() {
        let mut snapshot = snapshot();
        snapshot.last_error = Some("could not reach Fizzy".into());
        let home = build(&config(), &snapshot, &maya(), &[], &[], now());
        assert!(matches!(home.fizzy, FizzyStatus::Stale { .. }));
        assert!(home.render().unwrap().contains("Boards unavailable"));
        let waiting = build(&config(), &Snapshot::default(), &maya(), &[], &[], now());
        assert!(matches!(waiting.fizzy, FizzyStatus::Waiting { failed: false }));
        assert!(waiting.open.is_empty());
        let failed = Snapshot { last_error: Some("could not reach Fizzy: connection refused (10.0.0.7:80)".into()), ..Snapshot::default() };
        let html = build(&config(), &failed, &maya(), &[], &[], now()).render().unwrap();
        assert!(html.contains("Boards unavailable") && !html.contains("10.0.0.7"), "the detail stays in the server log");
    }
}
