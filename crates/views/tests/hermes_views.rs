//! Hermes fork: the live voice report's page, the composer's voice buttons and inline audio.

mod messages_support;

use askama::Template;
use campfire_views::hermes::{VoiceShow, VoiceView};
use campfire_views::messages::presentation::attachment_presentation;
use campfire_views::messages::{AttachmentPreview, AttachmentView};
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
    for absent in ["voice-note", "hermes", "microphone", "headset", "/voice"] {
        assert!(!html.contains(absent), "{absent}");
    }
}

#[test]
fn the_composer_records_voice_notes_next_to_the_attachment_button() {
    let (_, html) = show_room(|show| show.voice_note = true);
    let form = composer(&html);
    let note = form.find(r#"<div class="hermes-voice-note" data-controller="voice-note" data-voice-note-max-seconds-value="300" data-voice-note-warn-seconds-value="30" hidden>"#).expect(form);
    assert!(form.find("Attach a file").unwrap() < note, "after the attachment button");
    assert!(note < form.find(r#"name="send""#).unwrap(), "before the send button");
    for fragment in [
        r#"data-voice-note-target="toggle" data-action="voice-note#toggle" title="Enregistrer un message vocal">"#,
        r#"<img class="colorize--black hermes-voice-note__icon-record" aria-hidden="true" src="/assets/microphone.svg""#,
        r#"<img class="hermes-voice-note__icon-send" aria-hidden="true" src="/assets/arrow-up.svg""#,
        r#"data-voice-note-target="cancel" data-action="voice-note#cancel" title="Annuler l’enregistrement" hidden"#,
        r#"role="timer" aria-label="Durée de l’enregistrement"><span class="hermes-voice-note__dot" aria-hidden="true"></span><span data-voice-note-target="timer">0:00</span>"#,
        r#"data-voice-note-target="level" aria-hidden="true""#,
        r#"data-voice-note-target="warning" aria-live="polite""#,
        r#"data-voice-note-target="error" role="alert" hidden></p>"#,
    ] {
        assert!(form.contains(fragment), "{fragment}");
    }
    assert!(!form.contains("aria-pressed"), "the label changes instead");
    assert!(!form.contains("<style>"), "the styles are in hermes/hermes.css");
    assert!(!form.contains("<link"), "hermes/hermes.css is linked from the head (tests/hermes_head.rs)");
    assert!(!html.contains("headset"), "no live button without Gemini");
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
        r#"<a class="btn btn--borderless txt-small flex-item-no-shrink hermes-live-link" href="/rooms/{}/voice" data-turbo-frame="_top" title="Ticket vocal (demande, panne, incident)">"#,
        show.room.id
    );
    let at = form.find(&link).expect(form);
    assert!(form.find("hermes-voice-note__toggle").unwrap() < at, "after the record button");
    assert!(form[at..].contains(r#"src="/assets/headset.svg""#));
    assert!(form[at..].contains(r#"<span class="hermes-live-link__text" aria-hidden="true">Rapport</span>"#), "visible on touch screens");
    assert!(form[at..].contains(r#"<span class="for-screen-reader">Ticket vocal (demande, panne, incident)</span>"#));
    assert!(!form.contains("Gemini"));
    assert!(!html[..html.find("</nav>").unwrap()].contains("/voice"), "the nav's mic button is gone");
    assert!(!html.contains("hermes/hermes.css"), "linked from the head, by the seam (tests/hermes_head.rs)");

    // Gemini on, voice notes off: the live link alone.
    let (_, html) = show_room(|show| show.voice_path = Some("/rooms/1/voice".into()));
    assert!(composer(&html).contains(r#"href="/rooms/1/voice" data-turbo-frame="_top""#));
    assert!(!html.contains(r#"data-controller="voice-note""#));
}

fn audio_attachment(filename: &str) -> AttachmentView {
    serde_json::from_value(serde_json::json!({
        "filename": filename,
        "blob_path": "/rails/active_storage/blobs/redirect/abc--def/note.webm",
        "download_path": "/rails/active_storage/blobs/redirect/abc--def/note.webm?disposition=attachment",
        "preview": {"type": "audio"},
        "width": null,
        "height": null
    }))
    .unwrap()
}

#[test]
fn voice_notes_play_inline_with_a_compact_line() {
    let attachment = audio_attachment("note-vocale-20260929-070503.webm");
    assert_eq!(attachment.preview, AttachmentPreview::Audio);
    let html = render(|ctx| attachment_presentation(ctx, &attachment));
    assert!(
        html.contains(concat!(
            r#"<div class="hermes-audio" data-controller="voice-player">"#,
            r#"<audio src="/rails/active_storage/blobs/redirect/abc--def/note.webm" controls="controls" preload="metadata" aria-label="Message vocal" data-voice-player-target="audio"></audio>"#,
            r#"<div class="hermes-audio__meta txt-small">"#
        )),
        "{html}"
    );
    assert!(html.contains(r#"<span>Message vocal<span data-voice-player-target="duration"></span></span>"#), "{html}");
    assert!(html.contains(r#"href="/rails/active_storage/blobs/redirect/abc--def/note.webm?disposition=attachment""#), "download");
    assert!(html.contains("Télécharger le message vocal"));
    assert!(!html.contains("note-vocale-20260929"), "no file name row: {html}");
    assert!(!html.contains(" style=\"inline-size"), "sized by hermes/hermes.css");

    let file = AttachmentView { preview: AttachmentPreview::File, ..attachment };
    assert!(!render(|ctx| attachment_presentation(ctx, &file)).contains("<audio"), "other files are unchanged");
}

#[test]
fn other_audio_files_keep_their_file_link() {
    let attachment = audio_attachment("interview <1>.mp3");
    let html = render(|ctx| attachment_presentation(ctx, &attachment));
    assert!(html.contains(r#"preload="metadata" aria-label="Écouter interview &lt;1&gt;.mp3""#), "{html}");
    assert!(html.contains(r#"<span>interview &lt;1&gt;.mp3</span>"#), "the reference's file link: {html}");
    assert!(!html.contains("Message vocal"));
    assert!(html.ends_with("</div></div>"));
}

#[test]
fn the_voice_page_carries_the_controller_values_escaped() {
    let voice = VoiceView {
        room_id: 7,
        room_name: "Atelier \"B\" <1>".into(),
        room_url: "/rooms/7".into(),
        token_url: "/rooms/7/voice/token".into(),
        report_url: "/rooms/7/voice/report".into(),
        ask_url: None,
        worklet_url: "/assets/voice/pcm-worklet-abc.js".into(),
    };
    let html = render(|ctx| VoiceShow { ctx, voice: &voice }.render().unwrap());
    assert!(!html.contains("ask-url"), "no ask_hermes without HERMES_ASK_URL");
    let with_ask = VoiceView { ask_url: Some("/rooms/7/voice/ask".into()), ..voice.clone() };
    let asking = render(|ctx| VoiceShow { ctx, voice: &with_ask }.render().unwrap());
    assert!(
        asking.contains(r#"data-voice-report-url-value="/rooms/7/voice/report" data-voice-ask-url-value="/rooms/7/voice/ask" "#),
        "{asking}"
    );
    assert!(html.contains("<title>Rapport vocal · Atelier &quot;B&quot; &lt;1&gt;</title>"), "{html}");
    assert!(html.contains(r#"<h1 class="room__contents txt-medium overflow-ellipsis">Rapport vocal</h1>"#), "short title");
    assert!(html.contains("dans «&nbsp;Atelier &quot;B&quot; &lt;1&gt;&nbsp;» après votre accord"), "the room in the intro");
    assert!(!html.contains("hermes/hermes.css"), "linked from the head, by the seam (tests/hermes_head.rs)");
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
    for target in [
        "label", "control", "status", "timer", "hint", "notice", "noticeBody", "confirm", "cancel", "resume", "restart", "resultText",
        "messageLink", "announcer",
    ] {
        assert!(html.contains(&format!(r#"data-voice-target="{target}""#)), "{target}");
    }
    assert!(html.contains(r#"data-voice-target="transcript" aria-live="off""#), "turns are announced once, not streamed");
    assert!(html.contains(r#"data-voice-target="announcer" role="status""#));
    assert!(html.contains(r#"data-voice-target="result" hidden"#));
    assert!(html.contains(r#"data-voice-target="messageLink">Voir le message</a>"#));
    assert!(!html.contains("aria-pressed"));
    assert!(!html.contains("<style>"));
    assert!(html.contains(r#"<a class="btn" href="/rooms/7">"#), "back to the room");
}
