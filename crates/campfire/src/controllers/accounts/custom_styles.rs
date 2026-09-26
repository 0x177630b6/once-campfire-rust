//! `Accounts::CustomStylesController` (reference/app/controllers/accounts/custom_styles_controller.rb).

use askama::Template;
use campfire_kit::{Ctx, Error, Param, Redirect, Result, StatusCode, format, permit_keys};
use campfire_views::accounts;

use crate::app::AppCtx;
use crate::concerns::{self, Before};
use crate::controllers::presenters_a::view_context;

/// `before_action :ensure_can_administer, :set_account`
pub async fn edit(c: &mut Ctx) -> Result {
    concerns::before_actions(c, Before::default()).await?;
    concerns::ensure_can_administer(c)?;
    let account = super::current_account(c).await?;
    c.respond_to(&[&format::HTML])?;
    let custom_styles = account.custom_styles;
    view_context::page(c, StatusCode::OK, |ctx| accounts::CustomStylesEdit { ctx, custom_styles }.render()).await
}

/// `@account.update!(params.require(:account).permit(:custom_styles))`
pub async fn update(c: &mut Ctx) -> Result {
    concerns::before_actions(c, Before::default()).await?;
    concerns::ensure_can_administer(c)?;
    let mut account = super::current_account(c).await?;
    let params = c.params.require("account")?.permit(&permit_keys(&["custom_styles"]));
    let custom_styles = params.contains_key("custom_styles").then(|| params.get("custom_styles").and_then(Param::to_s));
    c.app()
        .db
        .write(move |tx| account.update(tx, None, custom_styles.as_ref().map(|styles| styles.as_deref()), None))
        .await
        .map_err(Error::internal)?;
    let location = c.url_for(&campfire_routes::edit_account_custom_styles());
    c.redirect_to_with(&location, Redirect { notice: Some("✓".into()), ..Redirect::default() })
}
