//! Views for `reference/app/views/messages`, plus `MessagesHelper`,
//! `Messages::AttachmentPresentation` and the boost partials.

pub mod json;
pub mod presentation;
pub mod support;

use askama::Template;
use campfire_routes as routes;
use jiff::Timestamp;
use serde::Deserialize;

use crate::helpers as h;
use crate::ViewContext;
use support::{epoch_ms, iso8601, RubyNumber};

/// What the message views show of a user: `avatar_tag` and the author heading.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct UserView {
    pub id: i64,
    pub name: String,
    /// `User#title`: name and bio joined by " – ".
    pub title: String,
    /// `fresh_user_avatar_path(user)`.
    pub avatar_url: String,
}

impl UserView {
    pub fn path(&self) -> String {
        routes::user(self.id)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    Open,
    Closed,
    Direct,
}

impl RoomKind {
    /// `Rooms::Open.model_name.param_key`, the stem of `dom_id(room)`.
    pub fn param_key(self) -> &'static str {
        match self {
            RoomKind::Open => "rooms_open",
            RoomKind::Closed => "rooms_closed",
            RoomKind::Direct => "rooms_direct",
        }
    }

    pub fn is_direct(self) -> bool {
        self == RoomKind::Direct
    }
}

/// `dom_id(room)` / `dom_id(room, prefix)`.
pub fn room_dom_id(kind: RoomKind, id: i64, prefix: &str) -> String {
    if prefix.is_empty() {
        format!("{}_{id}", kind.param_key())
    } else {
        format!("{prefix}_{}_{id}", kind.param_key())
    }
}

/// A message as `messages/_message` renders it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MessageView {
    pub id: i64,
    /// `Message#to_key`, so every `dom_id(message)` uses it.
    pub client_message_id: String,
    pub room_id: i64,
    /// `room_display_name(message.room, for_user: nil)`; see [`crate::rooms::room_display_name`].
    pub room_name: String,
    pub creator: UserView,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// `message.plain_text_body.all_emoji?` (`reference/lib/rails_ext/string.rb`).
    pub all_emoji: bool,
    pub content: MessageContent,
    /// `message.boosts.ordered`.
    #[serde(default)]
    pub boosts: Vec<BoostView>,
}

/// `Message#content_type` with what each presentation needs.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    /// The presentation filters' output after `auto_link`, from the richtext crate.
    Text { html: String },
    Sound(SoundView),
    Attachment(AttachmentView),
}

/// A `/play <name>` message's `Sound`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SoundView {
    /// `asset_path(sound.asset_path)`, the digested mp3.
    pub url: String,
    pub image: Option<SoundImage>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SoundImage {
    /// `image_path(image.asset_path)`.
    pub src: String,
    pub width: u32,
    pub height: u32,
}

/// The message's Active Storage attachment.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AttachmentView {
    /// `attachment.filename.to_s`.
    pub filename: String,
    /// `rails_blob_path(attachment)`.
    pub blob_path: String,
    /// `rails_blob_path(attachment, disposition: "attachment")`.
    pub download_path: String,
    pub preview: AttachmentPreview,
    /// `attachment.metadata[:width]`: an Integer for images, a Float for videos.
    pub width: Option<RubyNumber>,
    pub height: Option<RubyNumber>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachmentPreview {
    /// `attachment.video?`: `url_for(attachment.preview(format: :webp, resize_to_limit: ...))`.
    Video { poster_url: String },
    /// Otherwise previewable or variable: `polymorphic_url(attachment.representation(:thumb), only_path: true)`.
    Image { thumb_url: String },
    /// Neither previewable nor variable: a download link.
    File,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BoostView {
    pub id: i64,
    pub message_id: i64,
    pub content: String,
    /// `boost.content.all_emoji?`.
    pub all_emoji: bool,
    pub booster: UserView,
}

/// `EmojiHelper::REACTIONS`.
pub const REACTIONS: [(&str, &str); 8] = [
    ("👍", "Thumbs up"),
    ("👏", "Clapping"),
    ("👋", "Waving hand"),
    ("💪", "Muscle"),
    ("❤️", "Red heart"),
    ("😂", "Face with tears of joy"),
    ("🎉", "Party popper"),
    ("🔥", "Fire"),
];

impl MessageView {
    /// `dom_id(message)` / `dom_id(message, prefix)`.
    pub fn dom_id(&self, prefix: &str) -> String {
        if prefix.is_empty() {
            format!("message_{}", self.client_message_id)
        } else {
            format!("{prefix}_message_{}", self.client_message_id)
        }
    }

    pub fn attachment(&self) -> Option<&AttachmentView> {
        match &self.content {
            MessageContent::Attachment(attachment) => Some(attachment),
            _ => None,
        }
    }

    pub fn created_at_iso(&self) -> String {
        iso8601(self.created_at)
    }

    pub fn created_at_epoch(&self) -> i64 {
        epoch_ms(self.created_at)
    }

    pub fn updated_at_epoch(&self) -> i64 {
        epoch_ms(self.updated_at)
    }

    pub fn at_path(&self) -> String {
        routes::room_at_message(self.room_id, self.id)
    }

    pub fn path(&self) -> String {
        routes::room_message(self.room_id, self.id)
    }

    pub fn edit_path(&self) -> String {
        routes::edit_room_message(self.room_id, self.id)
    }

    pub fn boosts_path(&self) -> String {
        routes::message_boosts(self.id)
    }

    pub fn new_boost_path(&self) -> String {
        routes::new_message_boost(self.id)
    }
}

impl BoostView {
    pub fn dom_id(&self) -> String {
        format!("boost_{}", self.id)
    }

    pub fn path(&self) -> String {
        routes::message_boost(self.message_id, self.id)
    }
}

/// `messages/_message`.
#[derive(Template)]
#[template(path = "messages/_message.html")]
pub struct MessagePartial<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
}

pub fn message(ctx: &ViewContext, message: &MessageView) -> String {
    MessagePartial { ctx, message }.render().expect("messages/_message renders")
}

/// `render partial: "messages/message", collection: messages`.
pub fn message_collection(ctx: &ViewContext, messages: &[MessageView]) -> String {
    messages.iter().map(|m| message(ctx, m)).collect()
}

/// `messages/index`: the page of messages the client fetches while scrolling (no layout).
#[derive(Template)]
#[template(path = "messages/index.html")]
pub struct Index<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub messages: &'a [MessageView],
}

/// `messages/show`: the message partial, inside the application layout.
#[derive(Template)]
#[template(path = "messages/show.html")]
pub struct Show<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
}

/// `messages/_presentation`, which `MessagesController#update` also broadcasts.
#[derive(Template)]
#[template(path = "messages/_presentation.html")]
pub struct PresentationPartial<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
}

/// `messages/_template`: the client-side template for messages being sent.
#[derive(Template)]
#[template(path = "messages/_template.html")]
pub struct TemplatePartial<'a> {
    pub ctx: &'a ViewContext<'a>,
    /// `Current.user`.
    pub user: &'a UserView,
}

/// `messages/_unrenderable`.
#[derive(Template)]
#[template(path = "messages/_unrenderable.html")]
pub struct Unrenderable;

/// `messages/room_not_found`.
#[derive(Template)]
#[template(path = "messages/room_not_found.html")]
pub struct RoomNotFound;

/// What `messages/edit` needs besides the message.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct EditView {
    pub message: MessageView,
    /// The editor's `value`: `editable_body(message)` as HTML, from the richtext crate.
    pub editable_body_html: String,
}

/// `messages/edit`.
#[derive(Template)]
#[template(path = "messages/edit.html")]
pub struct Edit<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub edit: &'a EditView,
}

/// `messages/create.turbo_stream`: appends the new message to its room's list. Also what
/// `Message#broadcast_create` sends.
#[derive(Template)]
#[template(path = "messages/create.turbo_stream.html")]
pub struct CreateStream<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
    pub room_kind: RoomKind,
}

/// `messages/destroy.turbo_stream`, also what `Message#broadcast_remove` sends.
#[derive(Template)]
#[template(path = "messages/destroy.turbo_stream.html")]
pub struct DestroyStream<'a> {
    pub message: &'a MessageView,
}

/// `messages/boosts/_boosts`.
#[derive(Template)]
#[template(path = "messages/boosts/_boosts.html")]
pub struct BoostsPartial<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
}

/// `messages/boosts/index`.
#[derive(Template)]
#[template(path = "messages/boosts/index.html")]
pub struct BoostsIndex<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
}

/// `messages/boosts/_boost`, which the boosts controller also broadcasts on its own.
#[derive(Template)]
#[template(path = "messages/boosts/_boost.html")]
pub struct BoostPartial<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub boost: &'a BoostView,
}

/// `messages/boosts/new`.
#[derive(Template)]
#[template(path = "messages/boosts/new.html")]
pub struct NewBoost<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub message: &'a MessageView,
    /// `Current.user`.
    pub user: &'a UserView,
}

/// The turbo streams the message and boost controllers broadcast to `[room, :messages]`.
pub mod broadcasts {
    use askama::Template;

    use super::support::turbo_stream;
    use super::{BoostPartial, BoostView, MessageView, PresentationPartial};
    use crate::ViewContext;

    /// `MessagesController#update`: replaces the message's presentation, keeping the scroll.
    pub fn replace_presentation(ctx: &ViewContext, message: &MessageView) -> String {
        let content = PresentationPartial { ctx, message }.render().expect("messages/_presentation renders");
        turbo_stream("replace", &message.dom_id("presentation"), &content, true)
    }

    /// `Messages::BoostsController#broadcast_create`.
    pub fn append_boost(ctx: &ViewContext, boost: &BoostView, message_client_id: &str) -> String {
        let content = BoostPartial { ctx, boost }.render().expect("messages/boosts/_boost renders");
        turbo_stream("append", &format!("boosts_message_{message_client_id}"), &content, true)
    }

    /// `Messages::BoostsController#broadcast_remove`.
    pub fn remove_boost(boost: &BoostView) -> String {
        turbo_stream("remove", &boost.dom_id(), "", false)
    }
}
