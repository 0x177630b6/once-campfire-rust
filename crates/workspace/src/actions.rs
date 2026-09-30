//! What people do to incident cards from Campfire (phase 1): read a card's sheet, move / close /
//! "not now" it, change its severity or departments, tick its steps, comment, and create a card
//! (from a message). Every write:
//!
//! 1. is authorized by the settings' [`Policy`](crate::settings::Policy) ([`Workspace::authorize`]);
//! 2. validates its input ([`Change::parse`], [`NewCard::parse`]) and answers 404 for a card that
//!    isn't on the incident board (a fresh read, before anything is written);
//! 3. goes through a [`Writer`] as the actor's [`ActingIdentity`](crate::writes::ActingIdentity),
//!    which logs it;
//! 4. reads the card again, checks the change took, and puts it in the cache at once
//!    ([`Workspace::remember`]) instead of waiting for the next poll.

use serde_json::Value;

use crate::Workspace;
use crate::fizzy::{Card, Client, HttpClient, Severity};
use crate::home::Viewer;
use crate::pages::{self, CardSheet, SheetInput};
use crate::settings::{Act, Settings};
use crate::writes::{ActionError, Writer};

pub const MAX_TITLE_CHARS: usize = 255;
pub const MAX_TEXT_CHARS: usize = 10_000;
pub const MAX_COMMENT_CHARS: usize = 5_000;
/// Comment pages read for a sheet (Fizzy's pages are 15, 30, 50, then 100 long).
const COMMENT_PAGES: u32 = 4;

/// Where a card goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// "Maybe?" (`DELETE /triage`).
    New,
    /// A board column (`POST /triage`).
    Column(String),
    /// "Not Now" (`POST /not_now`).
    NotNow,
    /// Done (`POST /closure`).
    Closed,
}

impl Target {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "new" => Some(Self::New),
            "not_now" => Some(Self::NotNow),
            "closed" => Some(Self::Closed),
            other => other.strip_prefix("column:").filter(|id| !id.is_empty() && id.len() <= 64).map(|id| Self::Column(id.to_string())),
        }
    }

    fn reached(&self, card: &Card) -> bool {
        match self {
            Self::New => !card.closed && !card.postponed && card.column.is_none(),
            Self::Column(id) => !card.closed && !card.postponed && card.column.as_ref().is_some_and(|column| &column.id == id),
            Self::NotNow => !card.closed && card.postponed,
            Self::Closed => card.closed,
        }
    }
}

/// A change to one card (`POST /workspace/cards/:n/:change`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `move` `{"to": "new" | "column:<id>" | "not_now" | "closed"}`
    Move(Target),
    /// `severity` `{"severity": "high" | ""}`
    Severity(Option<Severity>),
    /// `departments` `{"tags": ["engineering"]}`: the card's department tags, exactly.
    Departments(Vec<String>),
    /// `step` `{"step_id": "…", "completed": true}`
    Step { id: String, completed: bool },
    /// `comment` `{"body": "…"}`
    Comment(String),
}

impl Change {
    /// `None` for an unknown change (404); an error for bad input (422).
    pub fn parse(kind: &str, params: &Value, settings: &Settings) -> Option<Result<Self, ActionError>> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        Some(match kind {
            "move" => Target::parse(&text("to"))
                .map(Self::Move)
                .ok_or_else(|| ActionError::invalid("invalid_target", "Pick where to move the card.")),
            "severity" => match text("severity").trim() {
                "" => Ok(Self::Severity(None)),
                value => Severity::parse(value)
                    .map(|severity| Self::Severity(Some(severity)))
                    .ok_or_else(|| ActionError::invalid("invalid_severity", "Severity must be low, medium, high or critical.")),
            },
            "departments" => department_tags(params.get("tags"), settings).map(Self::Departments),
            "step" => {
                let id = text("step_id");
                let completed =
                    params.get("completed").and_then(|value| value.as_bool().or_else(|| value.as_str().map(|s| s == "true" || s == "1")));
                match completed {
                    Some(completed) if !id.trim().is_empty() && id.len() <= 64 => Ok(Self::Step { id: id.trim().to_string(), completed }),
                    _ => Err(ActionError::invalid("invalid_step", "Which step, and is it done?")),
                }
            }
            "comment" => {
                let body = text("body").trim().to_string();
                if body.is_empty() {
                    Err(ActionError::invalid("blank_comment", "Write something first."))
                } else if body.chars().count() > MAX_COMMENT_CHARS {
                    Err(ActionError::invalid("comment_too_long", format!("Comments are {MAX_COMMENT_CHARS} characters at most.")))
                } else {
                    Ok(Self::Comment(body))
                }
            }
            _ => return None,
        })
    }

    fn act(&self) -> Act {
        match self {
            Self::Comment(_) => Act::Comment,
            _ => Act::ChangeCard,
        }
    }
}

/// `tags` (an array, or one string) as department tags, each one a configured department's.
fn department_tags(value: Option<&Value>, settings: &Settings) -> Result<Vec<String>, ActionError> {
    let values: Vec<&str> = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(tag)) => vec![tag.as_str()],
        Some(Value::Array(tags)) => tags.iter().filter_map(Value::as_str).collect(),
        Some(_) => return Err(ActionError::invalid("invalid_departments", "Departments must be a list of tags.")),
    };
    let mut tags = Vec::new();
    for value in values.into_iter().filter(|value| !value.trim().is_empty()) {
        let department = settings
            .department_by_tag(value)
            .ok_or_else(|| ActionError::invalid("unknown_department", format!("“{}” isn't one of the departments.", value.trim())))?;
        if !tags.contains(&department.tag) {
            tags.push(department.tag.clone());
        }
    }
    Ok(tags)
}

/// A card to create (`POST /workspace/cards`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCard {
    pub title: String,
    pub description: String,
    pub severity: Option<Severity>,
    pub departments: Vec<String>,
    /// The message it comes from, for the comment that links back.
    pub source: Option<CardSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardSource {
    /// Absolute URL of the message in its room.
    pub message_url: String,
    pub room_name: String,
    pub author_name: String,
}

impl NewCard {
    /// `{"title", "description", "severity", "departments": [...] | "department"}`; the source is
    /// the app's to fill in.
    pub fn parse(params: &Value, settings: &Settings) -> Result<Self, ActionError> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let title = text("title").split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return Err(ActionError::invalid("blank_title", "The card needs a title."));
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(ActionError::invalid("title_too_long", format!("Titles are {MAX_TITLE_CHARS} characters at most.")));
        }
        let description = text("description").trim().to_string();
        if description.chars().count() > MAX_TEXT_CHARS {
            return Err(ActionError::invalid("description_too_long", format!("Details are {MAX_TEXT_CHARS} characters at most.")));
        }
        let severity = match text("severity").trim() {
            "" => None,
            value => Some(
                Severity::parse(value)
                    .ok_or_else(|| ActionError::invalid("invalid_severity", "Severity must be low, medium, high or critical."))?,
            ),
        };
        let departments = department_tags(params.get("departments").or_else(|| params.get("department")), settings)?;
        Ok(Self { title, description, severity, departments, source: None })
    }
}

/// A card created from Campfire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub card: Card,
    /// The card's page for browsers.
    pub url: String,
    /// The card exists but a later step failed (tags, the comment): what to fix.
    pub warning: Option<String>,
}

fn severity_tags() -> Vec<String> {
    Severity::ALL.iter().map(|severity| severity.tag()).collect()
}

impl Workspace {
    /// The one place the settings' policy is applied.
    pub fn authorize(&self, act: &Act, actor: &Viewer) -> Result<(), ActionError> {
        let settings = self.settings();
        if settings.permits(act, actor) { Ok(()) } else { Err(ActionError::Forbidden(act.refusal(settings.confirm_policy).into())) }
    }

    /// Whether `actor` may, for showing or hiding controls (the actions check again).
    pub fn may(&self, act: &Act, actor: &Viewer) -> bool {
        self.settings().permits(act, actor)
    }

    /// The account and the incident board, once a poll found them.
    fn target(&self) -> Result<(String, crate::fizzy::Board), ActionError> {
        let snapshot = self.snapshot();
        match (&snapshot.account, &snapshot.board) {
            (Some(account), Some(board)) => Ok((account.clone(), board.clone())),
            _ => Err(ActionError::Fizzy("The incident board hasn't been reached yet; try again in a moment.".into())),
        }
    }

    fn writer<'a>(&'a self, http: &'a dyn HttpClient, actor: &'a Viewer) -> Result<Writer<'a>, ActionError> {
        let (account, board) = self.target()?;
        let identity = self.tokens.identity(actor).map_err(ActionError::Forbidden)?;
        Ok(Writer { client: Client::new(http, &self.config), identity, actor, account, board, audit: &*self.audit })
    }

    /// A card's sheet, read fresh from Fizzy (steps and comments included). 404 for a card that
    /// isn't on the incident board.
    pub async fn card_sheet(&self, http: &dyn HttpClient, viewer: &Viewer, number: u64) -> Result<CardSheet, ActionError> {
        let (account, board) = self.target()?;
        let client = Client::new(http, &self.config);
        let unavailable = |_| ActionError::Fizzy("Fizzy can't be reached right now; try again in a moment.".into());
        let card = match client.card(&account, number).await.map_err(unavailable)? {
            Some(card) if card.board.as_ref().is_some_and(|on| on.id == board.id) => card,
            _ => return Err(ActionError::NotFound),
        };
        let (comments, more_comments) = client.comments(&account, number, COMMENT_PAGES).await.map_err(unavailable)?;
        self.remember(card.clone());
        let snapshot = self.snapshot();
        let input = SheetInput {
            card: &card,
            comments: &comments,
            more_comments,
            columns: &snapshot.columns,
            can_change: self.may(&Act::ChangeCard, viewer),
            can_comment: self.may(&Act::Comment, viewer),
        };
        Ok(pages::card_sheet(&self.config, &snapshot, &self.settings(), input))
    }

    /// Applies one change to an incident-board card, as `actor`. Returns the card as Fizzy has it
    /// afterwards (also in the cache). On an error after a write, the cache gets the card's actual
    /// state if Fizzy still answers.
    pub async fn change_card(&self, http: &dyn HttpClient, actor: &Viewer, number: u64, change: Change) -> Result<Card, ActionError> {
        self.authorize(&change.act(), actor)?;
        let writer = self.writer(http, actor)?;
        let result = self.apply(&writer, number, change).await;
        match &result {
            Ok(card) => self.remember(card.clone()),
            Err(ActionError::NotFound | ActionError::Forbidden(_) | ActionError::Invalid { .. }) => {}
            Err(_) => {
                if let Ok(card) = writer.card(number).await {
                    self.remember(card);
                }
            }
        }
        result
    }

    async fn apply(&self, writer: &Writer<'_>, number: u64, change: Change) -> Result<Card, ActionError> {
        match change {
            Change::Move(target) => {
                let card = writer.card(number).await?;
                if target.reached(&card) {
                    return Ok(card);
                }
                match &target {
                    Target::New => writer.untriage(number).await?,
                    Target::Column(id) => {
                        if !writer.columns().await?.iter().any(|column| &column.id == id) {
                            return Err(ActionError::invalid("unknown_column", "That column isn't on the incident board any more."));
                        }
                        writer.triage(number, id).await?
                    }
                    Target::NotNow => {
                        // Fizzy postpones open cards only.
                        if card.closed {
                            writer.reopen(number).await?;
                        }
                        writer.not_now(number).await?
                    }
                    Target::Closed => writer.close(number).await?,
                }
                let after = writer.card(number).await?;
                if !target.reached(&after) {
                    return Err(ActionError::Fizzy("Fizzy didn't move the card; it shows where the card is now.".into()));
                }
                Ok(after)
            }
            Change::Severity(severity) => {
                writer.set_tags(number, &severity_tags(), &severity.map(|severity| vec![severity.tag()]).unwrap_or_default()).await
            }
            Change::Departments(tags) => {
                let managed: Vec<String> = self.settings().departments.iter().map(|department| department.tag.clone()).collect();
                writer.set_tags(number, &managed, &tags).await
            }
            Change::Step { id, completed } => {
                let card = writer.card(number).await?;
                let Some(step) = card.steps.iter().find(|step| step.id == id) else {
                    return Err(ActionError::invalid("unknown_step", "That step isn't on the card any more."));
                };
                if step.completed == completed {
                    return Ok(card);
                }
                writer.set_step(number, &id, completed).await?;
                writer.card(number).await
            }
            Change::Comment(text) => {
                writer.card(number).await?;
                writer.comment(number, &text, None).await?;
                writer.card(number).await
            }
        }
    }

    /// Creates a card on the incident board as `actor`: the card, then its severity and
    /// department tags, then a comment linking back to the source message. A failure after the
    /// card exists doesn't lose it: [`Created::warning`] says what's missing.
    pub async fn create_card(&self, http: &dyn HttpClient, actor: &Viewer, new: NewCard) -> Result<Created, ActionError> {
        self.authorize(&Act::CreateCard, actor)?;
        let writer = self.writer(http, actor)?;
        let description = crate::writes::comment_html(None, &new.description, None);
        let card = writer.create_card(&new.title, &description).await?;
        let number = card.number;
        let mut warnings = Vec::new();

        let mut managed = severity_tags();
        managed.extend(self.settings().departments.iter().map(|department| department.tag.clone()));
        let mut wanted: Vec<String> = new.severity.map(|severity| vec![severity.tag()]).unwrap_or_default();
        wanted.extend(new.departments.iter().cloned());
        let mut card = card;
        if !wanted.is_empty() {
            match writer.set_tags(number, &managed, &wanted).await {
                Ok(tagged) => card = tagged,
                Err(error) => warnings.push(format!("its tags couldn't be set ({error})")),
            }
        }
        if let Some(source) = &new.source {
            let text = format!("Created from {}’s message in {}:", source.author_name, source.room_name);
            if let Err(error) = writer.comment(number, &text, Some(&source.message_url)).await {
                warnings.push(format!("the comment linking to the message couldn't be posted ({error})"));
            }
        }
        if let Ok(fresh) = writer.card(number).await {
            card = fresh;
        }
        self.remember(card.clone());
        let url = crate::chips::card_link(&self.config, &self.snapshot(), number).unwrap_or_else(|| card.url.clone());
        let warning =
            (!warnings.is_empty()).then(|| format!("Card #{number} was created, but {}. Open it to finish.", warnings.join(", and ")));
        Ok(Created { card, url, warning })
    }
}
