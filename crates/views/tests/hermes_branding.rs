//! Hermes fork: the product's branding (Meshduty instead of Campfire, docs/hermes-theme.md,
//! "Branding"). The branding is process-wide (`campfire_views::hermes::install_page_assets`), so
//! this is a test binary of its own, with a single test: nothing else renders while it's installed.

mod support;

use askama::Template;
use campfire_views::hermes::{self, PageAssets, install_page_assets};
use campfire_views::*;
use support::facts::*;

/// `page` without the account's own name (data: the owner renames the account), so that what's
/// left is the code's text.
fn without_account_name(page: &str, account_name: &str) -> String {
    if account_name.is_empty() { page.to_string() } else { page.replace(account_name, "") }
}

fn account_name(case_name: &str) -> String {
    str_of(&case(case_name)["account"]["name"]).unwrap_or_default()
}

/// The pages and partials upstream writes "Campfire" in, rendered for the golden cases.
fn rendered_pages() -> Vec<(String, String)> {
    let mut pages = Vec::new();
    for name in ["sessions_new", "sessions_new_with_logo"] {
        let html =
            with_context(name, Request::default(), |ctx| sessions::New { ctx, email_address: None, help_contact: None }.render().unwrap());
        pages.push((name.to_string(), without_account_name(&html, &account_name(name))));
    }
    for name in ["incompatible_browser", "incompatible_browser_apple_messages"] {
        let html = with_context(name, Request::default(), |ctx| sessions::IncompatibleBrowser { ctx }.render().unwrap());
        pages.push((name.to_string(), without_account_name(&html, &account_name(name))));
    }
    let html = with_context("first_run", Request::default(), |ctx| first_runs::Show { ctx }.render().unwrap());
    pages.push(("first_run".into(), html));
    for ua in facts()["user_agents"].as_object().unwrap().keys() {
        for kind in ["system_settings", "install_instructions"] {
            let name = format!("{kind}_{ua}");
            let html = with_context(&name, Request::default(), |ctx| match kind {
                "system_settings" => pwa::SystemSettings { ctx }.render().unwrap(),
                _ => pwa::InstallInstructions { ctx }.render().unwrap(),
            });
            pages.push((name, html));
        }
    }
    pages
}

fn manifest(account_name: Option<&str>, has_logo: bool) -> serde_json::Value {
    let asset_path = |logical: &str| format!("/assets/{logical}");
    let json = pwa::Manifest {
        account_name: account_name.map(Into::into),
        logo_path_small: "/account/logo?size=small&v=1".into(),
        logo_path: "/account/logo?v=1".into(),
        has_logo,
        base_url: "http://campfire.test".into(),
        asset_path: &asset_path,
    }
    .render()
    .unwrap();
    assert!(!json.contains("Campfire") || !hermes::product_name().eq("Meshduty"), "{json}");
    serde_json::from_str(&json).expect("valid JSON")
}

// Keep this ONE test: the branding is a process-wide switch (`PAGE_ASSETS`, installed and removed
// below), and the test harness runs tests in parallel threads. Split into several tests, one would
// install the branding while another expects upstream's bytes, and they'd fail at random. Add new
// branded checks inside it, between `install_page_assets(Some(..))` and `install_page_assets(None)`.
#[test]
fn the_product_is_called_meshduty_once_branded() {
    // Not installed (the goldens): upstream's name.
    assert_eq!(hermes::product_name(), "Campfire");
    let upstream = rendered_pages();
    assert!(upstream.iter().any(|(_, html)| html.contains("Campfire")));
    let upstream_manifest = manifest(None, false);
    assert_eq!(upstream_manifest["name"], "Campfire");
    assert!(upstream_manifest.get("short_name").is_none() && upstream_manifest.get("screenshots").is_some());
    assert_eq!(
        (upstream_manifest["theme_color"].as_str(), upstream_manifest["background_color"].as_str()),
        (Some("#ffffff"), Some("#ffffff"))
    );

    // The app at boot: Meshduty everywhere upstream says Campfire.
    install_page_assets(Some(PageAssets { theme: true, brand: true }));
    assert_eq!(hermes::product_name(), "Meshduty");
    for (name, html) in rendered_pages() {
        assert!(!html.contains("Campfire"), "{name} still says Campfire:\n{html}");
    }
    let sign_in = with_context("sessions_new", Request::default(), |ctx| {
        sessions::New { ctx, email_address: None, help_contact: None }.render().unwrap()
    });
    assert!(sign_in.contains(r##"<meta name="theme-color" content="#f4f0e8" media="(prefers-color-scheme: light)">"##), "{sign_in}");
    assert!(sign_in.contains(r##"<meta name="theme-color" content="#181e1b" media="(prefers-color-scheme: dark)">"##));
    assert!(sign_in.contains(r#"<a href="/" id="app-logo" aria-label="Meshduty home">"#) && sign_in.contains(r#"alt="Meshduty logo""#));
    assert!(!sign_in.contains("once.com"));
    assert!(sign_in.contains(r#"<link rel="apple-touch-icon" href="/assets/hermes/apple-touch-icon.png">"#), "{sign_in}");
    assert!(sign_in.contains(r#"<link rel="icon" href="/assets/hermes/favicon-32.png" type="image/png">"#), "{sign_in}");
    let with_logo = with_context("sessions_new_with_logo", Request::default(), |ctx| {
        assert!(ctx.account.has_logo);
        [hermes::apple_touch_icon_url(ctx), hermes::favicon_url(ctx)]
    });
    assert!(with_logo.iter().all(|url| url.starts_with("/account/logo")), "an uploaded logo wins: {with_logo:?}");
    assert_eq!(helpers::page_title_tag(None).0, "<title>Meshduty</title>");
    assert_eq!(hermes::rebrand("Welcome to Campfire."), "Welcome to Meshduty.");
    assert!(hermes::THEME_SCRIPT.contains(hermes::THEME_COLOR_LIGHT) && hermes::THEME_SCRIPT.contains(hermes::THEME_COLOR_DARK));
    assert!(helpers::translations::translations_for("invite_message").0.contains("Welcome to Meshduty"));

    // The manifest: the product's name, a short name, the theme's colours, the default maskable
    // icon unless a logo is uploaded; the app's identity (start_url, no id) unchanged.
    let branded = manifest(None, false);
    assert_eq!((branded["name"].as_str(), branded["short_name"].as_str()), (Some("Meshduty"), Some("Meshduty")));
    assert_eq!((branded["theme_color"].as_str(), branded["background_color"].as_str()), (Some("#f4f0e8"), Some("#f4f0e8")));
    assert_eq!((branded["start_url"].as_str(), branded["scope"].as_str()), (Some("/"), Some("/")));
    assert!(branded.get("id").is_none() && branded.get("screenshots").is_none());
    assert_eq!(branded["icons"][0]["src"], "/account/logo?size=small&v=1");
    assert_eq!(branded["icons"][1]["src"], "/account/logo?v=1");
    assert_eq!(branded["icons"][2]["src"], "http://campfire.test/assets/hermes/icon-maskable-512.png");
    assert_eq!(branded["icons"][2]["purpose"], "maskable");
    assert_eq!(manifest(None, true)["icons"][2]["src"], "/account/logo?v=1", "an uploaded logo wins");
    assert_eq!(manifest(Some("Hotel Azur"), false)["name"], "Hotel Azur", "the account's name wins");

    // CAMPFIRE_THEME=off: still Meshduty, upstream's chrome colours.
    install_page_assets(Some(PageAssets { theme: false, brand: true }));
    assert_eq!(manifest(None, false)["theme_color"], "#ffffff");
    let sign_in = with_context("sessions_new", Request::default(), |ctx| {
        sessions::New { ctx, email_address: None, help_contact: None }.render().unwrap()
    });
    assert!(sign_in.contains(r##"<meta name="theme-color" content="#ffffff" media="(prefers-color-scheme: light)">"##));

    install_page_assets(None);
    assert_eq!(rendered_pages(), upstream, "removed: upstream's bytes again");
}

/// Every upstream template that says "Campfire" outside a comment is shadowed, and no shadow says
/// it but through `rebrand` (which swaps it) — so an upstream merge adding the name is caught.
#[test]
fn every_upstream_template_naming_campfire_is_shadowed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let without_comments = |source: &str| {
        let mut out = String::new();
        let mut rest = source;
        while let Some(start) = rest.find("{#") {
            out.push_str(&rest[..start]);
            rest = rest[start..].find("#}").map_or("", |end| &rest[start + end + 2..]);
        }
        out + rest
    };
    let mut stack = vec![root.join("templates")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(root.join("templates")).unwrap();
            if relative.starts_with("hermes") || path.extension().is_some_and(|ext| ext == "js") {
                continue;
            }
            let source = without_comments(&std::fs::read_to_string(&path).unwrap());
            if !source.contains("Campfire") {
                continue;
            }
            let shadow = root.join("templates-hermes").join(relative);
            assert!(shadow.exists(), "templates/{} names Campfire: shadow it (templates-hermes)", relative.display());
            let shadowed = without_comments(&std::fs::read_to_string(&shadow).unwrap());
            for line in shadowed.lines().filter(|line| line.contains("Campfire")) {
                assert!(line.contains("rebrand(\""), "templates-hermes/{}: {line}", relative.display());
            }
        }
    }
}
