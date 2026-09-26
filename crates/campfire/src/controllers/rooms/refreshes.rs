//! `Rooms::RefreshesController` (reference/app/controllers/rooms/refreshes_controller.rb): what
//! changed in a room since the client last loaded it.

use askama::Template;
use campfire_db::{Message, Timestamp};
use campfire_kit::{Ctx, Error, Result, StatusCode, format};
use campfire_views::rooms::{RefreshShow, RefreshView};

use crate::app::AppCtx;
use crate::concerns::{self, Before, before_actions};
use crate::controllers::presenters::page::{self, db_error};
use crate::controllers::presenters::{Presenter, room_kind};

pub async fn show(c: &mut Ctx) -> Result {
    before_actions(c, Before::default()).await?;
    let (_, room) = concerns::set_room(c).await?;
    let last_updated_at = set_last_updated_at(c)?;
    c.respond_to(&[&format::TURBO_STREAM])?;

    let app = c.app().clone();
    let request_host = Some(c.request.host());
    let refresh = c
        .app()
        .db
        .read(move |conn| {
            let new_messages = Message::page_created_since(conn, room.id, last_updated_at)?;
            let new_ids: Vec<i64> = new_messages.iter().map(|message| message.id).collect();
            let updated_messages = Message::page_updated_since(conn, room.id, last_updated_at, &new_ids)?;
            let presenter = Presenter::new(conn, &app.secrets, &app.storage, &*app.db.env().rich_text, app.clock.now(), request_host);
            Ok(RefreshView {
                room_id: room.id,
                room_kind: room_kind(room.room_type),
                new_messages: presenter.messages(&new_messages)?,
                updated_messages: presenter.messages(&updated_messages)?,
            })
        })
        .await
        .map_err(db_error)?;
    page::bare(c, StatusCode::OK, &format::TURBO_STREAM, |ctx| RefreshShow { ctx, refresh: &refresh }.render()).await
}

/// `Time.at(0, params[:since].to_i, :millisecond)`
fn set_last_updated_at(c: &Ctx) -> Result<Timestamp> {
    let since = match c.param("since") {
        None => 0,
        Some(param) => match param.as_str() {
            Some(value) => ruby_to_i(value),
            None if param.is_null() => 0,
            // `to_i` isn't defined for a hash or an array.
            None => return Err(Error::internal(anyhow::anyhow!("undefined method 'to_i'"))),
        },
    };
    Ok(Timestamp::from_microsecond(since.saturating_mul(1000)))
}

/// `String#to_i`: optional leading whitespace and sign, then digits (underscores between them).
pub fn ruby_to_i(value: &str) -> i64 {
    let value = value.trim_start();
    let (negative, rest) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let mut number: i64 = 0;
    let mut previous_underscore = false;
    for (i, c) in rest.char_indices() {
        match c {
            '0'..='9' => {
                number = number.saturating_mul(10).saturating_add(c as i64 - '0' as i64);
                previous_underscore = false;
            }
            '_' if i > 0 && !previous_underscore => previous_underscore = true,
            _ => break,
        }
    }
    if negative { -number } else { number }
}

#[cfg(test)]
mod tests {
    use super::ruby_to_i;

    #[test]
    fn to_i_like_ruby() {
        assert_eq!(ruby_to_i("1717243200000"), 1717243200000);
        assert_eq!(ruby_to_i(" 12abc"), 12);
        assert_eq!(ruby_to_i("abc"), 0);
        assert_eq!(ruby_to_i("-5"), -5);
        assert_eq!(ruby_to_i("1_000"), 1000);
    }
}
