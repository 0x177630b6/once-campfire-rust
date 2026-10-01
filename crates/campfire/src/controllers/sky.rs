//! Hermes fork: Sky push-to-talk, batch 1a (docs/hermes-gemini-live.md, "Sky push-to-talk"; plan:
//! Hermes-self `docs/ui-redesign/10-push-to-talk-plan.md`). Not in the reference.
//!
//! The floating button (`campfire_workspace::overlay::SkyButton`, rendered with the tab bar) holds
//! a Gemini Live session from the browser; these routes give it what it can't have by itself:
//!
//! - `POST /sky/token` (`{"reconnect_of"?}`): a single-use Gemini Live token with Sky's setup locked
//!   in it (`campfire_workspace::sky::SkySetup`: talk only, manual activity detection), 10 minutes,
//!   within Sky's own limits and the month budget. Answers `{token, ws_url, model, expires_at,
//!   warm_seconds, token_id}`; `token_id` is the receipt a cost report must name.
//! - `POST /sky/context` (`{screen, room_id?, card?, last?}`): one press. Counts it (the daily
//!   cap), checks the page's hint (a room the person is a member of, a card they may see) and
//!   answers the screen note Sky gets before the person's words, the chip that shows it, and a
//!   `press_id` for later calls of the same press (batch 1b). `last` carries the previous press's
//!   timings (numbers only), logged for the spike's measurements.
//! - `POST /sky/usage` (`{token_id, held_ms, reply_ms, turns}`): the page's report for one token's
//!   session, counted once into the estimated cost (`Sky::report_usage`).
//!
//! All three answer 404 unless the workspace and the live voice are on (`FIZZY_URL`, `FIZZY_TOKEN`,
//! `GEMINI_API_KEY`), `SKY_PTT` is not `off` and lets this person use it; they run
//! `ApplicationController`'s chain (session only, no bots, `Sec-Fetch-Site` forgery protection).
//! Logs (`sky:`) carry ids, sizes and timings, never words or tokens (decision O3).

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use campfire_db::{Room, User};
use campfire_kit::{Ctx, Error, Param, ParamMap, Result, StatusCode};
use campfire_workspace::Workspace;
use campfire_workspace::sky::{ContextNote, Mode, Press, Refusal, ReportError, Screen, SessionUsage, SkySetup, TokenKind};
use serde_json::json;

use crate::app::{App, AppCtx};
use crate::concerns::{Before, before_actions, require_current_user};
use crate::controllers::presenters::Presenter;
use crate::controllers::presenters::page::db_error;
use crate::controllers::workspace::{refresh_rooms, viewer};
use crate::integrations::gemini_live::{self, TokenLifetime};

pub const TOKEN_PATH: &str = "/sky/token";
pub const CONTEXT_PATH: &str = "/sky/context";
pub const USAGE_PATH: &str = "/sky/usage";

/// How often the usage counters are written (`sky-usage.json`), by the one save task.
const SAVE_EVERY: Duration = Duration::from_secs(5);

/// The press timings the page may report with the next press (`last`), all numbers but `outcome`.
const TIMINGS: [&str; 7] =
    ["held_ms", "context_ms", "connect_ms", "release_to_first_text_ms", "release_to_first_audio_ms", "reply_ms", "reply_chars"];
/// `last.outcome`'s values.
const OUTCOMES: [&str; 6] = ["answered", "no_reply", "cancelled", "tip", "error", "interrupted"];

// --- Boot -----------------------------------------------------------------------------------------

/// At boot, while Sky is on: one task saves the usage counters every few seconds, off the async
/// workers (the file's single saver).
pub fn start(app: &App, workspace: &Arc<Workspace>) {
    let config = &workspace.config().sky;
    if config.mode == Mode::Off {
        return;
    }
    if app.gemini_live.is_none() {
        tracing::warn!("SKY_PTT is set but GEMINI_API_KEY isn't: Sky push-to-talk stays off");
        return;
    }
    tracing::info!(
        mode = ?config.mode,
        pilot_users = config.users.len(),
        tokens_per_hour = config.tokens_per_hour,
        presses_per_day = config.presses_per_day,
        monthly_budget_usd = config.monthly_budget_usd,
        warm_seconds = config.warm_seconds,
        "sky: push-to-talk on"
    );
    tokio::spawn(save_loop(Arc::downgrade(workspace)));
}

async fn save_loop(workspace: Weak<Workspace>) {
    let mut interval = tokio::time::interval(SAVE_EVERY);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some(workspace) = workspace.upgrade() else { return };
        match tokio::task::spawn_blocking(move || workspace.sky().save()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "sky: could not save sky-usage.json (retried)"),
            Err(error) => tracing::warn!(%error, "sky: the usage save task failed"),
        }
    }
}

// --- Actions --------------------------------------------------------------------------------------

pub async fn token(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = allowed_user(c, &workspace)?;
    let (now, zone) = (c.now(), workspace.settings().handover.zone());
    let started = Instant::now();
    let reconnect_of = c.request_params.get("reconnect_of").and_then(Param::as_str).map(str::to_string);
    let grant = match workspace.sky().grant_token(user.id, reconnect_of.as_deref(), now, &zone) {
        Ok(grant) => grant,
        Err(refusal) => {
            tracing::info!(user_id = user.id, route = "token", outcome = ?refusal, "sky: refused");
            return refused(c, refusal);
        }
    };

    let languages = gemini_live::preferred_languages(c.request.header("accept-language"));
    let live = c.app().gemini_live.as_ref().ok_or(Error::NotFound)?;
    let model = live.config.model.clone();
    let setup = SkySetup { model: &model, user_name: &user.name, languages: &languages }.setup();
    let minted = live.mint_locked(setup, TokenLifetime::SKY, now).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let kind = if grant.kind == TokenKind::Reconnect { "reconnect" } else { "new" };
    match minted {
        Ok(token) => {
            workspace.sky().minted(&grant, now, &zone);
            tracing::info!(user_id = user.id, route = "token", kind, token_id = %grant.id, elapsed_ms, "sky: token minted");
            c.json(
                StatusCode::OK,
                &json!({
                    "token": token,
                    "ws_url": gemini_live::WS_URL,
                    "model": model,
                    "expires_at": gemini_live::rfc3339(now + TokenLifetime::SKY.expire),
                    "warm_seconds": workspace.config().sky.warm_seconds,
                    "token_id": grant.id,
                }),
            )
        }
        Err(error) => {
            workspace.sky().mint_failed(&grant, now, &zone);
            tracing::warn!(user_id = user.id, route = "token", kind, elapsed_ms, %error, "sky: could not mint a Gemini Live token");
            json_error(c, StatusCode::BAD_GATEWAY, "upstream_error", "Sky’s voice is unavailable; try again in a moment.")
        }
    }
}

pub async fn context(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = allowed_user(c, &workspace)?;
    let (now, zone) = (c.now(), workspace.settings().handover.zone());
    let started = Instant::now();
    log_last_press(user.id, c.request_params.get("last").and_then(Param::as_hash));
    if let Err(refusal) = workspace.sky().allow_press(user.id, now, &zone) {
        tracing::info!(user_id = user.id, route = "context", outcome = ?refusal, "sky: refused");
        return refused(c, refusal);
    }

    let screen = Screen::parse(c.request_params.get("screen").and_then(Param::as_str).unwrap_or(""));
    // The page's hint, checked: a room the person is a member of, a card they may see.
    let room = match positive_id(c.request_params.get("room_id")) {
        Some(room_id) => member_room_name(c, &user, room_id).await?,
        None => None,
    };
    let card = match positive_id(c.request_params.get("card")).and_then(|number| u64::try_from(number).ok()) {
        Some(number) => {
            refresh_rooms(c, &workspace, &user).await?;
            workspace.sky_card(&viewer(&user), number)
        }
        None => None,
    };
    let note = ContextNote::build(screen, room.as_ref().map(|(_, name)| name.as_str()), card.as_ref(), now, &zone);
    let press = Press {
        user_id: user.id,
        at: now,
        screen,
        room_id: room.as_ref().map(|(id, _)| *id),
        card: card.as_ref().map(|card| card.number),
        restricted: note.restricted,
    };
    let press_id = workspace.sky().remember_press(press);
    tracing::info!(
        user_id = user.id,
        route = "context",
        screen = screen.as_str(),
        room = room.is_some(),
        card = card.is_some(),
        restricted = note.restricted,
        note_chars = note.note.chars().count(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "sky: press"
    );
    c.json(StatusCode::OK, &json!({ "press_id": press_id, "note": note.note, "chip": note.chip, "restricted": note.restricted }))
}

pub async fn usage(c: &mut Ctx) -> Result {
    let workspace = feature(c)?;
    before_actions(c, Before::default()).await?;
    let user = allowed_user(c, &workspace)?;
    let (now, zone) = (c.now(), workspace.settings().handover.zone());
    let params = &c.request_params;
    let Some(token_id) = params.get("token_id").and_then(Param::as_str).map(str::to_string) else {
        return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "invalid_usage", "A token_id is required.");
    };
    let usage = SessionUsage {
        held_ms: number(params.get("held_ms")).unwrap_or(0),
        reply_ms: number(params.get("reply_ms")).unwrap_or(0),
        turns: number(params.get("turns")).map_or(0, |turns| u32::try_from(turns).unwrap_or(u32::MAX)),
    };
    match workspace.sky().report_usage(user.id, &token_id, usage, now, &zone) {
        Ok(added) => {
            tracing::info!(
                user_id = user.id,
                route = "usage",
                token_id = %token_id,
                held_ms = usage.held_ms,
                reply_ms = usage.reply_ms,
                turns = usage.turns,
                added_micro_usd = added,
                "sky: session usage"
            );
            c.json(StatusCode::OK, &json!({ "counted": true }))
        }
        Err(ReportError::Unknown) => json_error(c, StatusCode::NOT_FOUND, "unknown_token", "No such Sky session."),
        Err(ReportError::AlreadyReported) => json_error(c, StatusCode::CONFLICT, "already_reported", "This session was already counted."),
    }
}

// --- Helpers --------------------------------------------------------------------------------------

/// The workspace, while Sky can be on for someone: the workspace and the live voice on, `SKY_PTT`
/// not `off`. 404 otherwise.
fn feature(c: &Ctx) -> Result<Arc<Workspace>> {
    let workspace = c.app().workspace.clone().ok_or(Error::NotFound)?;
    if c.app().gemini_live.is_none() || workspace.config().sky.mode == Mode::Off {
        return Err(Error::NotFound);
    }
    Ok(workspace)
}

/// The signed-in person, when `SKY_PTT` lets them use Sky's voice; 404 otherwise (as while off).
fn allowed_user(c: &Ctx, workspace: &Workspace) -> Result<User> {
    let user = require_current_user(c)?.clone();
    if user.is_bot() || !workspace.config().sky.allows(user.id, user.is_administrator()) {
        return Err(Error::NotFound);
    }
    Ok(user)
}

fn refused(c: &mut Ctx, refusal: Refusal) -> Result {
    match refusal {
        Refusal::RateLimited => json_error(
            c,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Sky’s voice was used a lot in the last hour. Try again in a few minutes, or use the Sky tab.",
        ),
        Refusal::DailyCap => json_error(
            c,
            StatusCode::TOO_MANY_REQUESTS,
            "daily_cap",
            "You’ve reached today’s limit for Sky’s voice. Use the Sky tab until tomorrow.",
        ),
        Refusal::BudgetPaused => json_error(
            c,
            StatusCode::PAYMENT_REQUIRED,
            "budget_paused",
            "Sky’s voice is paused until the 1st of next month. Use the Sky tab.",
        ),
    }
}

fn json_error(c: &mut Ctx, status: StatusCode, code: &str, message: &str) -> Result {
    c.json(status, &json!({ "error": code, "message": message }))
}

/// `(id, display name)` of room `room_id` when `user` is a member, else `None`.
async fn member_room_name(c: &Ctx, user: &User, room_id: i64) -> Result<Option<(i64, String)>> {
    let (app, user) = (c.app().clone(), user.clone());
    c.app()
        .db
        .read(move |conn| {
            let Some(room) = Room::find_for_user(conn, user.id, room_id)? else { return Ok(None) };
            let name = Presenter::new(conn, &app, None).room_view(&room, &user)?.display_name;
            Ok(Some((room.id, name)))
        })
        .await
        .map_err(db_error)
}

/// A whole number from a JSON number or a string.
fn number(param: Option<&Param>) -> Option<u64> {
    match param? {
        Param::Number(number) => number.as_u64().or_else(|| number.as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64)),
        param => param.as_str().and_then(|value| value.trim().parse().ok()),
    }
}

fn positive_id(param: Option<&Param>) -> Option<i64> {
    number(param).and_then(|id| i64::try_from(id).ok()).filter(|id| *id > 0)
}

/// The previous press's timings, for the spike's measurements (plan §3.2, E1/E4): known keys
/// only, numbers only (and a fixed outcome word), never what was said.
fn log_last_press(user_id: i64, last: Option<&ParamMap>) {
    let Some(last) = last else { return };
    let value = |key: &str| number(last.get(key)).map(|n| n.min(3_600_000) as i64).unwrap_or(-1);
    let [held, context, connect, first_text, first_audio, reply, reply_chars] = TIMINGS.map(value);
    let outcome = last.get("outcome").and_then(Param::as_str).filter(|outcome| OUTCOMES.contains(outcome)).unwrap_or("unknown");
    let warm = matches!(last.get("warm"), Some(Param::Bool(true)));
    tracing::info!(
        user_id,
        route = "timings",
        outcome,
        warm,
        held_ms = held,
        context_ms = context,
        connect_ms = connect,
        release_to_first_text_ms = first_text,
        release_to_first_audio_ms = first_audio,
        reply_ms = reply,
        reply_chars,
        "sky: last press"
    );
}

#[cfg(test)]
mod tests;
