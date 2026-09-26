//! `reference/app/models/webhook.rb`: the row and the payload. Delivery (HTTP, replies)
//! lives in the app.

use rusqlite::{Connection, Row, params};

use crate::database::Tx;
use crate::error::Result;
use crate::models::{Message, Room, User};
use crate::rich_text::RichText;
use crate::sql::query_one;
use crate::time::Timestamp;

/// `Webhook::ENDPOINT_TIMEOUT`
pub const ENDPOINT_TIMEOUT_SECONDS: u64 = 7;

#[derive(Debug, Clone, PartialEq)]
pub struct Webhook {
    pub id: i64,
    pub user_id: i64,
    pub url: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Webhook {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            url: row.get("url")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    pub fn find_by_user(conn: &Connection, user_id: i64) -> Result<Option<Self>> {
        query_one(
            conn,
            r#"SELECT "webhooks".* FROM "webhooks" WHERE "webhooks"."user_id" = ? LIMIT 1"#,
            [user_id],
            Self::from_row,
        )
    }

    /// `create_webhook!(url:)`
    pub fn create(tx: &Tx<'_>, user_id: i64, url: Option<&str>) -> Result<Self> {
        let now = tx.now();
        let id: i64 = tx.conn().query_row(
            r#"INSERT INTO "webhooks" ("created_at", "updated_at", "url", "user_id") VALUES (?, ?, ?, ?) RETURNING "id""#,
            params![now, now, url, user_id],
            |r| r.get(0),
        )?;
        Ok(Self {
            id,
            user_id,
            url: url.map(Into::into),
            created_at: now,
            updated_at: now,
        })
    }

    /// `webhook.update!(url:)`
    pub fn update_url(&mut self, tx: &Tx<'_>, url: &str) -> Result<()> {
        if self.url.as_deref() == Some(url) {
            return Ok(());
        }
        let now = tx.now();
        tx.conn().execute(
            r#"UPDATE "webhooks" SET "updated_at" = ?, "url" = ? WHERE "webhooks"."id" = ?"#,
            params![now, url, self.id],
        )?;
        self.url = Some(url.into());
        self.updated_at = now;
        Ok(())
    }

    pub fn destroy(&self, tx: &Tx<'_>) -> Result<()> {
        tx.conn().execute(
            r#"DELETE FROM "webhooks" WHERE "webhooks"."id" = ?"#,
            [self.id],
        )?;
        Ok(())
    }

    /// The JSON body `deliver` posts. `room_bot_messages_path` and `message_path` come from
    /// the route helpers (`room_bot_messages_path(room, bot_key)`, `room_at_message_path(room, message)`).
    pub fn payload(
        &self,
        conn: &Connection,
        rich_text: &dyn RichText,
        message: &Message,
        room_bot_messages_path: &str,
        message_path: &str,
    ) -> Result<String> {
        let creator = message.creator(conn)?;
        let room = Room::find(conn, message.room_id)?;
        let recipient = User::find(conn, self.user_id)?;
        let html = message.body_html(conn)?;
        let plain =
            without_recipient_mentions(&message.plain_text_body(conn, rich_text)?, &recipient);

        // Hash order as written in `Webhook#payload`, encoded like `ActiveSupport::JSON`.
        let body = format!(
            r#"{{"user":{{"id":{},"name":{}}},"room":{{"id":{},"name":{},"path":{}}},"message":{{"id":{},"body":{{"html":{},"plain":{}}},"path":{}}}}}"#,
            creator.id,
            json_string(&creator.name),
            room.id,
            room.name
                .as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".into()),
            json_string(room_bot_messages_path),
            message.id,
            html.as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".into()),
            json_string(&plain),
            json_string(message_path),
        );
        Ok(body)
    }
}

/// Removes `@Recipient` mentions and leading/trailing (Unicode) whitespace.
fn without_recipient_mentions(body: &str, recipient: &User) -> String {
    body.replace(&recipient.attachable_plain_text_representation(), "")
        .trim_matches(char::is_whitespace)
        .to_string()
}

/// `ActiveSupport::JSON.encode` of a string: JSON with `<`, `>`, `&`, U+2028 and U+2029
/// escaped as `\uXXXX`.
pub fn json_string(s: &str) -> String {
    let encoded = serde_json::to_string(s).expect("strings encode");
    encoded
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}
