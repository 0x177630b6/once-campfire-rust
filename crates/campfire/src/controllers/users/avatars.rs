//! `Users::AvatarsController` (reference/app/controllers/users/avatars_controller.rb): a user's
//! avatar by its signed avatar token — the uploaded image's `:square` variant, the default bot
//! avatar, or an SVG of their initials.

use askama::Template;
use campfire_db::User;
use campfire_kit::{Ctx, Error, ExpiresIn, Freshness, Result, SendOptions, format, halt};
use campfire_storage::Variation;
use campfire_views::users::AvatarSvg;

use crate::app::AppCtx;
use crate::concerns::{self, Before};
use crate::controllers::presenters_a::attachments::{self, Record};
use crate::controllers::presenters_a::{self, cache_key_with_version};

/// `ActionView::Digestor.digest(name: "users/avatars/show", ...)`: the SHA256 (truncated) of
/// `show.svg.erb`'s source plus "-" (it renders nothing else). `EtagWithTemplateDigest` adds it
/// whenever the template can be found for the request's formats.
const TEMPLATE_DIGEST: &str = "d500db55e2a67222018ef0156839c3c9";

/// `expires_in 30.minutes, public: true, stale_while_revalidate: 1.week`
const MAX_AGE: u64 = 30 * 60;
const STALE_WHILE_REVALIDATE: u64 = 7 * 24 * 60 * 60;

pub async fn show(c: &mut Ctx) -> Result {
    concerns::before_actions(c, Before::default()).await?;
    let user = from_avatar_token(c).await?;

    let freshness = Freshness {
        etag: Some(cache_key_with_version("users", user.id, user.updated_at.jiff())),
        template: template_found(c).then(|| TEMPLATE_DIGEST.to_string()),
        ..Freshness::default()
    };
    if let Some(not_modified) = c.fresh_when(freshness) {
        return Ok(not_modified);
    }
    c.expires_in(MAX_AGE, ExpiresIn { public: true, stale_while_revalidate: Some(STALE_WHILE_REVALIDATE), ..ExpiresIn::default() });

    if let Some(variant) = avatar_variant(c, &user).await? {
        let path = c.app().storage.service.path_for(&variant.key);
        c.send_file(path, SendOptions::inline("image/webp"))
    } else if user.is_bot() {
        render_default_bot(c)
    } else {
        render_initials(&user)
    }
}

/// `Current.user.avatar.destroy`, then back to the profile.
pub async fn destroy(c: &mut Ctx) -> Result {
    concerns::before_actions(c, Before::default()).await?;
    let user_id = concerns::require_current_user(c)?.id;
    c.app()
        .db
        .write(move |tx| attachments::destroy(tx, Record::user(user_id), "avatar"))
        .await
        .map_err(Error::internal)?;
    let location = c.url_for(&campfire_routes::user_profile());
    c.redirect_to(&location)
}

/// `User.from_avatar_token(params[:user_id])`: a bad signature is `head :not_found`
/// (`rescue_from ActiveSupport::MessageVerifier::InvalidSignature`); a valid one for a missing
/// user is `ActiveRecord::RecordNotFound`.
async fn from_avatar_token(c: &mut Ctx) -> Result<User> {
    let token = c.param_str("user_id").unwrap_or_default().to_string();
    let Some(user_id) = presenters_a::user_id_from_avatar_token(&c.app().secrets, &token, c.now()) else {
        return halt(c.head(campfire_kit::StatusCode::NOT_FOUND));
    };
    c.app().db.read(move |conn| User::find_by_id(conn, user_id)).await.map_err(Error::internal)?.ok_or(Error::NotFound)
}

/// Whether `lookup_context.find_all("show", ["users/avatars", ...])` finds `show.svg.erb` for the
/// request's formats: `*/*` or svg.
fn template_found(c: &mut Ctx) -> bool {
    c.formats().unwrap_or_default().iter().any(|format| **format == format::ALL || **format == format::SVG)
}

/// `avatar.variant(:square).processed if avatar.variable?` (`resize_to_limit: [512, 512], format: :webp`).
async fn avatar_variant(c: &Ctx, user: &User) -> Result<Option<campfire_storage::Blob>> {
    attachments::processed_variant(c.app(), Record::user(user.id), "avatar", Variation::resize_to_limit(512, 512, Some("webp"))).await
}

/// `send_file Rails.root.join("app/assets/images/default-bot-avatar.svg"), content_type: "image/svg+xml", disposition: :inline`
fn render_default_bot(c: &mut Ctx) -> Result {
    let path = asset_file("default-bot-avatar.svg")?;
    c.send_file(path, SendOptions::inline("image/svg+xml"))
}

/// `render formats: :svg` (`users/avatars/show.svg.erb`).
fn render_initials(user: &User) -> Result {
    let svg = AvatarSvg { user_id: user.id, initials: user.initials() }.render().map_err(Error::internal)?;
    // No `Vary: Accept` here: ActiveStorage::Streaming makes this an ActionController::Live
    // response, which the reference sends without it.
    Ok(campfire_kit::Response::with_body(campfire_kit::StatusCode::OK, "image/svg+xml; charset=utf-8", svg))
}

/// A file under `app/assets/images` (embedded by campfire_assets) on disk, for `send_file`: it's
/// written once per process to a private directory under its own name, so the response carries
/// the same filename and, like Rails' file bodies, gets no `Rack::ETag` digest.
pub fn asset_file(logical_path: &str) -> Result<std::path::PathBuf> {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = match DIR.get() {
        Some(dir) => dir,
        None => {
            let dir = tempfile::Builder::new().prefix("campfire-assets-").tempdir()?;
            DIR.get_or_init(|| dir)
        }
    };
    let path = dir.path().join(logical_path);
    if !path.exists() {
        let url = campfire_assets::asset_path(logical_path);
        let request = campfire_assets::StaticRequest { method: "GET", path: &url, ..Default::default() };
        let data = campfire_assets::serve(&request).ok_or_else(|| Error::internal(anyhow::anyhow!("missing asset {logical_path}")))?.body;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&partial, &data)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(path)
}
