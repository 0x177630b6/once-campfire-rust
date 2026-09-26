//! Askama templates mirroring `reference/app/views`, one template file per ERB file at the same
//! relative path under `templates/`. Views take plain view-model structs defined here, never
//! database rows, so this crate doesn't depend on `campfire_db`. Rich text arrives pre-rendered
//! as sanitized HTML.
//!
//! Ownership: views agent A owns `helpers`, `layouts`, `sessions`, `first_runs`, `users`,
//! `accounts`, `welcome`, `pwa`, `autocompletable`. Views agent B owns `rooms`, `messages`,
//! `searches`. Both share `ViewContext` below; changes to it go through NOTES.md.

pub mod helpers;
pub mod layouts;
pub mod sessions;
pub mod first_runs;
pub mod users;
pub mod accounts;
pub mod welcome;
pub mod pwa;
pub mod autocompletable;
pub mod rooms;
pub mod messages;
pub mod searches;

/// Per-request state every page needs: what `ApplicationController`, the layout and the
/// helpers read from `Current`, `request`, `flash` and the session.
pub struct ViewContext<'a> {
    pub current_user: Option<CurrentUser>,
    pub account: AccountSummary,
    /// A freshly masked CSRF token for `csrf_meta_tags` and form hidden fields.
    pub csrf_token: String,
    pub flash_notice: Option<String>,
    pub flash_alert: Option<String>,
    /// `ApplicationPlatform` facts derived from the user agent.
    pub platform: Platform,
    pub vapid_public_key: String,
    /// Resolves a logical asset path ("campfire-icon.png") to its digested URL.
    pub asset_path: &'a dyn Fn(&str) -> String,
    /// The `<script type="importmap">` + modulepreload tags (`javascript_importmap_tags`).
    pub importmap_tags: &'a str,
    /// `<link rel="stylesheet">` tags for `stylesheet_link_tag :all`.
    pub stylesheet_tags: &'a str,
    /// The account's custom CSS, if any (`custom_styles_tag`).
    pub custom_styles: Option<String>,
    /// Signed Action Cable URL meta value (`script_aware_action_cable_meta_tag`).
    pub cable_url: String,
}

#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub id: i64,
    pub name: String,
    pub administrator: bool,
    pub bot: bool,
    pub avatar_url: String,
}

#[derive(Clone, Debug)]
pub struct AccountSummary {
    pub name: String,
    pub logo_url: String,
}

#[derive(Clone, Debug, Default)]
pub struct Platform {
    pub ios: bool,
    pub android: bool,
    pub mac: bool,
    pub windows: bool,
    pub chrome: bool,
    pub firefox: bool,
    pub safari: bool,
    pub edge: bool,
    pub mobile: bool,
    pub desktop: bool,
}
