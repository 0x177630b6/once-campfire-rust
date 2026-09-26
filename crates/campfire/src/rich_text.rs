//! `campfire_db::RichText` over `campfire_richtext`, for the models' Action Text needs
//! (`plain_text_body` for FTS, push and webhooks; `mentionees`).
//!
//! The models call this on the database writer thread, inside their transaction. Record lookups
//! go through a reader connection with the one resolver implementation the controllers use
//! (`controllers::presenters::DbResolver`); mentioned users are always committed rows already.

use std::sync::{Arc, OnceLock};

use campfire_db::{BasicRichText, Database, RichText};
use campfire_kit::SharedClock;
use campfire_richtext::RenderContext;
use rails_compat::Secrets;

use crate::controllers::presenters::DbResolver;

pub struct AppRichText {
    secrets: Arc<Secrets>,
    clock: SharedClock,
    /// Set once the database is open (the database needs this before it exists).
    db: Arc<OnceLock<Database>>,
}

impl AppRichText {
    pub fn new(secrets: Arc<Secrets>, clock: SharedClock) -> Self {
        Self { secrets, clock, db: Arc::new(OnceLock::new()) }
    }

    pub fn set_database(&self, db: Database) {
        let _ = self.db.set(db);
    }

    fn with_context<T>(&self, f: impl FnOnce(&RenderContext) -> T) -> Option<T> {
        let db = self.db.get()?;
        db.read_blocking(|conn| {
            let resolver = DbResolver { conn, secrets: &self.secrets, now: self.clock.now() };
            Ok(f(&resolver.render_context(None)))
        })
        .ok()
    }
}

impl RichText for AppRichText {
    /// `message.body.to_plain_text`. Where Rails would raise, the save would fail; the models
    /// can't fail here, so it's logged and the tag-stripped text is used instead.
    fn to_plain_text(&self, html: &str, user_names: campfire_db::rich_text::UserNames<'_>) -> String {
        match self.with_context(|ctx| campfire_richtext::to_plain_text(html, ctx)) {
            Some(Ok(text)) => text,
            Some(Err(error)) => {
                tracing::error!(%error, "to_plain_text raised");
                BasicRichText.to_plain_text(html, user_names)
            }
            None => BasicRichText.to_plain_text(html, user_names),
        }
    }

    /// `body.attachables.grep(User).uniq`: verified SGIDs only.
    fn mentioned_user_ids(&self, html: &str) -> Vec<i64> {
        match self.with_context(|ctx| campfire_richtext::mentioned_users(html, ctx)) {
            Some(Ok(users)) => users.into_iter().map(|user| user.id).collect(),
            Some(Err(error)) => {
                tracing::error!(%error, "mentioned_users raised");
                Vec::new()
            }
            None => BasicRichText.mentioned_user_ids(html),
        }
    }
}
