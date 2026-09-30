//! Hermes fork: the Duty Manager Workspace's adapter (docs/hermes-workspace.md). Not in the
//! reference. The feature lives in `campfire_workspace`; this file is the only place that knows
//! both sides:
//!
//! - [`start`]: at boot, the bots, the render hooks (`campfire_views::hermes::WorkspaceHooks`) and
//!   the Fizzy poll task, over the shared `integrations::net` client ([`FizzyHttp`]);
//! - `GET /workspace`: the Home page, in the application layout;
//! - `GET /workspace/cards.json?numbers=12,13`: current chips, for the page to refresh them;
//! - `POST /workspace/drafts/:message_id/reply` (`{"decision": "confirm" | "dismiss"}`): answers a
//!   Hermes draft in its room as the current user, through `MessagesController#create`'s path.
//!
//! All three answer 404 while the workspace is off (no `FIZZY_URL`/`FIZZY_TOKEN`), and run
//! `ApplicationController`'s chain (session only, no bots, `Sec-Fetch-Site` forgery protection on
//! the POST).

use std::sync::{Arc, Weak};
use std::time::Duration;

use askama::Template;
use campfire_db::{Message, Room, User};
use campfire_kit::{Ctx, Error, Param, Result, StatusCode, format};
use campfire_richtext::uri;
use campfire_views::helpers as h;
use campfire_views::layouts::Application;
use campfire_views::{ViewContext, hermes::WorkspaceHooks};
use campfire_workspace::drafts::{self, Decision};
use campfire_workspace::fizzy::{self, HttpResponse};
use campfire_workspace::overlay::{self, TabBar};
use campfire_workspace::{Bot, BoxFuture, ChatMessage, ChatSource, Viewer, Workspace};
use serde_json::json;

use crate::app::{App, AppCtx, AppState};
use crate::concerns::{Before, before_actions, require_current_user};
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

/// The workspace from the config, `None` while it's off.
pub fn build(app_config: &crate::config::Config) -> Option<Arc<Workspace>> {
    app_config.workspace.clone().map(|config| Arc::new(Workspace::new(config)))
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
        "Duty Manager Workspace on"
    );
    tokio::spawn(poll_loop(Arc::downgrade(app), workspace));
}

/// Polls every `FIZZY_POLL_S` for as long as the app lives. Logs when Fizzy goes up or down, not
/// on every failed poll.
async fn poll_loop(app: Weak<AppState>, workspace: Arc<Workspace>) {
    let http = FizzyHttp { net: Network::system() };
    let mut interval = tokio::time::interval(workspace.config().poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_ok: Option<bool> = None;
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
        let path = ctx.request_url.strip_prefix(&ctx.base_url).unwrap_or("/");
        let path = path.split(['?', '#']).next().unwrap_or("/");
        let (active, room_id) = overlay::locate(path);
        let report_room = room_id.or(ctx.last_room_visited_id);
        let bar = TabBar {
            active,
            // The live voice page has its own bottom bar.
            show_bar: !path.ends_with("/voice"),
            home_url: overlay::HOME_PATH.into(),
            chats_url: ctx.last_room_visited_id.map(campfire_routes::room).unwrap_or_else(|| "/".into()),
            report_url: report_room.filter(|_| self.voice).map(overlay::voice_path),
            cards_url: overlay::CARDS_PATH.into(),
            stylesheet_url: ctx.asset("hermes/workspace.css"),
            script_url: ctx.asset("hermes/workspace.js"),
            home_icon: ctx.asset("hermes/home.svg"),
            chats_icon: ctx.asset("messages-outlined.svg"),
            report_icon: ctx.asset("headset.svg"),
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
    let viewer = Viewer { id: user.id, name: user.name.clone(), email: user.email_address.clone() };
    let source = RoomMessages { app: c.app().clone(), user };
    let home = workspace.home(&viewer, &source, c.now()).await.map_err(|error| Error::internal(anyhow::anyhow!(error)))?;
    let content = home.render().map_err(Error::internal)?;
    page_in_any_format(c, StatusCode::OK, |ctx| {
        let nav = overlay::HomeNav {
            chats_url: ctx.last_room_visited_id.map(campfire_routes::room).unwrap_or_else(|| "/".into()),
            back_icon: ctx.asset("arrow-left.svg"),
        };
        Application {
            ctx,
            page_title: Some("Home".into()),
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
    let (room, bot, body) = c
        .app()
        .db
        .read(move |conn| {
            // Current.user.reachable_messages.find: 404 outside the user's rooms.
            let message = Message::find_reachable(conn, user_id, id)?;
            Ok((message.room(conn)?, message.creator(conn)?, message.body_html(conn)?.unwrap_or_default()))
        })
        .await
        .map_err(db_error)?;
    if !(bot.is_bot() && bot.is_active()) || drafts::detect(&body).is_none() {
        return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "not_a_draft", "This message isn't a draft awaiting confirmation.");
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

/// GETs over `integrations::net` (HTTP/1.1, rustls): 10 s to connect, 20 s per read, 30 s in all.
/// Errors never include the request's headers (the token).
struct FizzyHttp {
    net: Network,
}

const FIZZY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FIZZY_READ_TIMEOUT: Duration = Duration::from_secs(20);
const FIZZY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

impl fizzy::HttpClient for FizzyHttp {
    fn get<'a>(&'a self, url: &'a str, headers: &'a [(&'static str, String)]) -> BoxFuture<'a, std::result::Result<HttpResponse, String>> {
        Box::pin(async move {
            tokio::time::timeout(FIZZY_REQUEST_TIMEOUT, self.fetch(url, headers)).await.unwrap_or_else(|_| Err("timed out".into()))
        })
    }
}

impl FizzyHttp {
    async fn fetch(&self, url: &str, headers: &[(&'static str, String)]) -> std::result::Result<HttpResponse, String> {
        let uri = uri::parse(url).map_err(|_| "invalid Fizzy URL".to_string())?;
        let host = uri.host.clone().filter(|host| !host.is_empty() && uri.is_http()).ok_or("invalid Fizzy URL")?;
        let https = uri.scheme.as_deref().is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"));
        let port = uri.port.and_then(|port| u16::try_from(port).ok()).ok_or("invalid Fizzy URL")?;
        let endpoint = Endpoint { https, host, port, pinned_ip: None };
        let headers = headers.iter().map(|(name, value)| (name.to_string(), value.clone())).collect();
        let request = http::Request::net_http(hyper::Method::GET, http::request_uri(&uri), None, headers).transport(true, &endpoint);
        let timeouts = Timeouts { open: FIZZY_CONNECT_TIMEOUT, read: FIZZY_READ_TIMEOUT };
        let response = http::exchange(&self.net, &endpoint, request, &timeouts).await.map_err(|error| error.to_string())?;
        let (status, link) = (response.status, response.header("link"));
        match response.read_body(fizzy::MAX_BODY_BYTES).await.map_err(|error| error.to_string())? {
            Body::Complete(body) => Ok(HttpResponse { status, body, link }),
            Body::TooLarge => Err("reply too large".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use campfire_kit::Method;
    use campfire_workspace::fizzy::HttpClient;

    use super::*;
    use crate::integrations::test_support::{FakeServer, Route};

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
    }

    #[tokio::test]
    async fn fizzy_gets_go_over_the_shared_client() {
        let reply = Route::new("GET", "*", "/897/cards.json?board_ids%5B%5D=b1", 200)
            .header("Content-Type", "application/json")
            .header("Link", r#"<http://localhost:8484/897/cards.json?page=2>; rel="next""#)
            .body("[]");
        let server = FakeServer::start(vec![reply]).await;
        let http = FizzyHttp { net: Network::system() };
        let headers = [("Accept", "application/json".to_string()), ("Authorization", "Bearer t0k3n".to_string())];

        let response = http.get(&format!("http://{}/897/cards.json?board_ids%5B%5D=b1", server.addr), &headers).await.unwrap();
        assert_eq!((response.status, response.body.as_slice()), (200, b"[]".as_slice()));
        assert!(response.link.unwrap().contains(r#"rel="next""#));
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
}
