//! Hermes fork: the layout's head seam (`campfire_views::hermes::head_tags`), which links the fork's
//! stylesheets and the product theme (docs/hermes-theme.md). The page assets and the workspace hooks
//! are process-wide, so this is a test binary of its own, with a single test: nothing else renders
//! while they're installed.

mod messages_support;

use std::sync::Arc;

use askama::Template;
use campfire_views::hermes::{
    PageAssets, THEME_SCRIPT, VoiceShow, VoiceView, WorkspaceHooks, color_scheme_switch, install_page_assets, install_workspace_hooks,
};
use campfire_views::messages::MessageView;
use campfire_views::rooms::{self, ShowView};
use campfire_views::{AccountSummary, Platform, ViewContext};
use messages_support::golden;

const UPSTREAM_STYLESHEET: &str = r#"<link rel="stylesheet" href="/assets/base.css" data-turbo-track="reload" />"#;
const CUSTOM_STYLES: &str = r#"<style data-turbo-track="reload">body { --x: 1 }</style>"#;

/// A signed-out context with Custom styles, whose assets all resolve to `/assets/<logical path>`.
fn render(f: impl FnOnce(&ViewContext) -> String) -> String {
    let asset_path = |logical: &str| format!("/assets/{logical}");
    let ctx = ViewContext {
        current_user: None,
        account: AccountSummary { name: "Campfire".into(), logo_url: "/account/logo".into(), has_logo: false },
        flash_notice: None,
        flash_alert: None,
        platform: Platform::default(),
        vapid_public_key: None,
        asset_path: &asset_path,
        importmap_tags: "",
        stylesheet_tags: UPSTREAM_STYLESHEET,
        custom_styles: Some("body { --x: 1 }".into()),
        cable_url: "/cable".into(),
        base_url: "http://campfire.test".into(),
        request_url: "http://campfire.test/".into(),
        referrer: None,
        last_room_visited_id: None,
        app_version: "0".into(),
    };
    f(&ctx)
}

fn room_page() -> String {
    let g = golden("rooms_show_closed");
    let mut show: ShowView = g.input();
    show.voice_note = true;
    render(|ctx| rooms::Show { ctx, show: &show }.render().unwrap())
}

fn voice_page() -> String {
    let voice = VoiceView {
        room_id: 7,
        room_name: "Atelier".into(),
        room_url: "/rooms/7".into(),
        token_url: "/rooms/7/voice/token".into(),
        report_url: "/rooms/7/voice/report".into(),
        ask_url: None,
        worklet_url: "/assets/voice/pcm-worklet.js".into(),
    };
    render(|ctx| VoiceShow { ctx, voice: &voice }.render().unwrap())
}

fn link(logical: &str) -> String {
    format!("\n    <link rel=\"stylesheet\" href=\"/assets/{logical}\" data-turbo-track=\"reload\" />")
}

/// The light/dark switch's inline script, after the theme's link.
fn script() -> String {
    format!("\n    <script>{}</script>", THEME_SCRIPT.trim_end())
}

struct Workspace;

impl WorkspaceHooks for Workspace {
    fn stylesheets(&self, ctx: &ViewContext) -> Vec<String> {
        vec![ctx.asset("hermes/workspace.css")]
    }

    fn layout_overlay(&self, _ctx: &ViewContext) -> String {
        String::new()
    }

    fn message_html(&self, _message: &MessageView, _html: &str) -> Option<String> {
        None
    }
}

/// The theme's `theme-color` metas put back to upstream's (the theme changes nothing else in the
/// head but its links).
fn upstream_chrome(page: &str) -> String {
    let themed = concat!(
        r##"<meta name="theme-color" content="#f4f0e8" media="(prefers-color-scheme: light)">"##,
        "\n    ",
        r##"<meta name="theme-color" content="#181e1b" media="(prefers-color-scheme: dark)">"##
    );
    let upstream = concat!(
        r##"<meta name="theme-color" content="#ffffff" media="(prefers-color-scheme: light)">"##,
        "\n    ",
        r##"<meta name="theme-color" content="#000000" media="(prefers-color-scheme: dark)">"##
    );
    assert_eq!(page.matches(themed).count(), 1, "the theme's chrome colours: {page}");
    page.replace(themed, upstream)
}

/// Where each of `needles` is in `page`'s head, asserting each is there exactly once.
fn positions(page: &str, needles: &[&str]) -> Vec<usize> {
    let head_end = page.find("</head>").expect("a head");
    needles
        .iter()
        .map(|needle| {
            assert_eq!(page.matches(needle).count(), 1, "{needle} once: {page}");
            let at = page.find(needle).unwrap();
            assert!(at < head_end, "{needle} in the head");
            at
        })
        .collect()
}

#[test]
fn the_head_links_the_fork_stylesheets_then_the_theme_after_custom_styles() {
    // Not installed (the views' tests and goldens): upstream's bytes, no Hermes stylesheet at all.
    let upstream = room_page();
    let upstream_voice = voice_page();
    assert!(!upstream.contains("hermes/") && !upstream_voice.contains("hermes/"));

    // The app at boot: the tokens, hermes.css, then the theme, after Custom styles; nothing else
    // changes.
    install_page_assets(Some(PageAssets { theme: true, brand: false }));
    let themed = room_page();
    let themed_voice = voice_page();
    let added = format!("{}{}{}{}", link("hermes/tokens.css"), link("hermes/hermes.css"), link("hermes/theme.css"), script());
    let at = positions(&themed, &[UPSTREAM_STYLESHEET, CUSTOM_STYLES, &added]);
    assert!(at[0] < at[1] && at[1] < at[2], "upstream, Custom styles, then ours");
    assert_eq!(upstream_chrome(&themed.replace(&added, "")), upstream, "nothing else changes but the chrome's colours");
    assert_eq!(upstream_chrome(&themed_voice.replace(&added, "")), upstream_voice, "the voice page links nothing else");

    // The switch's script runs before the first paint: in the head, before anything in the body.
    assert!(THEME_SCRIPT.contains("data-theme") && THEME_SCRIPT.contains("localStorage"));
    assert!(!THEME_SCRIPT.contains("</script"));

    // CAMPFIRE_THEME=off: the fork's stylesheets only (no theme, no switch script).
    install_page_assets(Some(PageAssets { theme: false, brand: false }));
    let unthemed = room_page();
    assert!(!unthemed.contains("hermes/theme.css") && !unthemed.contains("<script>(function"));
    assert_eq!(unthemed.replace(&format!("{}{}", link("hermes/tokens.css"), link("hermes/hermes.css")), ""), upstream);

    // The workspace on: its stylesheet between the fork's and the theme.
    install_page_assets(Some(PageAssets { theme: true, brand: false }));
    install_workspace_hooks(Some(Arc::new(Workspace)));
    let with_workspace = room_page();
    let added = format!(
        "{}{}{}{}{}",
        link("hermes/tokens.css"),
        link("hermes/hermes.css"),
        link("hermes/workspace.css"),
        link("hermes/theme.css"),
        script()
    );
    positions(&with_workspace, &[CUSTOM_STYLES, &added]);
    assert_eq!(upstream_chrome(&with_workspace.replace(&added, "")), upstream);

    // The workspace's hooks without the page assets (not how the app boots): the tokens still come
    // first, since workspace.css reads them.
    install_page_assets(None);
    let added = format!("{}{}", link("hermes/tokens.css"), link("hermes/workspace.css"));
    assert_eq!(room_page().replace(&added, ""), upstream);

    // Removed: back to upstream's bytes.
    install_workspace_hooks(None);
    install_page_assets(None);
    assert_eq!(room_page(), upstream);
    assert_eq!(voice_page(), upstream_voice);

    // The theme's Light / Dark / System switch (the profile page's seam): only with the theme on.
    assert_eq!(render(color_scheme_switch), "");
    install_page_assets(Some(PageAssets { theme: false, brand: false }));
    assert_eq!(render(color_scheme_switch), "");
    install_page_assets(Some(PageAssets { theme: true, brand: false }));
    let switch = render(color_scheme_switch);
    for value in ["light", "dark", "system"] {
        assert_eq!(switch.matches(&format!(r#"<input type="radio" name="hermes-theme" value="{value}" />"#)).count(), 1, "{switch}");
    }
    assert!(switch.starts_with(r#"<fieldset class="hermes-theme-switch">"#) && switch.contains("<legend"), "{switch}");
    install_page_assets(None);
    let profile = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/templates-hermes/users/profiles/show.html")).unwrap();
    assert!(profile.contains("{{ crate::hermes::color_scheme_switch(ctx)|safe }}"));
}
