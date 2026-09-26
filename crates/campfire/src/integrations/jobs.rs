//! The jobs integrations perform for the app's job runner: `Room::PushMessageJob`
//! (reference/app/jobs/room/push_message_job.rb) and `Bot::WebhookJob`
//! (reference/app/jobs/bot/webhook_job.rb), including what `Webhook#deliver` does with a reply
//! (create the bot's message, process an attachment, `broadcast_create`).

use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, anyhow};
use campfire_db::{Event, Message, NewMessage, PushSubscription, Room, User, Webhook};
use campfire_richtext::Content;
use campfire_storage::Variation;
use campfire_views::messages as views;

use super::net::Network;
use super::web_push::{self, VapidConfig};
use super::webhook::{self, WebhookReply};
use crate::app::App;
use crate::controllers::presenters::page::{self, Rendered};
use crate::controllers::presenters::{DbResolver, Presenter, storage_error};
use crate::jobs::{JobKind, Registry};

/// Registers the handlers for `Event::PushMessage` and `Event::DeliverWebhook`.
pub fn register_jobs(registry: &mut Registry) {
    // `config.x.web_push_pool`: one pool for the process, made on first use (inside the runtime).
    let pool: Arc<OnceLock<web_push::Pool>> = Arc::new(OnceLock::new());
    registry.handle(JobKind::PushMessage, move |app: App, event: Event| {
        let pool = pool.clone();
        async move { push_message(app, &pool, event).await }
    });
    registry.handle(JobKind::DeliverWebhook, deliver_webhook);
}

/// `Room::PushMessageJob#perform(room, message)`: `Room::MessagePusher.new(room:, message:).push`
async fn push_message(app: App, pool: &OnceLock<web_push::Pool>, event: Event) -> anyhow::Result<()> {
    let Event::PushMessage { message_id, .. } = event else { return Ok(()) };
    let pool = pool.get_or_init(|| web_push_pool(&app)).clone();
    let db = app.db.clone();
    app.db
        .read(move |conn| {
            let message = Message::find(conn, message_id)?;
            web_push::push_message(&pool, conn, &*db.env().rich_text, &message, db.env().now()).map(|_| ())
        })
        .await?;
    Ok(())
}

/// config/initializers/web_push.rb: the pool, whose invalid subscription handler destroys the
/// subscription (`Push::Subscription.find_by(id:)&.destroy`).
fn web_push_pool(app: &App) -> web_push::Pool {
    let vapid = VapidConfig::new(
        app.config.vapid_public_key.clone().unwrap_or_default(),
        app.config.vapid_private_key.clone().unwrap_or_default(),
    );
    let db = app.db.clone();
    web_push::Pool::new(Network::system(), vapid, move |id| {
        db.write_blocking(move |tx| match PushSubscription::find(tx.conn(), id) {
            Ok(subscription) => subscription.destroy(tx),
            Err(campfire_db::Error::RecordNotFound(_)) => Ok(()),
            Err(error) => Err(error),
        })
    })
}

/// `Bot::WebhookJob#perform(bot, message)`: `bot.deliver_webhook(message)`, i.e.
/// `webhook.deliver(message)`, then the reply.
async fn deliver_webhook(app: App, event: Event) -> anyhow::Result<()> {
    let Event::DeliverWebhook { bot_id, message_id } = event else { return Ok(()) };
    let db = app.db.clone();
    let (bot, room, url, payload) = app
        .db
        .read(move |conn| {
            let bot = User::find(conn, bot_id)?;
            let message = Message::find(conn, message_id)?;
            let room = Room::find(conn, message.room_id)?;
            let Some(webhook) = Webhook::find_by_user(conn, bot_id)? else { return Ok(None) };
            let payload = webhook.payload(
                conn,
                &*db.env().rich_text,
                &message,
                &campfire_routes::room_bot_messages(room.id, bot.bot_key()),
                &campfire_routes::room_at_message(room.id, message.id),
            )?;
            Ok(Some((bot, room, webhook.url, payload)))
        })
        .await?
        .context("undefined method 'deliver' for nil (the bot has no webhook)")?;

    let delivery = webhook::deliver(&Network::system(), url.as_deref().unwrap_or(""), payload).await?;
    let message = match delivery.reply {
        WebhookReply::None => return Ok(()),
        WebhookReply::Text(text) => create_text_reply(&app, &room, &bot, text).await?,
        WebhookReply::Attachment(attachment) => create_attachment_reply(&app, &room, &bot, attachment).await?,
    };
    broadcast_create(&app, &room, &message).await
}

/// `room.messages.create!(body: text, creator: user)`: the text is assigned to the rich text
/// body, which stores it canonicalized (as `MessagesController` does; there's no request host
/// in a job).
async fn create_text_reply(app: &App, room: &Room, bot: &User, text: String) -> anyhow::Result<Message> {
    let (app2, room_id, creator_id) = (app.clone(), room.id, bot.id);
    let message = app
        .db
        .write(move |tx| {
            let resolver = DbResolver { conn: tx.conn(), secrets: &app2.secrets, now: app2.clock.now() };
            let ctx = resolver.render_context(None);
            let body = Content::load(&text, &ctx).map(|content| content.to_html()).unwrap_or(text);
            Message::create(tx, NewMessage { room_id, creator_id, client_message_id: None, body: Some(body), attachment_blob_id: None })
        })
        .await?;
    Ok(message)
}

/// `ActiveStorage::Blob.create_and_upload!` (its own save), then
/// `room.messages.create_with_attachment!(attachment:, creator: user)`, which processes the
/// attachment.
async fn create_attachment_reply(app: &App, room: &Room, bot: &User, attachment: webhook::Attachment) -> anyhow::Result<Message> {
    let storage = app.storage.clone();
    let now = app.clock.now();
    let blob = app.db.write(move |tx| attachment.create_blob(&storage, tx.conn(), now).map_err(storage_error)).await?;

    let (room_id, creator_id, blob_id) = (room.id, bot.id, blob.id);
    let message = app
        .db
        .write(move |tx| Message::create(tx, NewMessage { room_id, creator_id, client_message_id: None, body: None, attachment_blob_id: Some(blob_id) }))
        .await?;
    process_attachment(app, blob).await?;
    let id = message.id;
    Ok(app.db.read(move |conn| Message::find(conn, id)).await?)
}

/// `Message#process_attachment`: analyze the blob (its `after_update` touches the message and
/// its room), then generate the video preview or the `:thumb` representation.
async fn process_attachment(app: &App, blob: campfire_storage::Blob) -> anyhow::Result<()> {
    let storage = app.storage.clone();
    let blob = app
        .db
        .write(move |tx| {
            let mut blob = blob;
            storage.analyze(tx.conn(), &mut blob).map_err(storage_error)?;
            for (record_type, record_id) in campfire_storage::blob::attachment_records(tx.conn(), blob.id).map_err(storage_error)? {
                if record_type == "Message" {
                    Message::find(tx.conn(), record_id)?.touch(tx)?;
                }
            }
            Ok(blob)
        })
        .await?;

    let storage = app.storage.clone();
    let now = app.clock.now();
    if blob.is_video() {
        app.db
            .write(move |tx| storage.process_preview(tx.conn(), &blob, &Variation::format_only("webp"), now).map_err(storage_error))
            .await?;
    } else if blob.is_representable() {
        let thumb = Variation::resize_to_limit(1200, 800, None);
        crate::active_storage::processed_representation(app, blob, thumb).await.map_err(|e| anyhow!("{e:?}"))?;
    }
    Ok(())
}

/// `message.broadcast_create`, rendered without a request (`ApplicationController.renderer`).
async fn broadcast_create(app: &App, room: &Room, message: &Message) -> anyhow::Result<()> {
    let (app, room, message) = (app.clone(), room.clone(), message.clone());
    let db = app.db.clone();
    db.read(move |conn| {
        let presenter = Presenter::new(conn, &app.secrets, &app.storage, &*app.db.env().rich_text, app.clock.now(), None);
        let view = presenter.message(&message)?;
        let account = campfire_db::Account::first(conn)?;
        let html = page::render_detached(&app, account.as_ref(), |ctx| views::message(ctx, &view));
        let partials = Rendered { message: Some(html), ..Rendered::default() };
        app.broadcasts.message_create(conn, &room, &message, &partials)
    })
    .await?;
    Ok(())
}
