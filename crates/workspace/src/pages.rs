//! Phase 1's pages and fragments, built from the last poll (lists) or a fresh read (the card
//! sheet):
//!
//! - [`BoardView`] (`GET /workspace/board`): the incident board's columns, "New" (Fizzy's
//!   "Maybe?") first, then the board's columns, "Monitoring" ("Not Now") and "Closed" (recently
//!   closed); filters by department tag and severity; a Move menu on each card. On phones one
//!   column at a time, with a column switcher.
//! - [`CardSheet`] (`GET /workspace/cards/:n`, and the overlay the chips, the panel and the board
//!   open): title, severity, departments, column, owners, steps, the comment thread, a comment box.
//! - [`RoomPanel`]: in a room linked to departments, the open cards tagged with them, most severe
//!   first.
//! - [`NewCardForm`] (`GET /workspace/cards/new`): "Create a card from this message".
//! - [`SettingsPage`] (`GET /workspace/settings`, administrators): departments, duty managers,
//!   policy.
//!
//! Fizzy's HTML is never shown as is: descriptions and comments are rendered from their text.

use std::cmp::Reverse;

use askama::Template;
use serde_json::{Value, json};

use crate::cache::Snapshot;
use crate::chips::card_link;
use crate::config::WorkspaceConfig;
use crate::fizzy::{Card, CardState, Column, Comment, Severity};
use crate::home::FizzyStatus;
use crate::html;
use crate::settings::{Policy, Settings};

pub const BOARD_PATH: &str = "/workspace/board";
pub const SETTINGS_PATH: &str = "/workspace/settings";
pub const NEW_CARD_PATH: &str = "/workspace/cards/new";
pub const CARDS_PATH: &str = "/workspace/cards";
/// The most recently closed cards the board shows.
const MAX_CLOSED: usize = 30;

/// The card sheet (page, or overlay fragment).
pub fn sheet_path(number: u64) -> String {
    format!("/workspace/cards/{number}")
}

/// `GET`: a room's cards panel (fragment).
pub fn panel_path(room_id: i64) -> String {
    format!("/workspace/rooms/{room_id}/panel")
}

/// An option of a select, radio group or checkbox list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

impl Choice {
    fn new(value: impl Into<String>, label: impl Into<String>, selected: bool) -> Self {
        Self { value: value.into(), label: label.into(), selected }
    }
}

/// Where a card is, as a move target: `new`, `column:<id>`, `not_now`, `closed`.
pub fn state_key(card: &Card) -> String {
    match card.state() {
        CardState::New => "new".into(),
        CardState::Column(..) => format!("column:{}", card.column.as_ref().map(|column| column.id.as_str()).unwrap_or("")),
        CardState::Monitoring => "not_now".into(),
        CardState::Closed => "closed".into(),
    }
}

/// Every place a card can go, its current one marked.
pub fn move_choices(card: &Card, columns: &[Column]) -> Vec<Choice> {
    let current = state_key(card);
    let mut choices = vec![Choice::new("new", "New", current == "new")];
    for column in columns {
        let key = format!("column:{}", column.id);
        choices.push(Choice::new(key.clone(), column.name.clone(), current == key));
    }
    choices.push(Choice::new("not_now", "Monitoring (not now)", current == "not_now"));
    choices.push(Choice::new("closed", "Closed", current == "closed"));
    choices
}

/// A card in a list (board column, room panel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCard {
    pub number: u64,
    pub title: String,
    pub tone: &'static str,
    pub state: String,
    pub severity: Option<&'static str>,
    pub departments: String,
    pub assignees: String,
    pub closed: bool,
    pub sheet_url: String,
    pub fizzy_url: String,
    pub last_active_at: Option<String>,
    /// Where it can move (not where it is).
    pub moves: Vec<Choice>,
}

fn list_card(config: &WorkspaceConfig, snapshot: &Snapshot, settings: &Settings, card: &Card) -> ListCard {
    let state = card.state();
    ListCard {
        number: card.number,
        title: if card.title.trim().is_empty() { "Untitled".into() } else { card.title.trim().to_string() },
        tone: state.tone(),
        state: state.label().to_string(),
        severity: card.severity().map(Severity::as_str),
        departments: settings.departments_of(&card.tags).iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", "),
        assignees: card.assignees.iter().map(|user| user.name.as_str()).collect::<Vec<_>>().join(", "),
        closed: card.closed,
        sheet_url: sheet_path(card.number),
        fizzy_url: card_link(config, snapshot, card.number).unwrap_or_else(|| card.url.clone()),
        last_active_at: card.last_active_at.map(|at| at.to_string()),
        moves: move_choices(card, &snapshot.columns).into_iter().filter(|choice| !choice.selected).collect(),
    }
}

fn status(snapshot: &Snapshot) -> FizzyStatus {
    match (&snapshot.last_success, &snapshot.last_error) {
        (Some(_), None) => FizzyStatus::Ok,
        (Some(since), Some(_)) => FizzyStatus::Stale { since: since.to_string() },
        (None, error) => FizzyStatus::Waiting { failed: error.is_some() },
    }
}

fn board_name(config: &WorkspaceConfig, snapshot: &Snapshot) -> String {
    snapshot.board.as_ref().map(|board| board.name.clone()).unwrap_or_else(|| config.incident_board.clone())
}

// --- Board ----------------------------------------------------------------------------------------

/// The board's filters (`?dept=engineering&sev=critical,high`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardFilter {
    /// A department tag; unknown tags are ignored.
    pub department: Option<String>,
    /// Empty = every severity.
    pub severities: Vec<Severity>,
}

impl BoardFilter {
    pub fn parse(department: Option<&str>, severities: &[String], settings: &Settings) -> Self {
        Self {
            department: department.and_then(|tag| settings.department_by_tag(tag)).map(|department| department.tag.clone()),
            severities: severities.iter().flat_map(|value| value.split(',')).filter_map(Severity::parse).fold(
                Vec::new(),
                |mut all, severity| {
                    if !all.contains(&severity) {
                        all.push(severity);
                    }
                    all
                },
            ),
        }
    }

    fn keeps(&self, card: &Card) -> bool {
        self.department.as_deref().is_none_or(|tag| card.has_tag(tag))
            && (self.severities.is_empty() || card.severity().is_some_and(|severity| self.severities.contains(&severity)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardColumn {
    pub key: String,
    pub label: String,
    pub tone: &'static str,
    pub cards: Vec<ListCard>,
    /// Shown first on phones.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/board.html")]
pub struct BoardView {
    pub board_name: String,
    pub board_url: Option<String>,
    pub columns: Vec<BoardColumn>,
    pub departments: Vec<Choice>,
    pub severities: Vec<Choice>,
    pub filtered: bool,
    pub fizzy: FizzyStatus,
    pub retry_seconds: u64,
    pub settings_url: Option<String>,
    pub can_change: bool,
    pub can_create: bool,
}

pub fn board(
    config: &WorkspaceConfig,
    snapshot: &Snapshot,
    settings: &Settings,
    filter: &BoardFilter,
    administrator: bool,
    can_change: bool,
    can_create: bool,
) -> BoardView {
    let cards = |numbers: &[u64]| -> Vec<&Card> {
        numbers.iter().filter_map(|number| snapshot.card(*number)).filter(|card| filter.keeps(card)).collect()
    };
    let open = cards(&snapshot.open);
    let sorted = |mut cards: Vec<&Card>| {
        cards.sort_by_key(|card| (Reverse(card.severity()), card.created_at, card.number));
        cards.into_iter().map(|card| list_card(config, snapshot, settings, card)).collect::<Vec<_>>()
    };
    let mut columns = vec![BoardColumn {
        key: "new".into(),
        label: "New".into(),
        tone: "new",
        cards: sorted(open.iter().copied().filter(|card| card.state() == CardState::New).collect()),
        active: false,
    }];
    for column in &snapshot.columns {
        let here: Vec<&Card> = open
            .iter()
            .copied()
            .filter(|card| !card.closed && !card.postponed && card.column.as_ref().is_some_and(|on| on.id == column.id))
            .collect();
        columns.push(BoardColumn {
            key: format!("column:{}", column.id),
            label: column.name.clone(),
            tone: CardState::Column(column.name.clone(), column.color.clone()).tone(),
            cards: sorted(here),
            active: false,
        });
    }
    // Cards in a column the board no longer lists (renamed or deleted since the poll).
    let known: Vec<&str> = snapshot.columns.iter().map(|column| column.id.as_str()).collect();
    let strays: Vec<&Card> = open
        .iter()
        .copied()
        .filter(|card| {
            matches!(card.state(), CardState::Column(..)) && card.column.as_ref().is_some_and(|c| !known.contains(&c.id.as_str()))
        })
        .collect();
    if !strays.is_empty() {
        columns.push(BoardColumn {
            key: "other".into(),
            label: "Other columns".into(),
            tone: "doing",
            cards: sorted(strays),
            active: false,
        });
    }
    columns.push(BoardColumn {
        key: "not_now".into(),
        label: "Monitoring".into(),
        tone: "later",
        cards: sorted(open.iter().copied().filter(|card| card.state() == CardState::Monitoring).collect()),
        active: false,
    });
    let closed: Vec<ListCard> = cards(&snapshot.recently_closed)
        .into_iter()
        .filter(|card| card.closed)
        .take(MAX_CLOSED)
        .map(|card| list_card(config, snapshot, settings, card))
        .collect();
    columns.push(BoardColumn { key: "closed".into(), label: "Closed".into(), tone: "done", cards: closed, active: false });
    if let Some(first) = columns.iter().position(|column| !column.cards.is_empty()).or(Some(0)) {
        columns[first].active = true;
    }

    BoardView {
        board_name: board_name(config, snapshot),
        board_url: snapshot.board.as_ref().and_then(|board| {
            let account = snapshot.account.as_deref()?;
            Some(format!("{}/{account}/boards/{}", config.link_base(), board.id))
        }),
        columns,
        departments: settings
            .departments
            .iter()
            .map(|department| {
                Choice::new(department.tag.clone(), department.name.clone(), filter.department.as_deref() == Some(&department.tag))
            })
            .collect(),
        severities: Severity::ALL
            .iter()
            .map(|severity| Choice::new(severity.as_str(), severity.as_str(), filter.severities.contains(severity)))
            .collect(),
        filtered: filter.department.is_some() || !filter.severities.is_empty(),
        fizzy: status(snapshot),
        retry_seconds: config.poll_interval.as_secs(),
        settings_url: administrator.then(|| SETTINGS_PATH.to_string()),
        can_change,
        can_create,
    }
}

// --- Room panel -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/_panel.html")]
pub struct RoomPanel {
    pub room_id: i64,
    pub url: String,
    pub departments: String,
    pub board_name: String,
    /// The board, filtered by the first department.
    pub board_url: String,
    pub cards: Vec<ListCard>,
    pub stale: bool,
    pub can_create: bool,
}

/// `None` when the room isn't linked to any department (no panel).
pub fn room_panel(config: &WorkspaceConfig, snapshot: &Snapshot, settings: &Settings, room_id: i64, can_create: bool) -> Option<RoomPanel> {
    let departments = settings.departments_of_room(room_id);
    if departments.is_empty() {
        return None;
    }
    let mut cards: Vec<&Card> = snapshot
        .open
        .iter()
        .filter_map(|number| snapshot.card(*number))
        .filter(|card| !card.closed && departments.iter().any(|department| card.has_tag(&department.tag)))
        .collect();
    cards.sort_by_key(|card| (Reverse(card.severity()), Reverse(card.last_active_at), card.number));
    Some(RoomPanel {
        room_id,
        url: panel_path(room_id),
        departments: departments.iter().map(|department| department.name.as_str()).collect::<Vec<_>>().join(", "),
        board_name: board_name(config, snapshot),
        board_url: format!("{BOARD_PATH}?dept={}", departments[0].tag),
        cards: cards.into_iter().map(|card| list_card(config, snapshot, settings, card)).collect(),
        stale: snapshot.last_error.is_some(),
        can_create,
    })
}

// --- Card sheet -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepItem {
    pub id: String,
    pub content: String,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentItem {
    pub author: String,
    pub text: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/sheet.html")]
pub struct CardSheet {
    pub number: u64,
    pub title: String,
    pub board_name: String,
    pub fizzy_url: String,
    pub tone: &'static str,
    pub state: String,
    pub severity: Option<&'static str>,
    pub severities: Vec<Choice>,
    pub departments: Vec<Choice>,
    /// Tags that are neither severities nor departments.
    pub other_tags: Vec<String>,
    pub assignees: String,
    pub description: String,
    pub steps: Vec<StepItem>,
    pub steps_done: usize,
    pub comments: Vec<CommentItem>,
    /// Earlier comments than those shown exist (they're in Fizzy).
    pub earlier_comments: bool,
    pub moves: Vec<Choice>,
    /// `POST <action_url>/<change>`.
    pub action_url: String,
    pub can_change: bool,
    pub can_comment: bool,
}

pub struct SheetInput<'a> {
    pub card: &'a Card,
    /// The newest comments, oldest first.
    pub comments: &'a [Comment],
    pub earlier_comments: bool,
    pub columns: &'a [Column],
    pub can_change: bool,
    pub can_comment: bool,
}

pub fn card_sheet(config: &WorkspaceConfig, snapshot: &Snapshot, settings: &Settings, input: SheetInput<'_>) -> CardSheet {
    let card = input.card;
    let state = card.state();
    let severity = card.severity();
    let departments = settings.departments_of(&card.tags);
    let mut severities = vec![Choice::new("", "none", severity.is_none())];
    severities.extend(Severity::ALL.iter().map(|each| Choice::new(each.as_str(), each.as_str(), Some(*each) == severity)));
    CardSheet {
        number: card.number,
        title: if card.title.trim().is_empty() { "Untitled".into() } else { card.title.trim().to_string() },
        board_name: card.board.as_ref().map(|board| board.name.clone()).unwrap_or_else(|| board_name(config, snapshot)),
        fizzy_url: card_link(config, snapshot, card.number).unwrap_or_else(|| card.url.clone()),
        tone: state.tone(),
        state: state.label().to_string(),
        severity: severity.map(Severity::as_str),
        severities,
        departments: settings
            .departments
            .iter()
            .map(|department| {
                Choice::new(department.tag.clone(), department.name.clone(), departments.iter().any(|d| d.tag == department.tag))
            })
            .collect(),
        other_tags: card
            .tags
            .iter()
            .filter(|tag| Severity::from_tag(tag).is_none() && settings.department_by_tag(tag).is_none())
            .cloned()
            .collect(),
        assignees: card.assignees.iter().map(|user| user.name.as_str()).collect::<Vec<_>>().join(", "),
        description: card.description.trim().to_string(),
        steps_done: card.steps.iter().filter(|step| step.completed).count(),
        steps: card
            .steps
            .iter()
            .map(|step| StepItem { id: step.id.clone(), content: step.content.clone(), completed: step.completed })
            .collect(),
        comments: input
            .comments
            .iter()
            .map(|comment| CommentItem {
                author: comment
                    .creator
                    .as_ref()
                    .map(|creator| creator.name.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "Someone".into()),
                text: html::truncate(&comment.text(), 4000),
                created_at: comment.created_at.map(|at| at.to_string()),
            })
            .collect(),
        earlier_comments: input.earlier_comments,
        moves: move_choices(card, input.columns),
        action_url: sheet_path(card.number),
        can_change: input.can_change,
        can_comment: input.can_comment,
    }
}

// --- New card -------------------------------------------------------------------------------------

/// The message a card is created from, as the form shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSource {
    pub message_id: i64,
    pub author_name: String,
    pub room_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/new_card.html")]
pub struct NewCardForm {
    pub action: String,
    pub board_name: String,
    pub title: String,
    pub description: String,
    pub severities: Vec<Choice>,
    pub departments: Vec<Choice>,
    pub room_id: Option<i64>,
    pub source: Option<FormSource>,
}

/// The form, prefilled from the message's text (title: its first line, cut) and the room's
/// department.
pub fn new_card_form(
    config: &WorkspaceConfig,
    snapshot: &Snapshot,
    settings: &Settings,
    room_id: Option<i64>,
    message_text: Option<&str>,
    source: Option<FormSource>,
) -> NewCardForm {
    let text = message_text.unwrap_or("").trim();
    let title = text.lines().map(str::trim).find(|line| !line.is_empty()).map(|line| html::truncate(line, 120)).unwrap_or_default();
    let room_department = room_id.and_then(|room| settings.departments_of_room(room).first().map(|department| department.tag.clone()));
    let mut severities = vec![Choice::new("", "Pick a severity", true)];
    severities.extend(Severity::ALL.iter().map(|severity| Choice::new(severity.as_str(), severity.as_str(), false)));
    let mut departments = vec![Choice::new("", "No department", room_department.is_none())];
    departments.extend(
        settings.departments.iter().map(|d| Choice::new(d.tag.clone(), d.name.clone(), room_department.as_deref() == Some(d.tag.as_str()))),
    );
    NewCardForm {
        action: CARDS_PATH.into(),
        board_name: board_name(config, snapshot),
        title,
        description: html::truncate(text, 10_000),
        severities,
        departments,
        room_id,
        source,
    }
}

// --- Settings -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepartmentRow {
    pub name: String,
    pub tag: String,
    pub rooms: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/settings.html")]
pub struct SettingsPage {
    pub action: String,
    pub board_name: String,
    pub departments: Vec<DepartmentRow>,
    /// The rooms of a new department row, none checked.
    pub blank_rooms: Vec<Choice>,
    /// Active people (not bots); `selected` = a listed duty manager.
    pub people: Vec<Choice>,
    /// `duty_managers` isn't set: the administrators are the duty managers.
    pub managers_are_admins: bool,
    pub administrators: String,
    pub policies: Vec<Choice>,
    pub load_error: Option<String>,
}

/// `rooms` and `people` are `(id, name)`; `administrators` their names.
pub fn settings_page(
    config: &WorkspaceConfig,
    snapshot: &Snapshot,
    settings: &Settings,
    rooms: &[(i64, String)],
    people: &[(i64, String)],
    administrators: &[String],
    load_error: Option<String>,
) -> SettingsPage {
    let room_choices = |checked: &[i64]| {
        rooms.iter().map(|(id, name)| Choice::new(id.to_string(), name.clone(), checked.contains(id))).collect::<Vec<_>>()
    };
    let managers = settings.duty_managers.clone().unwrap_or_default();
    SettingsPage {
        action: SETTINGS_PATH.into(),
        board_name: board_name(config, snapshot),
        departments: settings
            .departments
            .iter()
            .map(|department| DepartmentRow {
                name: department.name.clone(),
                tag: department.tag.clone(),
                rooms: room_choices(&department.rooms),
            })
            .collect(),
        blank_rooms: room_choices(&[]),
        people: people.iter().map(|(id, name)| Choice::new(id.to_string(), name.clone(), managers.contains(id))).collect(),
        managers_are_admins: settings.duty_managers.is_none(),
        administrators: administrators.join(", "),
        policies: Policy::ALL
            .iter()
            .map(|policy| Choice::new(policy.as_str(), policy.label(), *policy == settings.confirm_policy))
            .collect(),
        load_error,
    }
}

/// `GET /hermes/:bot_key/workspace/settings.json`: what Hermes's skill needs to tag cards by
/// department. `rooms` and `managers` are `(id, name)`.
pub fn bot_settings(
    config: &WorkspaceConfig,
    snapshot: &Snapshot,
    settings: &Settings,
    rooms: &[(i64, String)],
    managers: &[(i64, String)],
) -> Value {
    let room_name = |id: &i64| rooms.iter().find(|(room, _)| room == id).map(|(_, name)| name.clone());
    json!({
        "incident_board": board_name(config, snapshot),
        "severity_tags": Severity::ALL.iter().rev().map(|severity| severity.tag()).collect::<Vec<_>>(),
        "departments": settings.departments.iter().map(|department| json!({
            "name": department.name,
            "tag": department.tag,
            "rooms": department.rooms.iter().map(|id| json!({ "id": id, "name": room_name(id) })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "duty_managers": managers.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
        "confirm_policy": settings.confirm_policy.as_str(),
    })
}
