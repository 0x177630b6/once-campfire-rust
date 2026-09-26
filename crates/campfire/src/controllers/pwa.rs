//! `PwaController` (reference/app/controllers/pwa_controller.rb): the web app manifest and the
//! service worker, at stable URLs.

use askama::Template;
use campfire_db::Account;
use campfire_kit::{Ctx, Error, Response, Result, StatusCode, format};
use campfire_views::pwa;

use crate::app::AppCtx;
use crate::concerns::{self, Before};
use crate::controllers::presenters_a;

/// `allow_unauthenticated_access`, `skip_forgery_protection`
fn before() -> Before {
    Before::default().allow_unauthenticated_access().skip_forgery_protection()
}

/// `pwa/service_worker.js`
pub async fn service_worker(c: &mut Ctx) -> Result {
    concerns::before_actions(c, before()).await?;
    c.respond_to(&[&format::JS])?;
    let response = Response::with_body(StatusCode::OK, "text/javascript; charset=utf-8", pwa::SERVICE_WORKER_JS);
    Ok(presenters_a::view_context::vary_by_accept(c, response))
}

/// `pwa/manifest.json.erb`
pub async fn manifest(c: &mut Ctx) -> Result {
    concerns::before_actions(c, before()).await?;
    c.respond_to(&[&format::JSON])?;
    let account = c.app().db.read(Account::first).await.map_err(Error::internal)?;
    let asset_path = |path: &str| campfire_assets::asset_path(path);
    let manifest = pwa::Manifest {
        account_name: account.as_ref().map(|account| account.name.clone()),
        logo_path_small: presenters_a::fresh_account_logo_path(account.as_ref(), Some("small")),
        logo_path: presenters_a::fresh_account_logo_path(account.as_ref(), None),
        base_url: c.url_for(""),
        asset_path: &asset_path,
    };
    let body = manifest.render().map_err(Error::internal)?;
    let response = Response::with_body(StatusCode::OK, "application/json; charset=utf-8", body);
    Ok(presenters_a::view_context::vary_by_accept(c, response))
}
