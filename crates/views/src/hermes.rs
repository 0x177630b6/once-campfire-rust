//! Hermes fork: pages that aren't in the reference app.
//!
//! - [`VoiceShow`]: the live voice incident report (`GET /rooms/:room_id/voice`), a page around
//!   the `voice` Stimulus controller (`crates/assets/overrides/controllers/voice_controller.js`).
//!   See docs/hermes-gemini-live.md.
//! - [`audio_preview`]: an audio attachment (a voice note from the composer) as an inline player.
//!
//! Their styles are `hermes/hermes.css` (crates/assets/overrides), which the Hermes templates link.

use askama::Template;
use serde::Deserialize;

use crate::ViewContext;
use crate::helpers as h;
use crate::helpers::escape;
use crate::layouts::Page;
use crate::messages::AttachmentView;

/// What the voice page needs. Every URL is a same-origin path.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct VoiceView {
    pub room_id: i64,
    /// `room_display_name(room)` for `Current.user`.
    pub room_name: String,
    /// The room page, to go back to.
    pub room_url: String,
    /// `POST`: a single-use Gemini Live token.
    pub token_url: String,
    /// `POST`: the confirmed report, published in the room.
    pub report_url: String,
    /// `POST`: an `ask_hermes` question, answered by the Hermes agent; only with `HERMES_ASK_URL`.
    pub ask_url: Option<String>,
    /// The digested `voice/pcm-worklet.js`, for `audioWorklet.addModule`.
    pub worklet_url: String,
}

/// `hermes/voice.html`
#[derive(Template)]
#[template(path = "hermes/voice.html", blocks = ["head", "content"])]
pub struct VoiceShow<'a> {
    pub ctx: &'a ViewContext<'a>,
    pub voice: &'a VoiceView,
}

impl Page for VoiceShow<'_> {
    fn page_title(&self) -> Option<String> {
        Some(format!("Rapport vocal · {}", self.voice.room_name))
    }
}

/// Voice notes recorded from the composer are named `note-vocale-YYYYMMDD-HHMMSS.<ext>`
/// (`voice_note_controller.js`).
const VOICE_NOTE_PREFIX: &str = "note-vocale-";

/// An `audio/*` attachment: a player that fetches only the metadata up front (so it can show the
/// duration), full width of the bubble. A voice note gets a compact « Message vocal · 0:07 » line
/// and a download button (the `voice-player` controller fills in the duration once known); any
/// other audio file keeps the file link the reference renders (name, download, share).
pub(crate) fn audio_preview(ctx: &ViewContext, attachment: &AttachmentView) -> String {
    let voice_note = attachment.filename.starts_with(VOICE_NOTE_PREFIX);
    let label = if voice_note { "Message vocal".to_string() } else { format!("Écouter {}", attachment.filename) };
    let meta = if voice_note {
        format!(
            concat!(
                r#"<div class="hermes-audio__meta txt-small">"#,
                r#"<span class="hermes-audio__label">"#,
                r#"<img class="colorize--black" aria-hidden="true" src="{icon}" width="16" height="16" />"#,
                r#"<span>Message vocal<span data-voice-player-target="duration"></span></span></span>"#,
                r#"<a class="btn message__action-btn hide-in-ios-pwa" style="--width: auto;" href="{download}" title="Télécharger">"#,
                r#"<img aria-hidden="true" src="{download_icon}" width="20" height="20" />"#,
                r#"<span class="for-screen-reader">Télécharger le message vocal</span></a>"#,
                r#"</div>"#
            ),
            icon = escape(&ctx.asset("microphone.svg")),
            download = escape(&attachment.download_path),
            download_icon = escape(&ctx.asset("download.svg")),
        )
    } else {
        crate::messages::presentation::file_link(ctx, attachment)
    };
    format!(
        concat!(
            r#"<div class="hermes-audio" data-controller="voice-player">"#,
            r#"<audio src="{src}" controls="controls" preload="metadata" aria-label="{label}" data-voice-player-target="audio"></audio>"#,
            "{meta}",
            r#"</div>"#
        ),
        src = escape(&attachment.blob_path),
        label = escape(&label),
        meta = meta,
    )
}
