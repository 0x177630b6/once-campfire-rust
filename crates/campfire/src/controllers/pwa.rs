//! `PwaController` (reference/app/controllers/pwa_controller.rb): the web app manifest and the
//! service worker, at stable URLs.

use askama::Template;
use campfire_db::Account;
use campfire_kit::{Ctx, Error, Result, StatusCode, format};
use campfire_views::pwa;

use crate::app::AppCtx;
use crate::concerns::{self, Before};
use crate::controllers::presenters;

/// `allow_unauthenticated_access`, `skip_forgery_protection`
fn before() -> Before {
    Before::default().allow_unauthenticated_access().skip_forgery_protection()
}

/// `pwa/service_worker.js`
pub async fn service_worker(c: &mut Ctx) -> Result {
    concerns::before_actions(c, before()).await?;
    c.respond_to(&[&format::JS])?;
    Ok(c.render_as(StatusCode::OK, "text/javascript; charset=utf-8", pwa::SERVICE_WORKER_JS))
}

/// `pwa/manifest.json.erb`
pub async fn manifest(c: &mut Ctx) -> Result {
    concerns::before_actions(c, before()).await?;
    c.respond_to(&[&format::JSON])?;
    let (account, has_logo) = c
        .app()
        .db
        .read(|conn| {
            let account = Account::first(conn)?;
            // Hermes fork: whether the maskable icon is the uploaded logo or the product's default.
            let has_logo = match &account {
                Some(account) => presenters::attachments::attached_blob(conn, "Account", account.id, "logo")?.is_some(),
                None => false,
            };
            Ok((account, has_logo))
        })
        .await
        .map_err(Error::internal)?;
    let asset_path = |path: &str| campfire_assets::asset_path(path);
    let manifest = pwa::Manifest {
        account_name: account.as_ref().map(|account| account.name.clone()),
        logo_path_small: presenters::accounts::fresh_account_logo_path(account.as_ref(), Some("small")),
        logo_path: presenters::accounts::fresh_account_logo_path(account.as_ref(), None),
        has_logo,
        base_url: c.url_for(""),
        asset_path: &asset_path,
    };
    let body = manifest.render().map_err(Error::internal)?;
    Ok(c.render_as(StatusCode::OK, "application/json; charset=utf-8", body))
}
