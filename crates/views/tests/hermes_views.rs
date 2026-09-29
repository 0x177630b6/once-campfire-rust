//! Hermes fork: the live voice report's page, and the composer's voice buttons.

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

fn show_room(configure: impl FnOnce(&mut ShowView)) -> (ShowView, String) {
    let g = golden("rooms_show_closed");
    let mut show: ShowView = g.input();
    configure(&mut show);
    let html = render(|ctx| rooms::Show { ctx, show: &show }.render().unwrap());
    (show, html)
}

/// The composer's `<form id="composer">…</form>`.
fn composer(html: &str) -> &str {
    let start = html.find(r#"<form id="composer""#).expect("composer form");
    let end = start + html[start..].find("</form>").unwrap();
    &html[start..end]
}

#[test]
fn the_voice_buttons_are_off_by_default() {
    let (show, html) = show_room(|_| {});
    assert!(!show.voice_note && show.voice_path.is_none(), "the goldens' input has neither");
    for absent in ["voice-note", "hermes-", "microphone", "waveform", "/voice"] {
        assert!(!html.contains(absent), "{absent}");
    }
}

#[test]
fn the_composer_records_voice_notes_next_to_the_attachment_button() {
    let (_, html) = show_room(|show| show.voice_note = true);
    let form = composer(&html);
    let note = form.find(r#"<div class="hermes-voice-note" data-controller="voice-note" data-voice-note-max-seconds-value="300" hidden>"#).expect(form);
    assert!(form.find("Attach a file").unwrap() < note, "after the attachment button");
    assert!(note < form.find(r#"name="send""#).unwrap(), "before the send button");
    for fragment in [
        r#"data-voice-note-target="toggle" data-action="voice-note#toggle" aria-pressed="false" title="Enregistrer un message vocal""#,
        r#"src="/assets/microphone.svg""#,
        r#"data-voice-note-target="cancel" data-action="voice-note#cancel" title="Annuler l’enregistrement" hidden"#,
        r#"data-voice-note-target="timer" role="timer" hidden>● 0:00</span>"#,
        r#"data-voice-note-target="error" role="alert" hidden></p>"#,
        "@media (pointer: coarse) { .hermes-composer-btn { --btn-size: 2.75rem; } }",
    ] {
        assert!(form.contains(fragment), "{fragment}");
    }
    assert!(!html.contains("waveform"), "no live button without Gemini");
    assert!(!html[..html.find("</nav>").unwrap()].contains("microphone"), "nothing in the nav");
}

#[test]
fn the_composer_links_to_the_live_report_only_when_gemini_is_on() {
    let (show, html) = show_room(|show| {
        show.voice_note = true;
        show.voice_path = Some(format!("/rooms/{}/voice", show.room.id));
    });
    let form = composer(&html);
    let link = format!(
        r#"<a class="btn btn--borderless txt-small flex-item-no-shrink hermes-composer-btn" href="/rooms/{}/voice" data-turbo-frame="_top" title="Compte rendu vocal en direct (Gemini)">"#,
        show.room.id
    );
    let at = form.find(&link).expect(form);
    assert!(form.find("hermes-voice-note__toggle").unwrap() < at, "after the record button");
    assert!(form[at..].contains(r#"src="/assets/waveform.svg""#));
    assert!(!html[..html.find("</nav>").unwrap()].contains("/voice"), "the nav's mic button is gone");
    assert_eq!(form.matches("<style>").count(), 1);

    // Gemini on, voice notes off: the live link alone.
    let (_, html) = show_room(|show| show.voice_path = Some("/rooms/1/voice".into()));
    assert!(composer(&html).contains(r#"href="/rooms/1/voice" data-turbo-frame="_top""#));
    assert!(!html.contains(r#"data-controller="voice-note""#));
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
