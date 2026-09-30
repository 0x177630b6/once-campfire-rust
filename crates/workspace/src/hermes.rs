//! Phase 2: supervising Hermes.
//!
//! - **The Hermes tab** (`GET /workspace/hermes`, [`HermesPage`]): what Hermes is waiting for
//!   (pending proposals, with Confirm / Dismiss), then its log ([`crate::hermes_log`]): its direct
//!   Fizzy actions, marked "direct", what Campfire ran for it, marked "via Campfire", and, from the
//!   viewer's own rooms, its drafts, questions and errors. Everyone signed in reads it (decision D4:
//!   it's incident-board activity, which they all see anyway), except a proposal's details (pending,
//!   or never filed), which only its room's members and the duty managers see
//!   ([`Workspace::sees_proposal`]); filters by kind.
//! - **Proposals** ([`Workspace::propose`], [`Workspace::decide`]): see [`crate::proposals`].
//! - **Undo** ([`Workspace::undo`]): an action of the last 24 hours with a recorded way back
//!   ([`Reverse`]), if nobody changed the card since; duty managers and the person it was for
//!   (decision D8, [`Act::Undo`]).
//!
//! Hermes keeps its own Fizzy write token (the owner's decision D7): the dial binds what Hermes
//! proposes through Campfire, not what it does with its `fizzy` CLI. Those direct actions show in the
//! log as "direct" (as far as Fizzy's activity feed tells: not tags, steps or where a moved card
//! was), and only the simple ones can be undone.

use std::collections::BTreeSet;

use askama::Template;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use crate::actions::{CardSource, Change, Target, severity_tags};
use crate::drafts::{self, Bot, ChatMessage, Decision};
use crate::fizzy::{Activity, Card, Client, FizzyError, HttpClient};
use crate::hermes_log::{self, Entry, Kind, Reverse, Via};
use crate::home::{FizzyStatus, Viewer};
use crate::html;
use crate::proposals::{self, Context, Proposal, Request, SourceMessage, Status};
use crate::settings::{Act, ActionKind, Dial};
use crate::writes::{ActingIdentity, ActionError, Purpose, Writer};
use crate::{ChatSource, Workspace};

pub const HERMES_PATH: &str = "/workspace/hermes";
/// `GET ?ids=a,b`: `{"proposals": {"a": {"status", "label"}}}`, for the drafts' buttons.
pub const PROPOSALS_PATH: &str = "/workspace/hermes/proposals.json";
/// Hermes's users are looked up again this often while unknown.
const USERS_RETRY: SignedDuration = SignedDuration::from_mins(10);
/// Pages of Hermes's activities read at the first poll (then one, or more while all are new).
const FIRST_PAGES: u32 = 3;
const MAX_PAGES: u32 = 3;
/// A change and its own record in Fizzy's feed or the write log are this close.
const MARGIN: SignedDuration = SignedDuration::from_secs(2);
/// Hermes's own follow-ups on a card it just created (tags, steps, a comment) don't count as
/// "touched since".
const CREATION_GRACE: SignedDuration = SignedDuration::from_mins(5);
/// Pages of the incident board's feed undo reads, at most, to reach the action it takes back.
const UNDO_FEED_PAGES: u32 = 10;
/// Bot messages the Hermes tab shows from the viewer's rooms.
const MESSAGES_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// The Fizzy users behind the tokens, learned from `GET /my/identity`.
#[derive(Debug, Clone, Default)]
pub struct FizzyUsers {
    /// `HERMES_FIZZY_TOKEN`'s user.
    pub hermes: Option<String>,
    /// `FIZZY_TOKEN`'s user (the workspace's "Campfire" user, or Hermes's on an older setup).
    pub workspace: Option<String>,
    checked_at: Option<Timestamp>,
    /// Hermes's activities were read once since the start (the first read goes further back).
    read_once: bool,
}

/// A proposal run: the card, the way back, the log's line, the card's link, a warning.
type Ran = (Card, Option<Reverse>, String, Option<String>, Option<String>);

/// What came of a proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum Proposed {
    /// Run at once (Alone, or a confirmed live voice report).
    Done { proposal: Proposal },
    /// Waiting for a confirmation; `duplicate`: Hermes had already proposed exactly this.
    Pending { proposal: Proposal, duplicate: bool },
    /// The dial says Never.
    Refused { message: String },
}

impl Workspace {
    /// Hermes's Fizzy user: the settings', else `HERMES_FIZZY_TOKEN`'s.
    pub fn hermes_fizzy_user(&self) -> Option<String> {
        self.settings().hermes_fizzy_user_id.clone().or_else(|| self.fizzy_users.read().unwrap_or_else(|e| e.into_inner()).hermes.clone())
    }

    /// Hermes's Campfire bot, the only one whose proposals are taken: `HERMES_BOT` (an id or an
    /// exact name), else `fallback` (the app passes `GEMINI_LIVE_VOICE_BOT`, the bot live voice
    /// reports go to), else the only active bot. `None`: none matches, or several bots and nothing
    /// says which (then proposals are refused).
    pub fn hermes_bot_id(&self, fallback: Option<&str>) -> Option<i64> {
        let bots = self.bots();
        match self.config.hermes_bot.as_deref().or(fallback).map(str::trim).filter(|wanted| !wanted.is_empty()) {
            Some(wanted) => match wanted.parse::<i64>() {
                Ok(id) => bots.iter().find(|bot| bot.id == id),
                Err(_) => bots.iter().find(|bot| bot.name == wanted),
            }
            .map(|bot| bot.id),
            None if bots.len() == 1 => Some(bots[0].id),
            None => None,
        }
    }

    pub fn fizzy_users(&self) -> FizzyUsers {
        self.fizzy_users.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The workspace writes with Hermes's own Fizzy user (no separate "Campfire" user yet): people's
    /// workspace actions then look like Hermes's in Fizzy, and the write log tells them apart.
    pub fn shares_hermes_token(&self) -> bool {
        let users = self.fizzy_users();
        match (users.workspace, self.hermes_fizzy_user()) {
            (Some(workspace), Some(hermes)) => workspace == hermes,
            _ => false,
        }
    }

    /// Learns the tokens' users, then reads Hermes's recent actions on the incident board into its
    /// log (deduplicated by activity id; Campfire's own writes left out).
    pub(crate) async fn poll_hermes(&self, client: &Client<'_>, snapshot: &crate::Snapshot, now: Timestamp) -> Result<(), FizzyError> {
        let (Some(account), Some(board)) = (snapshot.account.clone(), snapshot.board.clone()) else { return Ok(()) };
        let mut users = self.fizzy_users();
        let unknown = users.workspace.is_none() || (self.config.hermes_token.is_some() && users.hermes.is_none());
        if unknown && users.checked_at.is_none_or(|at| now.duration_since(at) >= USERS_RETRY) {
            users.checked_at = Some(now);
            if let Some(token) = &self.config.hermes_token {
                users.hermes = client.user_of(token, &account).await.ok().flatten().or(users.hermes);
            }
            users.workspace = client.user_of(&self.config.token, &account).await.ok().flatten().or(users.workspace);
            *self.fizzy_users.write().unwrap_or_else(|e| e.into_inner()) = users.clone();
        }
        self.hermes_log.compact(now);
        let Some(hermes) = self.hermes_fizzy_user() else { return Ok(()) };
        let path = Client::activities_path(&account, &board.id, Some(&hermes));
        let writes = self.journal.recent();
        let shared = self.shares_hermes_token();
        let can_delete = self.config.hermes_token.is_some();
        let pages = if users.read_once { MAX_PAGES } else { FIRST_PAGES };
        for page in 1..=pages {
            let (activities, next): (Vec<Activity>, bool) = client.get_page(&path, page).await?;
            let mut all_new = !activities.is_empty();
            for activity in &activities {
                let by_hermes = activity.creator.as_ref().is_some_and(|creator| creator.id == hermes);
                let on_board = activity.board.as_ref().is_none_or(|on| on.id == board.id);
                if !by_hermes || !on_board {
                    continue;
                }
                let id = format!("a-{}", activity.id);
                if self.hermes_log.contains(&id) {
                    all_new = false;
                    continue;
                }
                if hermes_log::made_by_campfire(activity, &writes, shared) {
                    continue;
                }
                let card = hermes_log::activity_card(activity).and_then(|number| snapshot.card(number));
                if let Some(entry) = hermes_log::direct_entry(activity, card, can_delete) {
                    self.hermes_log.add(entry);
                }
            }
            // Past the first read, the next page only while everything on this one was new.
            if !next || (users.read_once && !all_new) {
                break;
            }
        }
        self.fizzy_users.write().unwrap_or_else(|e| e.into_inner()).read_once = true;
        Ok(())
    }

    /// Pending proposals older than 24 h are dismissed ("timed out").
    pub fn expire_proposals(&self, now: Timestamp) {
        let due = self.proposals.all().iter().any(|p| p.status == Status::Pending && now >= p.expires_at());
        if !due {
            return;
        }
        let expired = self.proposals.update(now, |items| {
            let mut expired = Vec::new();
            for proposal in items.iter_mut().filter(|p| p.status == Status::Pending && now >= p.expires_at()) {
                proposal.status = Status::Expired;
                proposal.decided_at = Some(now);
                expired.push(proposal.clone());
            }
            expired
        });
        for proposal in expired {
            self.hermes_log.add(proposal_entry(
                &proposal,
                "expired",
                now,
                Kind::Expired,
                format!("Dismissed (timed out): {}", proposal.summary),
            ));
        }
    }

    /// The token Campfire writes with for Hermes: its own (`HERMES_FIZZY_TOKEN`), else the
    /// workspace's, whose comments then start with "Hermes: ".
    fn hermes_identity(&self, bot: &Viewer) -> Result<ActingIdentity, ActionError> {
        match &self.config.hermes_token {
            Some(token) => Ok(ActingIdentity { token: token.clone(), on_behalf: false, label: "hermes" }),
            None => self.tokens.identity(bot).map_err(ActionError::Forbidden),
        }
    }

    /// A proposal from Hermes (the bot route). `context` and `source` are what the app checked of
    /// the bridge's context. 422 for what doesn't parse, 404 for a card that isn't on the incident
    /// board, 502 when Fizzy doesn't answer; the dial decides the rest.
    pub async fn propose(
        &self,
        http: &dyn HttpClient,
        bot: &Bot,
        body: &Value,
        mut context: Context,
        source: Option<SourceMessage>,
    ) -> Result<Proposed, ActionError> {
        let now = self.now();
        self.expire_proposals(now);
        let (settings, snapshot) = (self.settings(), self.snapshot());
        let (request, action, kept) = proposals::parse(body, &settings, &snapshot)?;
        let (account, board) = self.target()?;
        if let Some(number) = request.card() {
            let client = Client::new(http, &self.config);
            match client.card(&account, number).await {
                Ok(Some(card)) if card.board.as_ref().is_some_and(|on| on.id == board.id) => self.remember(card),
                Ok(_) => return Err(ActionError::NotFound),
                Err(_) => return Err(ActionError::Fizzy("Fizzy can't be reached right now; try again in a moment.".into())),
            }
        }
        context.live_report = source.as_ref().is_some_and(|message| {
            Some(message.id) == context.message_id && proposals::is_confirmed_live_report(message, context.user_id, now)
        });
        let snapshot = self.snapshot();
        let (summary, details) = proposals::describe(&request, &settings, &snapshot);
        let kind = request.kind();
        let dial = settings.autonomy.dial(kind);

        if let Some(replaced) = body.get("replaces").and_then(Value::as_str) {
            self.supersede(replaced, bot.id, now);
        }
        let duplicate =
            self.proposals.all().into_iter().find(|p| {
                p.status == Status::Pending && p.bot_id == bot.id && p.request == kept && p.context.message_id == context.message_id
            });
        if let Some(proposal) = duplicate {
            return Ok(Proposed::Pending { proposal, duplicate: true });
        }
        let mut proposal = Proposal {
            id: proposals::new_id(now),
            created_at: now,
            status: Status::Pending,
            action,
            card: request.card(),
            request: kept,
            summary,
            details,
            context,
            bot_id: bot.id,
            bot_name: bot.name.clone(),
            draft_message_id: None,
            decided_at: None,
            decided_by_id: None,
            decided_by_name: None,
            result_card: None,
            result_url: None,
            message: None,
        };
        if dial == Dial::Never {
            let message = format!("Campfire's settings don't let Hermes do this (“{}”: Never).", kind.label());
            let mut entry = proposal_entry(&proposal, "refused", now, Kind::Refused, format!("Refused: {}", proposal.summary));
            entry.error = Some(message.clone());
            self.hermes_log.add(entry);
            return Ok(Proposed::Refused { message });
        }
        // Run at once (Alone, or a confirmed live report), else wait. Decided and stored under the
        // store's lock, so two proposals from one live report can't both run: one card per report
        // (one running counts), a second one asks first.
        let live_create = proposal.context.live_report && kind == ActionKind::Create;
        let runs = self.proposals.update(now, |items| {
            let report_used = live_create
                && items.iter().any(|p| {
                    p.action == "create"
                        && p.context.message_id.is_some()
                        && p.context.message_id == proposal.context.message_id
                        && matches!(p.status, Status::Done | Status::Running)
                });
            let runs = dial == Dial::Alone || (live_create && !report_used);
            let mut stored = proposal.clone();
            if runs {
                stored.status = Status::Running;
            }
            items.push(stored);
            runs
        });
        if runs {
            proposal.status = Status::Running;
            return match self.run_proposal(http, &proposal, request, None).await {
                Ok(done) => Ok(Proposed::Done { proposal: done }),
                Err(error) => Err(error),
            };
        }
        self.hermes_log.add(proposal_entry(&proposal, "proposed", now, Kind::Proposed, format!("Proposed: {}", proposal.summary)));
        Ok(Proposed::Pending { proposal, duplicate: false })
    }

    /// Hermes proposed something else in place of `id` (after "change: …").
    fn supersede(&self, id: &str, bot_id: i64, now: Timestamp) {
        let replaced = self.proposals.update(now, |items| {
            let proposal = items.iter_mut().find(|p| p.id == id && p.bot_id == bot_id && p.status == Status::Pending)?;
            proposal.status = Status::Superseded;
            proposal.decided_at = Some(now);
            Some(proposal.clone())
        });
        if let Some(proposal) = replaced {
            self.hermes_log.add(proposal_entry(&proposal, "superseded", now, Kind::Superseded, format!("Replaced: {}", proposal.summary)));
        }
    }

    /// The room message of a pending proposal's draft, once the app posted it.
    pub fn set_draft_message(&self, id: &str, message_id: i64) {
        self.proposals.update(self.now(), |items| {
            if let Some(proposal) = items.iter_mut().find(|p| p.id == id) {
                proposal.draft_message_id = Some(message_id);
            }
        });
    }

    /// Runs a proposal as Hermes (its token when Campfire holds it), logs it with the card's state
    /// before, and marks it done or failed. `confirmed_by`: who pressed File.
    async fn run_proposal(
        &self,
        http: &dyn HttpClient,
        proposal: &Proposal,
        request: Request,
        confirmed_by: Option<&Viewer>,
    ) -> Result<Proposal, ActionError> {
        let now = self.now();
        let bot = Viewer { id: proposal.bot_id, name: proposal.bot_name.clone(), email: None, administrator: false };
        let result = async {
            let identity = self.hermes_identity(&bot)?;
            let label = identity.label;
            let purpose = Purpose { via: "proposal", reference: Some(proposal.id.clone()) };
            let writer = self.writer_as(http, &bot, identity, purpose)?;
            match request {
                Request::Create(mut new) => {
                    let context = &proposal.context;
                    if let (Some(message_id), Some(room_id)) = (context.message_id, context.room_id) {
                        new.source = Some(CardSource {
                            message_path: format!("/rooms/{room_id}/@{message_id}"),
                            room_name: context.room_name.clone().unwrap_or_default(),
                            author_name: context.user_name.clone().unwrap_or_else(|| "someone".into()),
                        });
                    }
                    let created = self.create_with(&writer, new).await?;
                    let text = format!("Created {}", card_label(&created.card));
                    Ok((created.card, Some(Reverse::CloseCreated), text, Some(created.url), created.warning))
                }
                Request::Change { card: number, change } => {
                    let _lock = self.lock_card(number).await;
                    let result = self.run_change(&writer, number, change, label).await;
                    if result.is_err()
                        && let Ok(card) = writer.card(number).await
                    {
                        self.remember(card);
                    }
                    let (card, reverse, text) = result?;
                    self.remember(card.clone());
                    let url = crate::chips::card_link(&self.config, &self.snapshot(), number);
                    Ok((card, reverse, text, url, None))
                }
            }
        }
        .await;
        let result: Result<Ran, ActionError> = result;
        let decided = |status: Status, items: &mut Vec<Proposal>, update: &dyn Fn(&mut Proposal)| {
            items.iter_mut().find(|p| p.id == proposal.id).map(|p| {
                p.status = status;
                p.decided_at = Some(now);
                if let Some(by) = confirmed_by {
                    p.decided_by_id = Some(by.id);
                    p.decided_by_name = Some(by.name.clone());
                }
                update(p);
                p.clone()
            })
        };
        match result {
            Ok((card, reverse, text, url, warning)) => {
                let done = self.proposals.update(now, |items| {
                    decided(Status::Done, items, &|p| {
                        p.result_card = Some(card.number);
                        p.result_url = url.clone();
                        p.message = warning.clone();
                    })
                });
                let mut entry = proposal_entry(proposal, "done", now, done_kind(&proposal.action), text);
                entry.card = Some(card.number);
                entry.reverse = reverse;
                if entry.reverse.is_none() {
                    entry.no_undo = Some("Nothing changed (the card was already like that).".into());
                }
                entry.error = warning;
                if let Some(by) = confirmed_by {
                    entry.by_user_id = Some(by.id);
                    entry.by_name = Some(by.name.clone());
                }
                self.hermes_log.add(entry);
                Ok(done.unwrap_or_else(|| proposal.clone()))
            }
            Err(error) => {
                self.proposals.update(now, |items| decided(Status::Failed, items, &|p| p.message = Some(error.message())));
                let mut entry = proposal_entry(proposal, "failed", now, Kind::Failed, format!("Failed: {}", proposal.summary));
                entry.error = Some(error.message());
                if let Some(by) = confirmed_by {
                    entry.by_user_id = Some(by.id);
                    entry.by_name = Some(by.name.clone());
                }
                self.hermes_log.add(entry);
                Err(error)
            }
        }
    }

    /// One change for Hermes, with the way back.
    async fn run_change(
        &self,
        writer: &Writer<'_>,
        number: u64,
        change: Change,
        label: &'static str,
    ) -> Result<(Card, Option<Reverse>, String), ActionError> {
        let before = writer.card(number).await?;
        if let Change::Comment(text) = &change {
            let id = writer.comment(number, text, None).await?;
            let after = writer.card(number).await?;
            let reverse = id.map(|comment_id| Reverse::DeleteComment { comment_id, identity: label.into() });
            let said = format!("Commented on {}: “{}”", card_label(&after), html::truncate(&text.replace('\n', " "), 120));
            return Ok((after, reverse, said));
        }
        if let Change::Move(Target::Column(id)) = &change
            && !writer.columns().await?.iter().any(|column| &column.id == id)
        {
            return Err(ActionError::invalid("unknown_column", "That column isn't on the incident board any more."));
        }
        let after = self.apply(writer, number, change.clone()).await?;
        let managed = |tags: &[String]| -> Vec<String> { tags.to_vec() };
        let among = |card: &Card, managed: &[String]| -> Vec<String> {
            let mut tags: Vec<String> = card
                .tags
                .iter()
                .map(|t| t.trim_start_matches('#').to_lowercase())
                .filter(|tag| managed.iter().any(|m| m.eq_ignore_ascii_case(tag)))
                .collect();
            tags.sort();
            tags
        };
        let (reverse, text) = match &change {
            Change::Severity(severity) => {
                let managed = managed(&severity_tags());
                let (was, now) = (among(&before, &managed), among(&after, &managed));
                let text = match severity {
                    Some(severity) => format!("Set the severity of {} to {}", card_label(&after), severity.as_str()),
                    None => format!("Removed the severity of {}", card_label(&after)),
                };
                ((was != now).then_some(Reverse::Tags { managed, before: was, after: now }), text)
            }
            Change::Departments(_) => {
                let departments: Vec<String> = self.settings().departments.iter().map(|d| d.tag.clone()).collect();
                let managed = managed(&departments);
                let (was, now) = (among(&before, &managed), among(&after, &managed));
                let text = format!(
                    "Set the departments of {} to {}",
                    card_label(&after),
                    if now.is_empty() { "none".into() } else { now.join(", ") }
                );
                ((was != now).then_some(Reverse::Tags { managed, before: was, after: now }), text)
            }
            Change::Move(_) => {
                let (was, now) = (crate::pages::state_key(&before), crate::pages::state_key(&after));
                let text = if after.closed {
                    format!("Closed {}", card_label(&after))
                } else {
                    format!("Moved {} to {}", card_label(&after), after.state().label())
                };
                ((was != now).then_some(Reverse::Place { before: was, after: now }), text)
            }
            Change::Step { id, completed } => {
                let changed = before.steps.iter().find(|step| &step.id == id).is_some_and(|step| step.completed != *completed);
                let verb = if *completed { "Ticked" } else { "Unticked" };
                (changed.then(|| Reverse::Step { id: id.clone(), before: !completed }), format!("{verb} a step of {}", card_label(&after)))
            }
            Change::Comment(_) => (None, String::new()),
        };
        Ok((after, reverse, text))
    }

    /// File (confirm) or Dismiss a pending proposal, as `actor`, per the confirm policy.
    pub async fn decide(&self, http: &dyn HttpClient, actor: &Viewer, id: &str, decision: Decision) -> Result<Proposal, ActionError> {
        let now = self.now();
        self.expire_proposals(now);
        let proposal = self.proposals.get(id).ok_or(ActionError::NotFound)?;
        self.authorize(&Act::ConfirmDraft { reporter_id: proposal.context.user_id }, actor)?;
        let (settings, snapshot) = (self.settings(), self.snapshot());
        let parsed = proposals::parse(&proposal.request, &settings, &snapshot);
        if decision == Decision::Confirm
            && let Ok((request, ..)) = &parsed
            && settings.autonomy.dial(request.kind()) == Dial::Never
        {
            return Err(ActionError::Forbidden(format!(
                "Campfire's settings no longer let Hermes do this (“{}”: Never).",
                request.kind().label()
            )));
        }
        let claimed = self.proposals.update(now, |items| {
            let proposal = items.iter_mut().find(|p| p.id == id)?;
            if proposal.status != Status::Pending {
                return Some(Err(proposal.state_label()));
            }
            proposal.status = if decision == Decision::Confirm { Status::Running } else { Status::Dismissed };
            if decision == Decision::Dismiss {
                proposal.decided_at = Some(now);
                proposal.decided_by_id = Some(actor.id);
                proposal.decided_by_name = Some(actor.name.clone());
            }
            Some(Ok(proposal.clone()))
        });
        let proposal = match claimed {
            None => return Err(ActionError::NotFound),
            Some(Err(state)) => return Err(ActionError::invalid("not_pending", format!("This proposal isn't waiting any more: {state}."))),
            Some(Ok(proposal)) => proposal,
        };
        if decision == Decision::Dismiss {
            let mut entry = proposal_entry(&proposal, "dismissed", now, Kind::Dismissed, format!("Dismissed: {}", proposal.summary));
            entry.by_user_id = Some(actor.id);
            entry.by_name = Some(actor.name.clone());
            self.hermes_log.add(entry);
            return Ok(proposal);
        }
        match parsed {
            Ok((request, ..)) => self.run_proposal(http, &proposal, request, Some(actor)).await,
            Err(error) => {
                // The settings changed since (a department removed…): it can't run as proposed.
                self.proposals.update(now, |items| {
                    if let Some(p) = items.iter_mut().find(|p| p.id == id) {
                        p.status = Status::Failed;
                        p.decided_at = Some(now);
                        p.message = Some(error.message());
                    }
                });
                let mut entry = proposal_entry(&proposal, "failed", now, Kind::Failed, format!("Failed: {}", proposal.summary));
                entry.error = Some(error.message());
                self.hermes_log.add(entry);
                Err(error)
            }
        }
    }

    /// Takes back a logged action, as `actor`. Refused (422 with the reason) when it has no way
    /// back, was undone already, is older than 24 h, or the card changed since; 403 per D8.
    pub async fn undo(&self, http: &dyn HttpClient, actor: &Viewer, id: &str) -> Result<Entry, ActionError> {
        let now = self.now();
        let entry = self.hermes_log.get(id).filter(|entry| entry.kind != Kind::Undone).ok_or(ActionError::NotFound)?;
        let Some(reverse) = entry.reverse.clone() else {
            return Err(ActionError::invalid("cannot_undo", entry.no_undo.clone().unwrap_or_else(|| "This can't be undone.".into())));
        };
        self.authorize(&Act::Undo { for_user_id: entry.for_user_id }, actor)?;
        if let Some(undone) = self.hermes_log.undo_of(id) {
            let by = undone.by_name.map(|name| format!(" by {name}")).unwrap_or_default();
            return Err(ActionError::invalid("already_undone", format!("Already undone{by}.")));
        }
        if now.duration_since(entry.at) > hermes_log::UNDO_WINDOW {
            return Err(ActionError::invalid("too_old", "Only what Hermes did in the last 24 hours can be undone here."));
        }
        let number = entry.card.ok_or_else(|| ActionError::invalid("cannot_undo", "This isn't about a card."))?;
        let identity = match &reverse {
            // Only a comment's creator can delete it.
            Reverse::DeleteComment { identity, .. } if identity == "hermes" => match &self.config.hermes_token {
                Some(token) => ActingIdentity { token: token.clone(), on_behalf: false, label: "hermes" },
                None => {
                    return Err(ActionError::invalid(
                        "cannot_undo",
                        "Only Hermes's own token can delete its comment, and Campfire doesn't hold it (HERMES_FIZZY_TOKEN).",
                    ));
                }
            },
            _ => self.tokens.identity(actor).map_err(ActionError::Forbidden)?,
        };
        let writer = self.writer_as(http, actor, identity, Purpose { via: "undo", reference: Some(entry.id.clone()) })?;
        let _lock = self.lock_card(number).await;
        let card = writer.card(number).await?;
        if let Some(reason) = self.touched_since(&writer, &entry, &reverse, &card).await? {
            return Err(ActionError::invalid("changed_since", reason));
        }
        let result = match &reverse {
            Reverse::CloseCreated => {
                let text = format!("Undone from Campfire: {} created this card by mistake. Closed, not deleted.", entry_actor(&entry));
                writer.comment(number, &text, None).await?;
                writer.close(number).await
            }
            Reverse::Tags { managed, before, .. } => writer.set_tags(number, managed, before).await.map(|_| ()),
            Reverse::Place { before, .. } => match Target::parse(before) {
                Some(target) => self.apply(&writer, number, Change::Move(target)).await.map(|_| ()),
                None => Err(ActionError::invalid("cannot_undo", "Where the card was isn't known.")),
            },
            Reverse::Step { id, before } => writer.set_step(number, id, *before).await,
            Reverse::DeleteComment { comment_id, .. } => writer.delete_comment(number, comment_id).await,
            Reverse::Title { before, .. } => writer.set_title(number, before).await,
            Reverse::Reopen => writer.reopen(number).await,
            Reverse::Close => writer.close(number).await,
        };
        if let Ok(card) = writer.card(number).await {
            self.remember(card);
        }
        result?;
        let mut undone = Entry::new(format!("u-{}", entry.id), self.now(), Kind::Undone, Via::Campfire, format!("Undone: {}", entry.text));
        undone.card = Some(number);
        undone.target = Some(entry.id.clone());
        undone.by_user_id = Some(actor.id);
        undone.by_name = Some(actor.name.clone());
        undone.for_user_id = entry.for_user_id;
        undone.for_name = entry.for_name.clone();
        undone.proposal = entry.proposal.clone();
        self.hermes_log.add(undone.clone());
        Ok(undone)
    }

    /// Why the card can't be taken back safely: its state isn't what the action left, someone
    /// wrote to it from Campfire since, or Fizzy's feed shows a change since. `None`: untouched.
    async fn touched_since(
        &self,
        writer: &Writer<'_>,
        entry: &Entry,
        reverse: &Reverse,
        card: &Card,
    ) -> Result<Option<String>, ActionError> {
        let changed = |what: &str| Some(format!("The card has changed since ({what}); change it from the card instead."));
        let tags_among = |managed: &[String]| {
            let mut tags: Vec<String> = card
                .tags
                .iter()
                .map(|t| t.trim_start_matches('#').to_lowercase())
                .filter(|tag| managed.iter().any(|m| m.eq_ignore_ascii_case(tag)))
                .collect();
            tags.sort();
            tags
        };
        let state = match reverse {
            Reverse::CloseCreated => card.closed.then(|| changed("it's closed already")).flatten(),
            Reverse::Tags { managed, after, .. } => (tags_among(managed) != *after).then(|| changed("its tags")).flatten(),
            Reverse::Place { after, .. } => (crate::pages::state_key(card) != *after).then(|| changed("it was moved")).flatten(),
            Reverse::Step { id, before } => {
                let now = card.steps.iter().find(|step| &step.id == id).map(|step| step.completed);
                (now != Some(!before)).then(|| changed("the step")).flatten()
            }
            Reverse::DeleteComment { .. } => None,
            Reverse::Title { after, .. } => (card.title.trim() != after.trim()).then(|| changed("its title")).flatten(),
            Reverse::Reopen => (!card.closed).then(|| changed("it's open again")).flatten(),
            Reverse::Close => card.closed.then(|| changed("it's closed")).flatten(),
        };
        if state.is_some() {
            return Ok(state);
        }
        let since = entry.at + MARGIN;
        let belongs = |reference: Option<&str>| {
            reference.is_some() && (reference == entry.proposal.as_deref() || reference == Some(entry.id.as_str()))
        };
        let later = self.journal.card_writes_after(card.number, since);
        if later.iter().any(|write| write.outcome == "ok" && !belongs(write.reference.as_deref())) {
            return Ok(Some("Someone changed the card from Campfire since; change it from the card instead.".into()));
        }
        let own: Vec<_> = self.journal.recent().into_iter().filter(|write| belongs(write.reference.as_deref())).collect();
        // The board's feed, newest first, page after page until it reaches the action (or ends).
        let path = Client::activities_path(&writer.account, &writer.board.id, None);
        let hermes = self.hermes_fizzy_user();
        let grace = if matches!(reverse, Reverse::CloseCreated) { entry.at + CREATION_GRACE } else { since };
        for page in 1..=UNDO_FEED_PAGES {
            let (activities, next): (Vec<Activity>, bool) = writer
                .client
                .get_page(&path, page)
                .await
                .map_err(|_| ActionError::Fizzy("Fizzy can't be reached to check the card's history; nothing was undone.".into()))?;
            let mut reached = false;
            for activity in &activities {
                let Some(at) = activity.created_at else { continue };
                if at <= since {
                    reached = true;
                    continue;
                }
                if hermes_log::activity_card(activity) != Some(card.number) || entry.activity.as_deref() == Some(activity.id.as_str()) {
                    continue;
                }
                let creator = activity.creator.clone().unwrap_or_default();
                if hermes.as_deref() == Some(creator.id.as_str()) && at <= grace {
                    continue;
                }
                if hermes_log::made_by_campfire(activity, &own, true) {
                    continue;
                }
                let who = if creator.name.trim().is_empty() { "Someone".to_string() } else { creator.name };
                return Ok(Some(format!("{who} changed the card in Fizzy since; change it from the card instead.")));
            }
            if reached || !next {
                return Ok(None);
            }
        }
        Err(ActionError::invalid(
            "cannot_undo",
            "There's been too much activity on the incident board since to check that nobody changed the card; change it from the card instead.",
        ))
    }

    /// The pending proposals, newest first.
    pub fn pending_proposals(&self) -> Vec<Proposal> {
        self.expire_proposals(self.now());
        self.proposals.all().into_iter().filter(|p| p.status == Status::Pending).collect()
    }

    /// `{"<id>": {"status", "label"}}` for the drafts' buttons (unknown ids are left out).
    pub fn proposal_states(&self, ids: &[String]) -> Value {
        self.expire_proposals(self.now());
        let mut states = serde_json::Map::new();
        for id in ids {
            if let Some(proposal) = self.proposals.get(id) {
                states.insert(id.clone(), json!({ "status": proposal.status.as_str(), "label": proposal.state_label() }));
            }
        }
        Value::Object(states)
    }

    /// What Hermes (the bot route) is told about its proposal.
    pub fn proposal_status(&self, id: &str) -> Option<Value> {
        self.expire_proposals(self.now());
        self.proposals.get(id).map(|proposal| proposal_json(&proposal))
    }

    /// The Hermes tab for `viewer`, filtered (`created`, `tags`, `moves`, `comments`, `questions`,
    /// `failures`; anything else is everything).
    pub async fn hermes_page(&self, viewer: &Viewer, filter: &str, source: &dyn ChatSource) -> Result<HermesPage, String> {
        let now = self.now();
        let messages = source.recent_messages(now - MESSAGES_WINDOW).await?;
        let rooms = source.room_ids().await?;
        Ok(self.hermes_view(viewer, filter, &messages, &rooms, now))
    }

    /// Whether `viewer` may see a proposal's details (its text, the person, the room): the members
    /// of its room (`rooms`: the viewer's) and the duty managers. Without a room (no context from
    /// the bridge, a Fizzy comment): the duty managers only.
    pub fn sees_proposal(&self, viewer: &Viewer, room_id: Option<i64>, rooms: &[i64]) -> bool {
        self.settings().is_duty_manager(viewer) || room_id.is_some_and(|room| rooms.contains(&room))
    }

    fn hermes_view(&self, viewer: &Viewer, filter: &str, messages: &[ChatMessage], rooms: &[i64], now: Timestamp) -> HermesPage {
        let filter = FILTERS.iter().find(|(key, _)| *key == filter).map(|(key, _)| *key).unwrap_or("all");
        let snapshot = self.snapshot();
        let settings = self.settings();
        let card_url = |number: u64| crate::chips::card_link(&self.config, &snapshot, number);
        let entries = self.hermes_log.entries();
        let undos: std::collections::HashMap<&str, &Entry> =
            entries.iter().filter(|entry| entry.kind == Kind::Undone).filter_map(|entry| Some((entry.target.as_deref()?, entry))).collect();
        let mut items: Vec<LogItem> = Vec::new();
        for entry in &entries {
            if !(filter == "all" || entry.kind.filter() == filter) {
                continue;
            }
            // What never became a card (proposed, refused, failed, dismissed…) carries the
            // proposal's text: its room's members and the duty managers only.
            let unfiled =
                matches!(entry.kind, Kind::Proposed | Kind::Refused | Kind::Failed | Kind::Dismissed | Kind::Expired | Kind::Superseded);
            if unfiled && entry.proposal.is_some() && !self.sees_proposal(viewer, entry.room_id, rooms) {
                continue;
            }
            let undone = undos.get(entry.id.as_str()).copied();
            let (undo_url, undo_note) = match (&entry.reverse, &undone) {
                (_, Some(_)) => (None, None),
                (None, None) => (None, None),
                (Some(_), None) if now.duration_since(entry.at) > hermes_log::UNDO_WINDOW => (None, None),
                (Some(_), None) if !settings.permits(&Act::Undo { for_user_id: entry.for_user_id }, viewer) => {
                    (None, Some("Duty managers and the person it was for can undo this.".to_string()))
                }
                (Some(_), None) => (Some(undo_path(&entry.id)), None),
            };
            items.push(LogItem {
                id: entry.id.clone(),
                at: entry.at.to_string(),
                sort_at: entry.at,
                via: match entry.via {
                    Via::Direct => "direct",
                    Via::Campfire => "via Campfire",
                },
                tone: match entry.via {
                    Via::Direct => "direct",
                    Via::Campfire => "campfire",
                },
                failed: matches!(entry.kind, Kind::Failed | Kind::Refused),
                text: entry.text.clone(),
                card_url: entry.card.and_then(card_url),
                for_label: for_label(entry),
                source: entry.source.clone().unwrap_or_else(|| "source unknown".into()),
                by_label: entry.by_name.clone().map(|name| match entry.kind {
                    Kind::Undone => format!("undone by {name}"),
                    Kind::Dismissed => format!("dismissed by {name}"),
                    _ => format!("confirmed by {name}"),
                }),
                error: entry.error.clone(),
                undo_url,
                undo_note,
                undone_by: undone.map(|undo| undo.by_name.clone().unwrap_or_else(|| "someone".into())),
            });
        }
        // The viewer's rooms: Hermes's drafts, questions and errors (not stored, not shown to
        // anyone else).
        let bots = self.bots();
        for message in messages.iter().filter(|m| m.creator_is_bot && bots.iter().any(|bot| bot.id == m.creator_id)) {
            let Some((kind, text)) = classify(message) else { continue };
            if !(filter == "all" || filter == kind) {
                continue;
            }
            items.push(LogItem {
                id: format!("m-{}", message.id),
                at: message.created_at.to_string(),
                sort_at: message.created_at,
                via: "in Campfire",
                tone: "chat",
                failed: kind == "failures",
                text,
                card_url: None,
                for_label: None,
                source: format!("in {}", message.room_name),
                by_label: None,
                error: None,
                undo_url: None,
                undo_note: None,
                undone_by: None,
            });
        }
        // By time, not by the text of it (whose fractional seconds vary in length).
        items.sort_by(|a, b| b.sort_at.cmp(&a.sort_at).then_with(|| b.id.cmp(&a.id)));
        items.truncate(hermes_log::MAX_SHOWN);
        let hermes_known = self.hermes_fizzy_user().is_some();
        HermesPage {
            filters: FILTERS
                .iter()
                .map(|(key, label)| FilterLink {
                    label,
                    url: if *key == "all" { HERMES_PATH.to_string() } else { format!("{HERMES_PATH}?filter={key}") },
                    selected: *key == filter,
                })
                .collect(),
            filtered: filter != "all",
            pending: self.pending_items(viewer, rooms),
            items,
            direct_unknown: !hermes_known,
            shared_token: self.shares_hermes_token(),
            fizzy: match (&snapshot.last_success, &snapshot.last_error) {
                (Some(_), None) => FizzyStatus::Ok,
                (Some(since), Some(_)) => FizzyStatus::Stale { since: since.to_string() },
                (None, error) => FizzyStatus::Waiting { failed: error.is_some() },
            },
            retry_seconds: self.config.poll_interval.as_secs(),
            settings_url: viewer.administrator.then(|| crate::pages::SETTINGS_PATH.to_string()),
        }
    }

    /// Pending proposals as the Hermes tab and Home list them, with buttons for those `viewer` may
    /// decide: those of the viewer's `rooms`, and all of them for duty managers
    /// ([`Workspace::sees_proposal`]); the others aren't listed.
    pub fn pending_items(&self, viewer: &Viewer, rooms: &[i64]) -> Vec<PendingItem> {
        let settings = self.settings();
        let can_decide = |proposal: &Proposal| settings.permits(&Act::ConfirmDraft { reporter_id: proposal.context.user_id }, viewer);
        let bots = self.bots();
        self.pending_proposals()
            .into_iter()
            .filter(|proposal| self.sees_proposal(viewer, proposal.context.room_id, rooms))
            .map(|proposal| {
                let message_url = match (proposal.context.room_id, proposal.draft_message_id) {
                    (Some(room), Some(message)) => Some(format!("/rooms/{room}/@{message}")),
                    _ => None,
                };
                let buttons = bots
                    .iter()
                    .find(|bot| bot.id == proposal.bot_id)
                    .filter(|_| can_decide(&proposal))
                    .map(|bot| drafts::proposal_buttons(&proposal, bot, drafts::Place::Elsewhere(message_url.as_deref())))
                    .unwrap_or_default();
                PendingItem {
                    id: proposal.id.clone(),
                    summary: proposal.summary.clone(),
                    details: proposal.details.clone(),
                    for_label: proposal.context.user_name.clone(),
                    room_name: proposal.context.room_name.clone(),
                    source: proposal.context.source_label(),
                    message_url,
                    created_at: proposal.created_at.to_string(),
                    expires_at: proposal.expires_at().to_string(),
                    buttons,
                    decide_note: (!can_decide(&proposal)).then(|| settings.confirm_policy.label().to_string()),
                }
            })
            .collect()
    }
}

/// `POST` here to undo a log entry.
pub fn undo_path(id: &str) -> String {
    format!("/workspace/hermes/actions/{id}/undo")
}

const FILTERS: [(&str, &str); 7] = [
    ("all", "Everything"),
    ("created", "Cards created"),
    ("tags", "Tags"),
    ("moves", "Moves and closes"),
    ("comments", "Comments"),
    ("questions", "Questions asked"),
    ("failures", "Failures"),
];

/// A bot message worth a line on the Hermes tab: `(filter, text)`.
fn classify(message: &ChatMessage) -> Option<(&'static str, String)> {
    if proposals::marker_in(&message.body_html).is_some() {
        return None;
    }
    let text = html::to_text(&message.body_html);
    let lower = text.to_lowercase();
    if let Some(draft) = drafts::detect(&message.body_html) {
        return Some(("other", format!("Drafted an incident in {}: {}", message.room_name, draft.title)));
    }
    if lower.contains("could not reach the agent")
        || lower.contains("could not transcribe")
        || lower.starts_with("sorry —")
        || lower.starts_with("désolé")
    {
        return Some(("failures", format!("Couldn't answer in {}: {}", message.room_name, html::truncate(&text.replace('\n', " "), 140))));
    }
    let last = text.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("");
    if last.trim_end().ends_with('?') {
        return Some(("questions", format!("Asked in {}: {}", message.room_name, html::truncate(last.trim(), 160))));
    }
    None
}

fn for_label(entry: &Entry) -> Option<String> {
    let name = entry.for_name.as_deref()?;
    Some(match (entry.via, entry.for_user_id) {
        (Via::Direct, None) => format!("for {name} (from the report)"),
        _ => format!("for {name}"),
    })
}

fn entry_actor(entry: &Entry) -> &'static str {
    match entry.via {
        Via::Direct | Via::Campfire => "Hermes",
    }
}

fn card_label(card: &Card) -> String {
    let title = card.title.trim();
    if title.is_empty() { format!("#{}", card.number) } else { format!("#{} {}", card.number, html::truncate(title, 80)) }
}

fn done_kind(action: &str) -> Kind {
    match action {
        "create" => Kind::Created,
        "close" => Kind::Closed,
        "move" => Kind::Moved,
        "severity" | "departments" => Kind::Tagged,
        "step" => Kind::Step,
        "comment" => Kind::Commented,
        _ => Kind::Other,
    }
}

/// A log entry about a proposal (`p-<id>-<event>`), with who it's for and where it came from.
fn proposal_entry(proposal: &Proposal, event: &str, at: Timestamp, kind: Kind, text: String) -> Entry {
    let mut entry = Entry::new(format!("p-{}-{event}", proposal.id), at, kind, Via::Campfire, text);
    entry.card = proposal.card;
    entry.for_user_id = proposal.context.user_id;
    entry.for_name = proposal.context.user_name.clone();
    entry.source = proposal.context.source_label();
    entry.proposal = Some(proposal.id.clone());
    entry.room_id = proposal.context.room_id;
    entry
}

/// A proposal as the bot route answers it.
pub fn proposal_json(proposal: &Proposal) -> Value {
    json!({
        "status": proposal.status.as_str(),
        "id": proposal.id,
        "action": proposal.action,
        "card": proposal.result_card.or(proposal.card),
        "url": proposal.result_url,
        "message": proposal.state_label(),
        "warning": if proposal.status == Status::Done { proposal.message.clone() } else { None },
        "decided_by": proposal.decided_by_name,
        "draft_message_id": proposal.draft_message_id,
    })
}

// --- The page -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterLink {
    pub label: &'static str,
    pub url: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingItem {
    pub id: String,
    pub summary: String,
    pub details: Vec<String>,
    pub for_label: Option<String>,
    pub room_name: Option<String>,
    pub source: Option<String>,
    /// The draft in its room.
    pub message_url: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    /// Confirm / Edit / Dismiss, for those the policy lets decide.
    pub buttons: String,
    /// Who can decide, for the others.
    pub decide_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogItem {
    pub id: String,
    pub at: String,
    /// `at`, to sort by.
    pub sort_at: Timestamp,
    /// "direct", "via Campfire", "in Campfire".
    pub via: &'static str,
    pub tone: &'static str,
    pub failed: bool,
    pub text: String,
    pub card_url: Option<String>,
    pub for_label: Option<String>,
    pub source: String,
    pub by_label: Option<String>,
    pub error: Option<String>,
    pub undo_url: Option<String>,
    pub undo_note: Option<String>,
    pub undone_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/hermes.html")]
pub struct HermesPage {
    pub filters: Vec<FilterLink>,
    pub filtered: bool,
    pub pending: Vec<PendingItem>,
    pub items: Vec<LogItem>,
    /// Hermes's Fizzy user isn't known: its direct actions can't be shown.
    pub direct_unknown: bool,
    /// The workspace writes with Hermes's own token.
    pub shared_token: bool,
    pub fizzy: FizzyStatus,
    pub retry_seconds: u64,
    pub settings_url: Option<String>,
}

/// The proposals' JSON for `GET /workspace/hermes/proposals.json`.
pub fn states_json(states: Value) -> Value {
    json!({ "proposals": states })
}

/// Ids from `?ids=a,b` (at most 50, lowercase letters and digits).
pub fn parse_ids(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
        .take(50)
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
