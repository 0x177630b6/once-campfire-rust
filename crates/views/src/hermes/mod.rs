//! Hermes fork: pages that aren't in the reference app.
//!
//! - [`VoiceShow`]: the live voice incident report (`GET /rooms/:room_id/voice`), a page around
//!   the `voice` Stimulus controller (`crates/assets/overrides/controllers/voice_controller.js`).
//!   See docs/hermes-gemini-live.md.
//! - [`audio_preview`]: an audio attachment (a voice note from the composer) as an inline player.

use askama::Template;
use serde::Deserialize;

use crate::helpers as h;
use crate::helpers::escape;
use crate::messages::AttachmentView;
use crate::layouts::Page;
use crate::ViewContext;

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
        Some(format!("Compte rendu vocal · {}", self.voice.room_name))
    }
}

/// An `audio/*` attachment: a player (nothing is fetched until play) above the file link the
/// reference renders for any other file (name, download, share).
pub(crate) fn audio_preview(ctx: &ViewContext, attachment: &AttachmentView) -> String {
    format!(
        concat!(
            r#"<div class="flex flex-column gap-half max-inline-size">"#,
            r#"<audio src="{src}" controls="controls" preload="none" style="inline-size: min(100%, 22rem); max-inline-size: 100%;" aria-label="{label}"></audio>"#,
            "{link}",
            r#"</div>"#
        ),
        src = escape(&attachment.blob_path),
        label = escape(&format!("Écouter {}", attachment.filename)),
        link = crate::messages::presentation::file_link(ctx, attachment),
    )
}
