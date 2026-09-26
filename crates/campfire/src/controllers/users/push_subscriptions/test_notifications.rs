//! `Users::PushSubscriptions::TestNotificationsController`
//! (reference/app/controllers/users/push_subscriptions/test_notifications_controller.rb).

use campfire_db::{Membership, PushSubscription};
use campfire_kit::{Ctx, Error, Result};

use crate::app::AppCtx;
use crate::concerns::{self, Before, cast_integer};
use crate::integrations::net::Network;
use crate::integrations::web_push::{self, VapidConfig};

/// `@push_subscription.notification(title: "Campfire Test", body: Random.uuid, path: user_push_subscriptions_url).deliver`
pub async fn create(c: &mut Ctx) -> Result {
    concerns::before_actions(c, Before::default()).await?;
    let user_id = concerns::require_current_user(c)?.id;
    // `Current.user.push_subscriptions.find(params[:push_subscription_id])`
    let id = c.param_str("push_subscription_id").and_then(cast_integer).ok_or(Error::NotFound)?;
    let (subscription, badge) = c
        .app()
        .db
        .read(move |conn| match PushSubscription::find(conn, id) {
            Ok(subscription) if subscription.user_id == user_id => Ok(Some((subscription, Membership::unread_count(conn, user_id)?))),
            Ok(_) | Err(campfire_db::Error::RecordNotFound(_)) => Ok(None),
            Err(error) => Err(error),
        })
        .await
        .map_err(Error::internal)?
        .ok_or(Error::NotFound)?;

    let location = c.url_for(&campfire_routes::user_push_subscriptions());
    let config = &c.app().config;
    let vapid = VapidConfig::new(config.vapid_public_key.clone().unwrap_or_default(), config.vapid_private_key.clone().unwrap_or_default());
    web_push::deliver_test_notification(&Network::system(), &vapid, &subscription, badge, &location).await
        .map_err(|error| Error::internal(anyhow::anyhow!("{error:?}")))?;
    c.redirect_to(&location)
}
