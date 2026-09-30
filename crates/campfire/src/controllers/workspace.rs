//! Hermes fork: the Duty Manager Workspace's adapter (docs/hermes-workspace.md). Not in the
//! reference. The feature lives in `campfire_workspace`; this file is the only place that knows
//! both sides:
//!
//! - [`start`]: at boot, the bots, the render hooks (`campfire_views::hermes::WorkspaceHooks`) and
//!   the Fizzy poll task, over the shared `integrations::net` client ([`FizzyHttp`]); every Fizzy
//!   write is logged ([`log_write`]);
//! - `GET /workspace`: the Home page, in the application layout;
//! - `GET /workspace/cards.json?numbers=12,13`: current chips, for the page to refresh them;
//! - `POST /workspace/drafts/:message_id/reply` (`{"decision": "confirm" | "dismiss"}`): answers a
//!   Hermes draft in its room as the current user, through `MessagesController#create`'s path;
//! - `GET /workspace/board`, `GET /workspace/cards/:number`, `GET /workspace/cards/new`,
//!   `GET /workspace/rooms/:room_id/panel`: the board, a card's sheet, the new-card form, a room's
//!   cards panel (the last three as fragments with `?fragment=1`, for the page's overlays);
//! - `POST /workspace/cards` and `POST /workspace/cards/:number/:change`: create and change cards;
//! - `GET`/`POST /workspace/settings`: the settings (administrators);
//! - `GET /hermes/:bot_key/workspace/settings.json`: the settings, for Hermes (bots only).
//!
//! All answer 404 while the workspace is off (no `FIZZY_URL`/`FIZZY_TOKEN`), and run
//! `ApplicationController`'s chain (session only and no bots, except the bot route;
//! `Sec-Fetch-Site` forgery protection on the POSTs). Who may change what is the settings' policy
//! (`Workspace::authorize`), not decided here.

use std::sync::{Arc, Weak};
use std::time::Duration;

use askama::Template;
use campfire_db::{Message, Room, User};
use campfire_kit::{Ctx, Error, Param, Result, StatusCode, format, halt};
use campfire_richtext::uri;
use campfire_views::helpers as h;
use campfire_views::layouts::Application;
use campfire_views::{ViewContext, hermes::WorkspaceHooks};
use campfire_workspace::actions::{CardSource, Change, NewCard};
use campfire_workspace::drafts::{self, Decision};
use campfire_workspace::fizzy::{self, HttpResponse};
use campfire_workspace::overlay::{self, TabBar};
use campfire_workspace::pages::{self, BoardFilter, FormSource};
use campfire_workspace::settings::SaveError;
use campfire_workspace::{Act, ActionError, Bot, BoxFuture, ChatMessage, ChatSource, Settings, Viewer, Workspace, WriteRecord};
use serde_json::{Value, json};

use crate::app::{App, AppCtx, AppState};
use crate::concerns::{self, Before, before_actions, require_current_user};
use crate::controllers::messages::{self, MessageParams};
use crate::controllers::presenters::Presenter;
use crate::controllers::presenters::accounts::attachable_sgid;
use crate::controllers::presenters::page::db_error;
use crate::controllers::presenters::view_context::{find_template, page_in_any_format};
use crate::integrations::net::Network;
use crate::integrations::net::http::{self, Body, Endpoint, Timeouts};

/// The most messages the Home page reads for drafts (the last day of the user's rooms).
const RECENT_MESSAGES: i64 = 500;
/// The most chips one request asks for.
const MAX_CHIPS: usize = 50;

// --- Boot -----------------------------------------------------------------------------------------

/// The workspace from the config (and its settings file), `None` while it's off.
pub fn build(app_config: &crate::config::Config) -> Option<Arc<Workspace>> {
    app_config.workspace.clone().map(|config| Arc::new(Workspace::new(config).with_audit(log_write)))
}

/// Every Fizzy write, in the server log: who (Campfire user), which card, what, through whose
/// token, and how it went. Never the token.
fn log_write(record: &WriteRecord) {
    tracing::info!(
        user_id = record.user_id,
        card = record.card.map(|number| number.to_string()).unwrap_or_else(|| "new".into()),
        action = %record.action,
        identity = record.identity,
        outcome = %record.outcome,
        "workspace wrote to Fizzy"
    );
}

/// Loads the bots, installs the render hooks and starts polling Fizzy; while the workspace is off,
/// makes sure no hooks are installed (so pages render exactly as upstream's).
pub async fn start(app: &App) {
    let Some(workspace) = app.workspace.clone() else {
        campfire_views::hermes::install_workspace_hooks(None);
        return;
    };
    refresh_bots(app, &workspace).await;
    let hooks = Hooks { workspace: workspace.clone(), voice: app.gemini_live.is_some() };
    campfire_views::hermes::install_workspace_hooks(Some(Arc::new(hooks)));
    let config = workspace.config();
    tracing::info!(
        fizzy_url = %config.fizzy_url,
        board = %config.incident_board,
        poll_seconds = config.poll_interval.as_secs(),
        settings = %workspace.settings_store().path().display(),
        "Duty Manager Workspace on"
    );
    if let Some(error) = workspace.settings_store().load_error() {
        tracing::warn!(
            %error,
            "the workspace settings couldn't be read; duty managers only and no departments until an administrator saves them"
        );
    }
    tokio::spawn(poll_loop(Arc::downgrade(app), workspace));
}

/// Polls every `FIZZY_POLL_S` for as long as the app lives. Logs when Fizzy goes up or down, not
/// on every failed poll, and single card or user lookups that failed when they change (they're
/// retried every poll).
async fn poll_loop(app: Weak<AppState>, workspace: Arc<Workspace>) {
    let http = FizzyHttp::new();
    let mut interval = tokio::time::interval(workspace.config().poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_ok: Option<bool> = None;
    let mut last_lookup_errors: Vec<String> = Vec::new();
    loop {
        interval.tick().await;
        let Some(app) = app.upgrade() else { return };
        refresh_bots(&app, &workspace).await;
        let now = app.clock.now();
        drop(app);
        match workspace.poll(&http, now).await {
            Ok(()) if last_ok != Some(true) => {
                let snapshot = workspace.snapshot();
                let board = snapshot.board.as_ref().map(|board| board.name.as_str()).unwrap_or("?");
                tracing::info!(board, open_cards = snapshot.open.len(), "Fizzy polled");
                last_ok = Some(true);
            }
            Ok(()) => {}
            Err(error) if last_ok != Some(false) => {
                tracing::warn!(%error, "could not poll Fizzy; the workspace shows what it last saw");
                last_ok = Some(false);
            }
            Err(error) => tracing::debug!(%error, "could not poll Fizzy"),
        }
        // Never carry the token (`FizzyError`).
        let lookup_errors = workspace.snapshot().lookup_errors.clone();
        if !lookup_errors.is_empty() && lookup_errors != last_lookup_errors {
            tracing::warn!(errors = %lookup_errors.join("; "), "some Fizzy lookups failed; retrying at the next poll");
        }
        last_lookup_errors = lookup_errors;
    }
}

/// The active bots, which post the drafts, with their mention sgids.
async fn refresh_bots(app: &App, workspace: &Workspace) {
    let secrets = app.secrets.clone();
    match app.db.read(User::active_bots_ordered).await {
        Ok(bots) => workspace
            .set_bots(bots.into_iter().map(|bot| Bot { sgid: attachable_sgid(&secrets, bot.id), id: bot.id, name: bot.name }).collect()),
        Err(error) => tracing::warn!(%error, "could not load the bots for the workspace"),
    }
}

// --- Render hooks ---------------------------------------------------------------------------------

struct Hooks {
    workspace: Arc<Workspace>,
    /// The live voice report is on (`GEMINI_API_KEY`): the tab bar's Report opens it.
    voice: bool,
}

impl WorkspaceHooks for Hooks {
    fn layout_overlay(&self, ctx: &ViewContext) -> String {
        if !ctx.current_user.as_ref().is_some_and(|user| !user.bot) {
            return String::new();
        }
        let Some(user) = ctx.current_user.as_ref() else { return String::new() };
        let path = ctx.request_url.strip_prefix(&ctx.base_url).unwrap_or("/");
        let path = path.split(['?', '#']).next().unwrap_or("/");
        let (active, room_id) = overlay::locate(path);
        let report_room = room_id.or(ctx.last_room_visited_id);
        let viewer = Viewer { id: user.id, name: user.name.clone(), email: None, administrator: user.administrator };
        // The room on screen (the user's, or they wouldn't see its page) linked to a department.
        let panel = overlay::room_page(path)
            .and_then(|room| self.workspace.room_panel(&viewer, room))
            .and_then(|panel| panel.render().ok())
            .unwrap_or_default();
        let bar = TabBar {
            active,
            // The live voice page has its own bottom bar.
            show_bar: !path.ends_with("/voice"),
            home_url: overlay::HOME_PATH.into(),
            board_url: pages::BOARD_PATH.into(),
            chats_url: ctx.last_room_visited_id.map(campfire_routes::room).unwrap_or_else(|| "/".into()),
            report_url: report_room.filter(|_| self.voice).map(overlay::voice_path),
            cards_url: overlay::CARDS_PATH.into(),
            stylesheet_url: ctx.asset("hermes/workspace.css"),
            script_url: ctx.asset("hermes/workspace.js"),
            home_icon: ctx.asset("hermes/home.svg"),
            board_icon: ctx.asset("hermes/board.svg"),
            chats_icon: ctx.asset("messages-outlined.svg"),
            report_icon: ctx.asset("headset.svg"),
            panel,
            can_create: self.workspace.may(&Act::CreateCard, &viewer),
        };
        bar.render().unwrap_or_default()
    }

    fn message_html(&self, message: &campfire_views::messages::MessageView, html: &str) -> Option<String> {
        self.workspace.decorate_message(message.id, message.creator.id, html)
    }
}

// --- Actions --------------------------------------------------------------------------------------

pub async fn show(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    find_template(c, &format::HTML)?;
    let user = require_current_user(c)?.clone();
    let viewer = viewer(&user);
    let source = RoomMessages { app: c.app().clone(), user };
    let home = workspace.home(&viewer, &source, c.now()).await.map_err(|error| Error::internal(anyhow::anyhow!(error)))?;
    let content = home.render().map_err(Error::internal)?;
    page(c, StatusCode::OK, "Home", content).await
}

/// A workspace page in the application layout, with the nav back to the chats.
async fn page(c: &mut Ctx, status: StatusCode, title: &str, content: String) -> Result {
    page_in_any_format(c, status, |ctx| {
        let nav = overlay::HomeNav {
            title: title.to_string(),
            chats_url: ctx.last_room_visited_id.map(campfire_routes::room).unwrap_or_else(|| "/".into()),
            back_icon: ctx.asset("arrow-left.svg"),
        };
        Application {
            ctx,
            page_title: Some(title.to_string()),
            body_class: None,
            head: h::empty(),
            nav: h::raw(nav.render()?),
            content: h::raw(content),
            footer: h::empty(),
            sidebar: h::empty(),
        }
        .render()
    })
    .await
}

fn viewer(user: &User) -> Viewer {
    Viewer { id: user.id, name: user.name.clone(), email: user.email_address.clone(), administrator: user.is_administrator() }
}

pub async fn cards(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let numbers: Vec<u64> = c
        .params
        .get("numbers")
        .and_then(Param::as_str)
        .unwrap_or("")
        .split(',')
        .filter_map(|number| number.trim().parse().ok())
        .take(MAX_CHIPS)
        .collect();
    c.json(StatusCode::OK, &json!({ "cards": workspace.chips(&numbers) }))
}

pub async fn reply(c: &mut Ctx) -> Result {
    feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = require_current_user(c)?.clone();
    let id: i64 = c.params.get("message_id").and_then(Param::as_str).and_then(|id| id.parse().ok()).ok_or(Error::NotFound)?;
    let Some(decision) = c.request_params.get("decision").and_then(Param::as_str).and_then(Decision::parse) else {
        return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "invalid_decision", "Decision must be confirm or dismiss.");
    };
    let user_id = user.id;
    let (room, bot, body, reporter_id) = c
        .app()
        .db
        .read(move |conn| {
            // Current.user.reachable_messages.find: 404 outside the user's rooms.
            let message = Message::find_reachable(conn, user_id, id)?;
            let reporter = reporter_of(conn, &message)?;
            Ok((message.room(conn)?, message.creator(conn)?, message.body_html(conn)?.unwrap_or_default(), reporter))
        })
        .await
        .map_err(db_error)?;
    if !(bot.is_bot() && bot.is_active()) || drafts::detect(&body).is_none() {
        return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "not_a_draft", "This message isn't a draft awaiting confirmation.");
    }
    let workspace = feature(c)?;
    if let Err(error) = workspace.authorize(&Act::ConfirmDraft { reporter_id }, &viewer(&user)) {
        return action_error(c, &error);
    }

    let body = decision.reply_html(&attachable_sgid(&c.app().secrets, bot.id));
    // MessagesController#create's path; the bot's mention makes `deliver_webhooks_to_bots` pick it
    // in a shared room.
    let message = messages::create_message(c, &room, MessageParams { body: Some(body), ..MessageParams::default() }).await?;
    messages::broadcast_create(c, &room, &message).await?;
    messages::deliver_webhooks_to_bots(c, &room, &message).await?;
    tracing::info!(room_id = room.id, user_id, draft_id = id, decision = decision.reply(), "answered a Hermes draft");
    c.json(StatusCode::CREATED, &json!({ "message_id": message.id, "reply": decision.reply() }))
}

/// Who reported what a draft is about: the last person (not a bot) who wrote in the room before
/// it. The policy's "author" of a draft.
fn reporter_of(conn: &campfire_db::Connection, draft: &Message) -> campfire_db::Result<Option<i64>> {
    let mut statement = conn.prepare_cached(concat!(
        r#"SELECT "messages"."creator_id" FROM "messages" INNER JOIN "users" ON "users"."id" = "messages"."creator_id""#,
        r#" WHERE "messages"."room_id" = ? AND "messages"."id" != ? AND "messages"."created_at" <= ? AND "users"."role" != 2"#,
        r#" ORDER BY "messages"."created_at" DESC, "messages"."id" DESC LIMIT 1"#
    ))?;
    let mut rows = statement.query_map(rusqlite::params![draft.room_id, draft.id, draft.created_at], |row| row.get(0))?;
    Ok(rows.next().transpose()?)
}

/// `GET /workspace/board?dept=<tag>&sev[]=critical&sev[]=high` (or `sev=critical,high`).
pub async fn board(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    find_template(c, &format::HTML)?;
    let user = require_current_user(c)?.clone();
    let severities: Vec<String> = match c.params.get("sev") {
        Some(Param::Array(values)) => values.iter().filter_map(Param::as_str).map(str::to_string).collect(),
        Some(value) => value.as_str().map(|value| vec![value.to_string()]).unwrap_or_default(),
        None => Vec::new(),
    };
    let filter = BoardFilter::parse(c.params.get("dept").and_then(Param::as_str), &severities, &workspace.settings());
    let content = workspace.board(&viewer(&user), &filter).render().map_err(Error::internal)?;
    page(c, StatusCode::OK, "Board", content).await
}

/// `GET /workspace/cards/:number`: the card's sheet, read from Fizzy (a page, or the overlay's
/// fragment with `?fragment=1`).
pub async fn card(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = require_current_user(c)?.clone();
    let number = card_number(c)?;
    let fragment = wants_fragment(c);
    if !fragment {
        find_template(c, &format::HTML)?;
    }
    let sheet = workspace.card_sheet(&FizzyHttp::new(), &viewer(&user), number).await;
    let (status, html) = match sheet {
        Ok(sheet) => (StatusCode::OK, sheet.render().map_err(Error::internal)?),
        Err(ActionError::NotFound) if !fragment => return Err(Error::NotFound),
        Err(error) => (status_of(&error), notice(&error.message())),
    };
    if fragment {
        return Ok(c.render_html(status, html));
    }
    page(c, status, &format!("No. {number}"), format!(r#"<section class="ws-home ws-sheet-page">{html}</section>"#)).await
}

/// `GET /workspace/cards/new?message_id=…` (or `?room_id=…`): the new-card form, prefilled from
/// the message (a page, or a fragment with `?fragment=1`). `404` for a message or room that isn't
/// the user's, then `403` (a notice) when the policy doesn't let them create cards.
pub async fn new_card(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = require_current_user(c)?.clone();
    let fragment = wants_fragment(c);
    if !fragment {
        find_template(c, &format::HTML)?;
    }
    let (message_id, room_id) = (id_param(c.params.get("message_id")), id_param(c.params.get("room_id")));
    let (app, reader) = (c.app().clone(), user.clone());
    let found =
        c.app().db.read(move |conn| source_of(conn, &app, &reader, message_id, room_id)).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    if let Err(error) = workspace.authorize(&Act::CreateCard, &viewer(&user)) {
        let html = notice(&error.message());
        if fragment {
            return Ok(c.render_html(StatusCode::FORBIDDEN, html));
        }
        return page(c, StatusCode::FORBIDDEN, "New card", format!(r#"<section class="ws-home ws-sheet-page">{html}</section>"#)).await;
    }
    let form_source = found.message.as_ref().map(|message| FormSource {
        message_id: message.id,
        author_name: message.author_name.clone(),
        room_name: found.room_name.clone().unwrap_or_default(),
    });
    let text = found.message.as_ref().map(|message| campfire_workspace::text_of(&message.body_html));
    let html =
        workspace.new_card_form(found.room.as_ref().map(|room| room.id), text.as_deref(), form_source).render().map_err(Error::internal)?;
    if fragment {
        return Ok(c.render_html(StatusCode::OK, html));
    }
    page(c, StatusCode::OK, "New card", format!(r#"<section class="ws-home ws-sheet-page">{html}</section>"#)).await
}

/// `POST /workspace/cards` `{"title", "description", "severity", "department", "message_id" |
/// "room_id"}`: creates the card on the incident board, then posts its link in the room as the
/// user (it renders as a chip). `201 {"number", "url", "chip", "message_id", "warning"}`.
pub async fn create_card(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = require_current_user(c)?.clone();
    let params = c.request_params.to_json();
    let mut new = match NewCard::parse(&params, &workspace.settings()) {
        Ok(new) => new,
        Err(error) => return action_error(c, &error),
    };
    let (message_id, room_id) = (id_param(c.params.get("message_id")), id_param(c.params.get("room_id")));
    let (app, reader) = (c.app().clone(), user.clone());
    let Some(found) = c.app().db.read(move |conn| source_of(conn, &app, &reader, message_id, room_id)).await.map_err(db_error)? else {
        return json_error(c, StatusCode::NOT_FOUND, "not_found", "That message or room isn't one of yours.");
    };
    new.source = found.message.as_ref().map(|message| CardSource {
        message_url: c.url_for(&campfire_routes::room_at_message(message.room_id, message.id)),
        room_name: found.room_name.clone().unwrap_or_default(),
        author_name: message.author_name.clone(),
    });
    let created = match workspace.create_card(&FizzyHttp::new(), &viewer(&user), new).await {
        Ok(created) => created,
        Err(error) => return action_error(c, &error),
    };

    let mut warning = created.warning.clone();
    let mut posted = None;
    if let Some(room) = &found.room {
        // Posted as the user, like MessagesController#create, without the bot webhooks: a link
        // to the card isn't something to ask Hermes about.
        let link = h::escape(&created.url);
        let body = format!(r#"<p>New card: <a href="{link}">{link}</a></p>"#);
        let result = async {
            let message = messages::create_message(c, room, MessageParams { body: Some(body), ..MessageParams::default() }).await?;
            messages::broadcast_create(c, room, &message).await?;
            Ok::<_, Error>(message.id)
        }
        .await;
        match result {
            Ok(id) => posted = Some(id),
            Err(error) => {
                tracing::warn!(%error, room_id = room.id, card = created.card.number, "couldn't post the new card's link in the room");
                let note = format!("Card #{} was created, but its link couldn't be posted in the room.", created.card.number);
                warning = Some(warning.map_or(note.clone(), |warning| format!("{warning} {note}")));
            }
        }
    }
    let chip = workspace.chip(created.card.number);
    c.json(
        StatusCode::CREATED,
        &json!({ "number": created.card.number, "url": created.url, "chip": chip, "message_id": posted, "warning": warning }),
    )
}

/// `POST /workspace/cards/:number/:change` (`move`, `severity`, `departments`, `step`,
/// `comment`; see `campfire_workspace::actions::Change`). `200 {"number", "sheet", "chip"}`: the
/// card's fresh sheet and chip.
pub async fn change_card(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = require_current_user(c)?.clone();
    let number = card_number(c)?;
    let kind = c.params.get("change").and_then(Param::as_str).unwrap_or("").to_string();
    let change = match Change::parse(&kind, &c.request_params.to_json(), &workspace.settings()) {
        None => return Err(Error::NotFound),
        Some(Err(error)) => return action_error(c, &error),
        Some(Ok(change)) => change,
    };
    let (http, viewer) = (FizzyHttp::new(), viewer(&user));
    if let Err(error) = workspace.change_card(&http, &viewer, number, change).await {
        return action_error(c, &error);
    }
    let sheet = match workspace.card_sheet(&http, &viewer, number).await {
        Ok(sheet) => Some(sheet.render().map_err(Error::internal)?),
        Err(_) => None,
    };
    c.json(StatusCode::OK, &json!({ "number": number, "sheet": sheet, "chip": workspace.chip(number) }))
}

/// `GET /workspace/rooms/:room_id/panel`: the room's cards panel (fragment), `204` when the room
/// isn't linked to a department; 404 outside the user's rooms.
pub async fn panel(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = require_current_user(c)?.clone();
    let room_id: i64 = c.params.get("room_id").and_then(Param::as_str).and_then(|id| id.parse().ok()).ok_or(Error::NotFound)?;
    let user_id = user.id;
    c.app().db.read(move |conn| Room::find_for_user(conn, user_id, room_id)).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    match workspace.room_panel(&viewer(&user), room_id) {
        Some(panel) => {
            let html = panel.render().map_err(Error::internal)?;
            Ok(c.render_html(StatusCode::OK, html))
        }
        None => Ok(c.head(StatusCode::NO_CONTENT)),
    }
}

/// `GET /workspace/settings` (administrators).
pub async fn settings(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    concerns::ensure_can_administer(c)?;
    find_template(c, &format::HTML)?;
    let directory = c.app().db.read(directory).await.map_err(db_error)?;
    let administrators: Vec<String> = directory.people.iter().filter(|person| person.2).map(|person| person.1.clone()).collect();
    let people: Vec<(i64, String)> = directory.people.iter().map(|(id, name, _)| (*id, name.clone())).collect();
    let content = pages::settings_page(
        workspace.config(),
        &workspace.snapshot(),
        &workspace.settings(),
        &directory.rooms,
        &people,
        &administrators,
        workspace.settings_store().load_error(),
    )
    .render()
    .map_err(Error::internal)?;
    page(c, StatusCode::OK, "Workspace settings", content).await
}

/// `POST /workspace/settings` `{"departments": [{"name", "tag", "rooms": [ids]}], "duty_managers":
/// null | [ids], "confirm_policy"}` (administrators). Room and user ids that aren't rooms or active
/// people are dropped. `200 {"ok": true}`, `422 {"error": "invalid_settings", "message"}`, or `500
/// {"error": "settings_not_saved", …}` when the file couldn't be written (nothing changed).
pub async fn update_settings(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    if !require_current_user(c)?.is_administrator() {
        return halt(concerns::head(StatusCode::FORBIDDEN));
    }
    let mut settings: Settings = match serde_json::from_value(settings_json(c.request_params.to_json())) {
        Ok(settings) => settings,
        Err(error) => {
            return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "invalid_settings", &format!("The settings aren't valid: {error}"));
        }
    };
    let directory = c.app().db.read(directory).await.map_err(db_error)?;
    for department in &mut settings.departments {
        department.rooms.retain(|id| directory.rooms.iter().any(|(room, _)| room == id));
    }
    if let Some(managers) = &mut settings.duty_managers {
        managers.retain(|id| directory.people.iter().any(|(person, ..)| person == id));
    }
    match workspace.settings_store().save(settings) {
        Ok(saved) => {
            let user_id = require_current_user(c)?.id;
            tracing::info!(
                user_id,
                departments = saved.departments.len(),
                policy = saved.confirm_policy.as_str(),
                "workspace settings saved"
            );
            c.json(StatusCode::OK, &json!({ "ok": true }))
        }
        Err(SaveError::Invalid(error)) => json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "invalid_settings", &error.0),
        Err(error @ SaveError::Io(_)) => {
            tracing::error!(%error, "the workspace settings couldn't be saved");
            let message =
                "Couldn't save the settings (the server couldn't write its file); nothing changed. Try again, or see the server log.";
            json_error(c, StatusCode::INTERNAL_SERVER_ERROR, "settings_not_saved", message)
        }
    }
}

/// `GET /hermes/:bot_key/workspace/settings.json` (bots only): the departments and their tags, the
/// duty managers and the policy, for Hermes's skill to tag cards by department.
pub async fn bot_settings(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default().allow_bot_access()).await?;
    if !require_current_user(c)?.is_bot() {
        return halt(concerns::head(StatusCode::FORBIDDEN));
    }
    let directory = c.app().db.read(directory).await.map_err(db_error)?;
    let settings = workspace.settings();
    let administrators: Vec<i64> = directory.people.iter().filter(|person| person.2).map(|person| person.0).collect();
    let managers: Vec<(i64, String)> = settings
        .duty_manager_ids(&administrators)
        .into_iter()
        .filter_map(|id| directory.people.iter().find(|person| person.0 == id).map(|person| (id, person.1.clone())))
        .collect();
    let body = pages::bot_settings(workspace.config(), &workspace.snapshot(), &settings, &directory.rooms, &managers);
    c.json(StatusCode::OK, &body)
}

/// The settings form's JSON as `Settings` reads it: ids sent as strings become numbers.
fn settings_json(mut params: Value) -> Value {
    let numbers = |ids: &mut Value| {
        if let Value::Array(ids) = ids {
            for id in ids.iter_mut() {
                if let Some(number) = id.as_str().and_then(|id| id.trim().parse::<i64>().ok()) {
                    *id = json!(number);
                }
            }
        }
    };
    if let Some(Value::Array(departments)) = params.get_mut("departments") {
        for department in departments {
            if let Some(rooms) = department.get_mut("rooms") {
                numbers(rooms);
            }
        }
    }
    if let Some(managers) = params.get_mut("duty_managers") {
        numbers(managers);
    }
    params
}

/// The rooms (not directs) and the active people (not bots), by name: `(id, name, administrator)`.
struct Directory {
    rooms: Vec<(i64, String)>,
    people: Vec<(i64, String, bool)>,
}

fn directory(conn: &campfire_db::Connection) -> campfire_db::Result<Directory> {
    let mut rooms: Vec<(i64, String)> = Room::all(conn)?
        .into_iter()
        .filter(|room| !room.direct())
        .map(|room| (room.id, room.name.clone().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| format!("Room {}", room.id))))
        .collect();
    rooms.sort_by_key(|(_, name)| name.to_lowercase());
    let people =
        User::active_ordered_without_bots(conn)?.into_iter().map(|user| (user.id, user.name.clone(), user.is_administrator())).collect();
    Ok(Directory { rooms, people })
}

/// What a card is created from: a message of one of the user's rooms, or one of their rooms.
struct Found {
    room: Option<Room>,
    room_name: Option<String>,
    message: Option<SourceMessage>,
}

struct SourceMessage {
    id: i64,
    room_id: i64,
    author_name: String,
    body_html: String,
}

/// `None` when the message or room isn't reachable by the user (404).
fn source_of(
    conn: &campfire_db::Connection,
    app: &App,
    user: &User,
    message_id: Option<i64>,
    room_id: Option<i64>,
) -> campfire_db::Result<Option<Found>> {
    let presenter = Presenter::new(conn, app, None);
    if let Some(id) = message_id {
        let Ok(message) = Message::find_reachable(conn, user.id, id) else { return Ok(None) };
        let room = message.room(conn)?;
        let source = SourceMessage {
            id: message.id,
            room_id: room.id,
            author_name: message.creator(conn)?.name,
            body_html: message.body_html(conn)?.unwrap_or_default(),
        };
        let name = presenter.room_view(&room, user)?.display_name;
        return Ok(Some(Found { room: Some(room), room_name: Some(name), message: Some(source) }));
    }
    if let Some(id) = room_id {
        let Some(room) = Room::find_for_user(conn, user.id, id)? else { return Ok(None) };
        let name = presenter.room_view(&room, user)?.display_name;
        return Ok(Some(Found { room: Some(room), room_name: Some(name), message: None }));
    }
    Ok(Some(Found { room: None, room_name: None, message: None }))
}

/// A positive id from a param given as a number or a string.
fn id_param(param: Option<&Param>) -> Option<i64> {
    let id = match param? {
        Param::Number(number) => number.as_i64(),
        param => param.as_str().and_then(|id| id.trim().parse().ok()),
    };
    id.filter(|id| *id > 0)
}

fn card_number(c: &Ctx) -> Result<u64> {
    c.params.get("number").and_then(Param::as_str).and_then(|number| number.parse().ok()).ok_or(Error::NotFound)
}

fn wants_fragment(c: &Ctx) -> bool {
    c.params.get("fragment").and_then(Param::as_str).is_some_and(|value| value == "1")
}

fn status_of(error: &ActionError) -> StatusCode {
    StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn notice(message: &str) -> String {
    format!(r#"<p class="ws-notice" role="alert">{}</p>"#, h::escape(message))
}

/// `{"error": code, "message"}` with the error's status.
fn action_error(c: &mut Ctx, error: &ActionError) -> Result {
    json_error(c, status_of(error), error.code(), &error.message())
}

/// The workspace, or 404 while it's off.
fn feature(c: &Ctx) -> Result<Arc<Workspace>> {
    c.app().workspace.clone().ok_or(Error::NotFound)
}

fn json_error(c: &mut Ctx, status: StatusCode, code: &str, message: &str) -> Result {
    c.json(status, &json!({ "error": code, "message": message }))
}

// --- What the workspace reads from Campfire -------------------------------------------------------

/// The Home page's messages: the user's rooms, newest first, bodies as stored.
struct RoomMessages {
    app: App,
    user: User,
}

impl ChatSource for RoomMessages {
    fn recent_messages(&self, since: jiff::Timestamp) -> BoxFuture<'_, std::result::Result<Vec<ChatMessage>, String>> {
        let (app, user) = (self.app.clone(), self.user.clone());
        Box::pin(async move {
            let reader = app.clone();
            reader.db.read(move |conn| recent_messages(conn, &app, &user, since)).await.map_err(|error| error.to_string())
        })
    }
}

fn recent_messages(
    conn: &campfire_db::Connection,
    app: &App,
    user: &User,
    since: jiff::Timestamp,
) -> campfire_db::Result<Vec<ChatMessage>> {
    let ids: Vec<i64> = {
        let mut statement = conn.prepare_cached(concat!(
            r#"SELECT "messages"."id" FROM "messages" INNER JOIN "memberships" ON "memberships"."room_id" = "messages"."room_id""#,
            r#" WHERE "memberships"."user_id" = ? AND "messages"."created_at" >= ? ORDER BY "messages"."created_at" DESC LIMIT ?"#
        ))?;
        let rows =
            statement.query_map(rusqlite::params![user.id, campfire_db::Timestamp::from_jiff(since), RECENT_MESSAGES], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let presenter = Presenter::new(conn, app, None);
    let mut rooms: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    let mut creators: std::collections::HashMap<i64, User> = std::collections::HashMap::new();
    let mut messages = Vec::with_capacity(ids.len());
    for id in ids {
        let message = Message::find(conn, id)?;
        if !rooms.contains_key(&message.room_id) {
            let room = Room::find(conn, message.room_id)?;
            rooms.insert(room.id, presenter.room_view(&room, user)?.display_name);
        }
        let creator = match creators.entry(message.creator_id) {
            std::collections::hash_map::Entry::Occupied(known) => known.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(message.creator(conn)?),
        };
        messages.push(ChatMessage {
            id: message.id,
            room_id: message.room_id,
            room_name: rooms[&message.room_id].clone(),
            url: campfire_routes::room_at_message(message.room_id, message.id),
            creator_id: creator.id,
            creator_name: creator.name.clone(),
            creator_is_bot: creator.is_bot(),
            created_at: message.created_at.jiff(),
            body_html: message.body_html(conn)?.unwrap_or_default(),
        });
    }
    Ok(messages)
}

// --- Fizzy over the shared HTTP client ------------------------------------------------------------

/// Requests over `integrations::net` (HTTP/1.1, rustls): 10 s to connect, 20 s per read, 30 s in
/// all. Errors never include the request's headers (the token).
struct FizzyHttp {
    net: Network,
}

const FIZZY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FIZZY_READ_TIMEOUT: Duration = Duration::from_secs(20);
const FIZZY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

impl fizzy::HttpClient for FizzyHttp {
    fn send<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'static str, String)],
        body: Option<&'a [u8]>,
    ) -> BoxFuture<'a, std::result::Result<HttpResponse, String>> {
        Box::pin(async move {
            tokio::time::timeout(FIZZY_REQUEST_TIMEOUT, self.fetch(method, url, headers, body))
                .await
                .unwrap_or_else(|_| Err("timed out".into()))
        })
    }
}

impl FizzyHttp {
    fn new() -> Self {
        Self { net: Network::system() }
    }

    async fn fetch(
        &self,
        method: &str,
        url: &str,
        headers: &[(&'static str, String)],
        body: Option<&[u8]>,
    ) -> std::result::Result<HttpResponse, String> {
        let method = hyper::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid method".to_string())?;
        let uri = uri::parse(url).map_err(|_| "invalid Fizzy URL".to_string())?;
        let host = uri.host.clone().filter(|host| !host.is_empty() && uri.is_http()).ok_or("invalid Fizzy URL")?;
        let https = uri.scheme.as_deref().is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"));
        let port = uri.port.and_then(|port| u16::try_from(port).ok()).ok_or("invalid Fizzy URL")?;
        let endpoint = Endpoint { https, host, port, pinned_ip: None };
        let headers = headers.iter().map(|(name, value)| (name.to_string(), value.clone())).collect();
        let mut request = http::Request::net_http(method, http::request_uri(&uri), None, headers).transport(true, &endpoint);
        if let Some(body) = body {
            request.body = body.to_vec();
        }
        let timeouts = Timeouts { open: FIZZY_CONNECT_TIMEOUT, read: FIZZY_READ_TIMEOUT };
        let response = http::exchange(&self.net, &endpoint, request, &timeouts).await.map_err(|error| error.to_string())?;
        let (status, link) = (response.status, response.header("link"));
        let total = response.header("x-total-count").and_then(|count| count.trim().parse().ok());
        match response.read_body(fizzy::MAX_BODY_BYTES).await.map_err(|error| error.to_string())? {
            Body::Complete(body) => Ok(HttpResponse { status, body, link, total }),
            Body::TooLarge => Err("reply too large".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use campfire_kit::Method;
    use campfire_workspace::fizzy::HttpClient;

    use super::*;
    use crate::controllers::presenters::test_support::*;
    use crate::integrations::test_support::{FakeServer, Route};

    /// On, with a Fizzy that never answers: everything that needs it says so.
    const ON: &[(&str, &str)] = &[("FIZZY_URL", "http://127.0.0.1:1"), ("FIZZY_TOKEN", "t0k3n")];

    fn json_post(path: &str, body: &Value) -> Req {
        Req::new(Method::POST, path)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(serde_json::to_vec(body).unwrap())
    }

    #[tokio::test]
    async fn phase_one_routes_are_404_while_off() {
        let Some(app) = TestApp::boot().await else { return };
        let mut david = app.david();
        let panel = format!("/workspace/rooms/{ALL_TALK}/panel");
        for path in ["/workspace/board", "/workspace/cards/12", "/workspace/cards/new", "/workspace/settings", panel.as_str()] {
            assert_eq!(david.get(path).await.status, StatusCode::NOT_FOUND, "{path}");
        }
        for path in ["/workspace/cards", "/workspace/cards/12/move", "/workspace/settings"] {
            assert_eq!(david.write(json_post(path, &json!({}))).await.status, StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(app.anonymous().get(&format!("/hermes/{BENDER_KEY}/workspace/settings.json")).await.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn phase_one_routes_before_fizzy_answers() {
        let Some(app) = TestApp::boot_with(ON).await else { return };
        let mut david = app.david();
        let board = david.get("/workspace/board").await;
        assert_eq!(board.status, StatusCode::OK);
        assert!(board.text().contains("data-ws-board"));

        let moved = david.write(json_post("/workspace/cards/12/move", &json!({"to": "closed"}))).await;
        assert_eq!((moved.status, moved.json()["error"].clone()), (StatusCode::BAD_GATEWAY, json!("fizzy_unavailable")));
        assert!(!moved.text().contains("t0k3n"));
        assert_eq!(david.write(json_post("/workspace/cards/12/fly", &json!({}))).await.status, StatusCode::NOT_FOUND);
        let invalid = david.write(json_post("/workspace/cards/12/move", &json!({"to": "sideways"}))).await;
        assert_eq!((invalid.status, invalid.json()["error"].clone()), (StatusCode::UNPROCESSABLE_ENTITY, json!("invalid_target")));
        let blank = david.write(json_post("/workspace/cards", &json!({"title": " "}))).await;
        assert_eq!((blank.status, blank.json()["error"].clone()), (StatusCode::UNPROCESSABLE_ENTITY, json!("blank_title")));
        let elsewhere = david.write(json_post("/workspace/cards", &json!({"title": "x", "message_id": 1}))).await;
        assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);

        assert_eq!(david.get("/workspace/cards/new?message_id=1").await.status, StatusCode::NOT_FOUND);
        let form = david.get(&format!("/workspace/cards/new?room_id={ALL_TALK}&fragment=1")).await;
        assert!(form.text().contains("data-ws-new-card-form"), "{}", form.text());
        assert_eq!(david.get(&format!("/workspace/rooms/{ALL_TALK}/panel")).await.status, StatusCode::NO_CONTENT, "no department linked");
        assert_eq!(david.get("/workspace/cards/12?fragment=1").await.status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn settings_are_saved_by_administrators_and_read_by_bots() {
        let Some(app) = TestApp::boot_with(ON).await else { return };
        let mut david = app.david();
        assert_eq!(david.get("/workspace/settings").await.status, StatusCode::OK);
        let body = json!({
            "departments": [{"name": "Engineering", "tag": "#Engineering", "rooms": [ALL_TALK, 1]}],
            "duty_managers": null,
            "confirm_policy": "anyone"
        });
        let saved = david.write(json_post("/workspace/settings", &body)).await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
        let settings = app.booted.app.workspace.as_ref().unwrap().settings();
        assert_eq!(
            (settings.departments[0].tag.as_str(), settings.departments[0].rooms.as_slice()),
            ("engineering", [ALL_TALK].as_slice())
        );
        let invalid = david.write(json_post("/workspace/settings", &json!({"departments": [{"name": "", "tag": "x"}]}))).await;
        assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(david.get(&format!("/workspace/rooms/{ALL_TALK}/panel")).await.status, StatusCode::OK);
        assert!(david.get(&format!("/rooms/{ALL_TALK}")).await.text().contains("data-ws-panel-root"));

        let bot = app.anonymous().get(&format!("/hermes/{BENDER_KEY}/workspace/settings.json")).await;
        assert_eq!(bot.status, StatusCode::OK, "{}", bot.text());
        assert_eq!(bot.json()["departments"][0]["tag"], "engineering");
        assert_eq!(david.get("/hermes/nope/workspace/settings.json").await.status, StatusCode::FORBIDDEN, "people aren't bots");

        // Nobody may create cards: no menu entry for the script, and the form is refused.
        let workspace = app.booted.app.workspace.clone().unwrap();
        let closed = Settings {
            duty_managers: Some(Vec::new()),
            confirm_policy: campfire_workspace::Policy::DutyManagersOnly,
            ..Settings::default()
        };
        workspace.settings_store().save(closed).unwrap();
        assert!(david.get(&format!("/rooms/{ALL_TALK}")).await.text().contains(r#"data-ws-can-create="false""#));
        let refused = david.get(&format!("/workspace/cards/new?room_id={ALL_TALK}&fragment=1")).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text());
        assert!(refused.text().contains("Only duty managers"));
        assert_eq!(david.get("/workspace/cards/new?message_id=1").await.status, StatusCode::NOT_FOUND, "404 before the policy");
    }

    #[test]
    fn routes_come_after_the_rails_table() {
        use crate::controllers::recognize;
        let found = |method: Method, path: &str| {
            recognize(&method, path).unwrap().map(|(route, params)| (route.endpoint, params.str("message_id").map(str::to_string)))
        };
        assert_eq!(found(Method::GET, "/workspace"), Some(("hermes/workspace#show", None)));
        assert_eq!(found(Method::GET, "/workspace/cards.json"), Some(("hermes/workspace#cards", None)));
        assert_eq!(found(Method::POST, "/workspace/drafts/42/reply"), Some(("hermes/workspace#reply", Some("42".into()))));
        assert_eq!(found(Method::GET, "/workspace/drafts/42/reply"), None);
        assert_eq!(found(Method::POST, "/workspace"), None);

        let endpoint = |method: Method, path: &str| recognize(&method, path).unwrap().map(|(route, _)| route.endpoint);
        let param = |method: Method, path: &str, name: &str| {
            recognize(&method, path).unwrap().and_then(|(_, params)| params.str(name).map(str::to_string))
        };
        assert_eq!(endpoint(Method::GET, "/workspace/board"), Some("hermes/workspace#board"));
        assert_eq!(endpoint(Method::GET, "/workspace/cards/new"), Some("hermes/workspace#new_card"));
        assert_eq!(endpoint(Method::POST, "/workspace/cards"), Some("hermes/workspace#create_card"));
        assert_eq!(endpoint(Method::GET, "/workspace/cards/12"), Some("hermes/workspace#card"));
        assert_eq!(param(Method::GET, "/workspace/cards/12", "number").as_deref(), Some("12"));
        assert_eq!(param(Method::POST, "/workspace/cards/12/severity", "change").as_deref(), Some("severity"));
        assert_eq!(endpoint(Method::GET, "/workspace/cards/12/severity"), None);
        assert_eq!(endpoint(Method::GET, "/workspace/rooms/3/panel"), Some("hermes/workspace#panel"));
        assert_eq!(endpoint(Method::GET, "/workspace/settings"), Some("hermes/workspace#settings"));
        assert_eq!(endpoint(Method::POST, "/workspace/settings"), Some("hermes/workspace#update_settings"));
        assert_eq!(endpoint(Method::GET, "/hermes/1-abc/workspace/settings.json"), Some("hermes/workspace#bot_settings"));
        assert_eq!(param(Method::GET, "/hermes/1-abc/workspace/settings.json", "bot_key").as_deref(), Some("1-abc"));
    }

    #[test]
    fn settings_ids_become_numbers() {
        let json = settings_json(json!({"departments": [{"name": "A", "tag": "a", "rooms": ["3", 4]}], "duty_managers": ["5"]}));
        assert_eq!(json, json!({"departments": [{"name": "A", "tag": "a", "rooms": [3, 4]}], "duty_managers": [5]}));
        assert_eq!(id_param(Some(&Param::Str(" 12 ".into()))), Some(12));
        assert_eq!(id_param(Some(&Param::Str("-1".into()))), None);
    }

    #[tokio::test]
    async fn fizzy_gets_go_over_the_shared_client() {
        let reply = Route::new("GET", "*", "/897/cards.json?board_ids%5B%5D=b1", 200)
            .header("Content-Type", "application/json")
            .header("Link", r#"<http://localhost:8484/897/cards.json?page=2>; rel="next""#)
            .header("X-Total-Count", "42")
            .body("[]");
        let server = FakeServer::start(vec![reply]).await;
        let http = FizzyHttp::new();
        let headers = [("Accept", "application/json".to_string()), ("Authorization", "Bearer t0k3n".to_string())];

        let response = http.get(&format!("http://{}/897/cards.json?board_ids%5B%5D=b1", server.addr), &headers).await.unwrap();
        assert_eq!((response.status, response.body.as_slice()), (200, b"[]".as_slice()));
        assert!(response.link.unwrap().contains(r#"rel="next""#));
        assert_eq!(response.total, Some(42));
        let received = &server.received()[0];
        assert_eq!(received.target, "/897/cards.json?board_ids%5B%5D=b1");
        assert_eq!(received.header("Authorization"), Some("Bearer t0k3n"));
        assert_eq!(received.header("Accept"), Some("application/json"));

        let missing = http.get(&format!("http://{}/897/cards/1.json", server.addr), &headers).await.unwrap();
        assert_eq!(missing.status, 404);
        let error = http.get("http://127.0.0.1:1/897/cards.json", &headers).await.unwrap_err();
        assert!(!error.contains("t0k3n"), "{error}");
        assert_eq!(http.get("fizzy/897", &headers).await.unwrap_err(), "invalid Fizzy URL");
    }

    #[tokio::test]
    async fn fizzy_writes_carry_their_method_and_body() {
        let reply = Route::new("POST", "*", "/897/cards/12/taggings.json", 204);
        let server = FakeServer::start(vec![reply]).await;
        let http = FizzyHttp::new();
        let headers = [("Authorization", "Bearer t0k3n".to_string()), ("Content-Type", "application/json".to_string())];
        let url = format!("http://{}/897/cards/12/taggings.json", server.addr);
        let response = http.send("POST", &url, &headers, Some(br#"{"tag_title":"sev-high"}"#)).await.unwrap();
        assert_eq!(response.status, 204);
        let received = &server.received()[0];
        assert_eq!((received.method.as_str(), received.body.as_slice()), ("POST", br#"{"tag_title":"sev-high"}"#.as_slice()));
        assert_eq!(received.header("Content-Type"), Some("application/json"));
        assert_eq!(http.send("NOT A METHOD", &url, &headers, None).await.unwrap_err(), "invalid method");
    }
}
