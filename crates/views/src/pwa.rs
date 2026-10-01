//! Views for `reference/app/views/pwa`.

use askama::Template;

use crate::ViewContext;
use crate::helpers as h;

/// `pwa/service_worker.js`, served verbatim.
pub const SERVICE_WORKER_JS: &str = include_str!("../templates/pwa/service_worker.js");

/// `pwa/manifest.json.erb`. ERB HTML-escapes the values into the JSON, so an account named `a\b`
/// or `"a"` made the manifest invalid and the logo URL came out as `?size=small&amp;v=...`; the
/// values are JSON strings here.
#[derive(Template)]
#[template(path = "pwa/manifest.json")]
pub struct Manifest<'a> {
    /// `Current.account&.name`.
    pub account_name: Option<String>,
    /// `fresh_account_logo_path(size: :small)`.
    pub logo_path_small: String,
    /// `fresh_account_logo_path`.
    pub logo_path: String,
    /// Hermes fork: an account logo is uploaded (the maskable icon is then the logo, as upstream's).
    pub has_logo: bool,
    /// `request.base_url`, for `image_url`.
    pub base_url: String,
    pub asset_path: &'a dyn Fn(&str) -> String,
}

impl Manifest<'_> {
    /// `image_url(source)`.
    fn image_url(&self, source: &str) -> String {
        format!("{}{}", self.base_url, (self.asset_path)(source))
    }

    /// Hermes fork (`templates-hermes/pwa/manifest.json`): what follows renders upstream's values
    /// until the app installs the branding (`crate::hermes::PageAssets`), so the golden still
    /// compares upstream's manifest.
    ///
    /// The account's name, else the product's.
    fn name(&self) -> &str {
        self.account_name.as_deref().unwrap_or(crate::hermes::product_name())
    }

    /// The home screen label: the product's name, whatever the account is called (upstream has none).
    fn short_name(&self) -> Option<&'static str> {
        crate::hermes::branded().then_some(crate::hermes::PRODUCT_NAME)
    }

    /// Upstream's maskable icon is the plain logo. Without an uploaded logo, the product's default
    /// has a variant with the mark inside the safe zone.
    fn maskable_path(&self) -> String {
        if self.has_logo || !crate::hermes::branded() { self.logo_path.clone() } else { self.image_url(crate::hermes::MASKABLE_ICON) }
    }

    fn description(&self) -> &'static str {
        if crate::hermes::branded() {
            "Team chat, tickets and voice reports for the people on duty."
        } else {
            "A chat app from the makers of Basecamp and HEY."
        }
    }

    /// Android's splash screen and title bar before the page loads: the theme's light surface
    /// (the manifest has no dark variant; the page's `theme-color` metas take over once loaded).
    fn theme_color(&self) -> &'static str {
        if crate::hermes::themed() { crate::hermes::THEME_COLOR_LIGHT } else { "#ffffff" }
    }

    /// Upstream's screenshots show Campfire's look and name: left out once branded.
    fn upstream_screenshots(&self) -> bool {
        !crate::hermes::branded()
    }

    /// `value` as a JSON string, quotes included.
    fn json(&self, value: &str) -> askama::filters::Safe<String> {
        askama::filters::Safe(serde_json::to_string(value).expect("a string serializes"))
    }
}

/// `pwa/_install_instructions.html.erb`.
#[derive(Template)]
#[template(path = "pwa/_install_instructions.html")]
pub struct InstallInstructions<'a> {
    pub ctx: &'a ViewContext<'a>,
}

/// `pwa/_browser_settings.html.erb`.
#[derive(Template)]
#[template(path = "pwa/_browser_settings.html")]
pub struct BrowserSettings<'a> {
    pub ctx: &'a ViewContext<'a>,
}

/// `pwa/_system_settings.html.erb`.
#[derive(Template)]
#[template(path = "pwa/_system_settings.html")]
pub struct SystemSettings<'a> {
    pub ctx: &'a ViewContext<'a>,
}
