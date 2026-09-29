//! Hermes fork: the live voice report's page and the room nav's mic button.

mod messages_support;

use askama::Template;
use campfire_views::hermes::{VoiceShow, VoiceView};
use campfire_views::rooms::{self, ShowView};
use campfire_views::{AccountSummary, Platform, ViewContext};
use messages_support::golden;

/// A signed-out context whose assets all resolve to `/assets/<logical path>` (the goldens only
/// know the reference's assets, not the fork's microphone icon).
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
        stylesheet_tags: "",
        custom_styles: None,
        cable_url: "/cable".into(),
        base_url: "http://campfire.test".into(),
        request_url: "http://campfire.test/".into(),
        referrer: None,
        last_room_visited_id: None,
        app_version: "0".into(),
    };
    f(&ctx)
}

#[test]
fn the_room_nav_links_to_the_voice_page_only_when_the_feature_is_on() {
    let g = golden("rooms_show_closed");
    let mut show: ShowView = g.input();
    let off = render(|ctx| rooms::Show { ctx, show: &show }.render().unwrap());
    assert!(!off.contains("microphone"));

    show.voice_path = Some(format!("/rooms/{}/voice", show.room.id));
    let on = render(|ctx| rooms::Show { ctx, show: &show }.render().unwrap());
    let link = format!(r#"<a class="btn" href="/rooms/{}/voice" title="Compte rendu vocal">"#, show.room.id);
    assert!(on.contains(&link), "{on}");
    assert!(on.contains(r#"src="/assets/microphone.svg""#));
    let nav_end = on.find("</nav>").unwrap();
    assert!(on.find(&link).unwrap() < nav_end, "in the nav");
}

#[test]
fn the_voice_page_carries_the_controller_values_escaped() {
    let voice = VoiceView {
        room_id: 7,
        room_name: "Atelier \"B\" <1>".into(),
        room_url: "/rooms/7".into(),
        token_url: "/rooms/7/voice/token".into(),
        report_url: "/rooms/7/voice/report".into(),
        worklet_url: "/assets/voice/pcm-worklet-abc.js".into(),
    };
    let html = render(|ctx| VoiceShow { ctx, voice: &voice }.render().unwrap());
    assert!(html.contains("<title>Compte rendu vocal · Atelier &quot;B&quot; &lt;1&gt;</title>"), "{html}");
    for (name, value) in [
        ("token-url", "/rooms/7/voice/token"),
        ("report-url", "/rooms/7/voice/report"),
        ("worklet-url", "/assets/voice/pcm-worklet-abc.js"),
        ("room-url", "/rooms/7"),
        ("room-name", "Atelier &quot;B&quot; &lt;1&gt;"),
    ] {
        assert!(html.contains(&format!(r#"data-voice-{name}-value="{value}""#)), "{name}");
    }
    assert!(html.contains(r#"data-controller="voice""#));
    assert!(html.contains(r#"data-voice-target="toggle" data-action="voice#toggle""#));
    assert!(html.contains(r#"data-voice-target="result" role="status" hidden"#));
    assert!(html.contains(r#"<a class="btn" href="/rooms/7">"#), "back to the room");
}
