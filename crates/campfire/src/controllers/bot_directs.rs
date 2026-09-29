//! Hermes fork: a bot's direct message to a user named by email address. Not in the reference.
//!
//! `POST /hermes/:bot_key/directs?email_address=<address>`, the raw body being the message HTML
//! (as `Messages::ByBotsController#create`). Finds or creates the direct room between the bot and
//! that active, non-bot user, then posts the message there as the bot, through the same path as
//! the bot API (so the user gets the unread ping and Web Push). Answers 201
//! `{"room_id", "message_id"}`; 404 for an unknown, deactivated or bot address; 403 when the
//! caller is not a bot. Used by the Hermes bridge to relay Fizzy @mentions to Campfire.

use campfire_db::{Room, User};
use campfire_kit::{Ctx, Param, Result, StatusCode, halt};
use serde_json::json;

use crate::app::AppCtx;
use crate::concerns::{self, Before, before_actions, require_current_user};
use crate::controllers::messages::by_bots::{is_blank, raw_request_body};
use crate::controllers::messages::{self, MessageParams};
use crate::controllers::presenters::page::db_error;
use crate::controllers::rooms::directs::broadcast_create_room;

pub async fn create(c: &mut Ctx) -> Result {
    before_actions(c, Before::default().allow_bot_access()).await?;
    let bot = require_current_user(c)?.clone();
    if !bot.is_bot() {
        return halt(concerns::head(StatusCode::FORBIDDEN));
    }
    let body = raw_request_body(c);
    if is_blank(&body) {
        return halt(concerns::head(StatusCode::UNPROCESSABLE_ENTITY));
    }
    let email = c.params.get("email_address").and_then(Param::as_str).map(|e| e.trim().to_lowercase()).unwrap_or_default();
    if email.is_empty() {
        return halt(concerns::head(StatusCode::UNPROCESSABLE_ENTITY));
    }

    let bot_id = bot.id;
    let found = c
        .app()
        .db
        .write(move |tx| {
            let user = User::find_by_email_address(tx.conn(), &email)?.filter(|u| u.is_active() && !u.is_bot());
            let Some(user) = user else { return Ok(None) };
            let existing = Room::find_direct_for(tx.conn(), &[bot_id, user.id])?;
            let created = existing.is_none();
            let room = match existing {
                Some(room) => room,
                None => Room::find_or_create_direct_for(tx, &[bot_id, user.id], bot_id)?,
            };
            Ok(Some((room, created)))
        })
        .await
        .map_err(db_error)?;
    let Some((room, created)) = found else {
        return halt(concerns::head(StatusCode::NOT_FOUND));
    };
    if created {
        broadcast_create_room(c, &room).await?;
    }

    let message = messages::create_message(c, &room, MessageParams { body: Some(body), ..MessageParams::default() }).await?;
    messages::broadcast_create(c, &room, &message).await?;
    messages::deliver_webhooks_to_bots(c, &room, &message).await?;
    c.json(StatusCode::CREATED, &json!({ "room_id": room.id, "message_id": message.id }))
}
