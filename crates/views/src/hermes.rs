//! Hermes fork: pages that aren't in the reference app.
//!
//! - [`VoiceShow`]: the live voice incident report (`GET /rooms/:room_id/voice`), a page around
//!   the `voice` Stimulus controller (`crates/assets/overrides/controllers/voice_controller.js`).
//!   See docs/hermes-gemini-live.md.
//! - [`audio_preview`]: an audio attachment (a voice note from the composer) as an inline player.
//! - [`WorkspaceHooks`]: where the Duty Manager Workspace (`campfire_workspace`,
//!   docs/hermes-workspace.md) plugs into rendering; nothing is installed while it's off.
//! - [`head_tags`]: the fork's stylesheets and the product theme, linked from the layout's head
//!   (docs/hermes-theme.md); nothing until the app installs [`PageAssets`] at boot.
//!
//! Their styles are `hermes/hermes.css` (crates/assets/overrides), linked by [`head_tags`].

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

// --- Duty Manager Workspace hooks -----------------------------------------------------------------

/// Hermes fork: what the Duty Manager Workspace (docs/hermes-workspace.md, `campfire_workspace`)
/// adds to rendering, through three seams in upstream files: the head and the end of
/// `layouts/application.html` ([`head_tags`], [`workspace_overlay`]) and `message_presentation`'s
/// text case ([`workspace_message_html`]). Nothing is installed while the feature is off, and all
/// three render exactly what upstream does.
pub trait WorkspaceHooks: Send + Sync {
    /// The digested URLs of the stylesheets [`head_tags`] links for this page (the workspace's own,
    /// on the pages that get the overlay), after the fork's and before the theme.
    fn stylesheets(&self, ctx: &ViewContext) -> Vec<String>;
    /// Appended to the application layout's body (scripts, tab bar).
    fn layout_overlay(&self, ctx: &ViewContext) -> String;
    /// A text message's body as shown (card chips, draft buttons), or `None` to keep it. It ends up
    /// in the shared message fragment cache, so it must not depend on who's looking.
    fn message_html(&self, message: &crate::messages::MessageView, html: &str) -> Option<String>;
}

static WORKSPACE_HOOKS: std::sync::RwLock<Option<std::sync::Arc<dyn WorkspaceHooks>>> = std::sync::RwLock::new(None);

/// Installs (or, with `None`, removes) the workspace's hooks. The app does it once at boot.
pub fn install_workspace_hooks(hooks: Option<std::sync::Arc<dyn WorkspaceHooks>>) {
    *WORKSPACE_HOOKS.write().unwrap_or_else(|e| e.into_inner()) = hooks;
}

fn workspace_hooks() -> Option<std::sync::Arc<dyn WorkspaceHooks>> {
    WORKSPACE_HOOKS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The layout seam: empty unless the workspace is on.
pub fn workspace_overlay(ctx: &ViewContext) -> String {
    workspace_hooks().map(|hooks| hooks.layout_overlay(ctx)).unwrap_or_default()
}

/// The message seam: the body as stored unless the workspace decorates it.
pub(crate) fn workspace_message_html(message: &crate::messages::MessageView, html: &str) -> String {
    workspace_hooks().and_then(|hooks| hooks.message_html(message, html)).unwrap_or_else(|| html.to_string())
}

// --- Page assets: the fork's stylesheets and the product theme, in <head> ------------------------

/// The fork's semantic tokens (`--ui-*`), which the fork's stylesheets and the theme build on.
pub const TOKENS_STYLESHEET: &str = "hermes/tokens.css";
/// The voice features' styles (composer buttons, recording bar, audio player, live voice page).
pub const HERMES_STYLESHEET: &str = "hermes/hermes.css";
/// The product theme: loaded last, so it wins over upstream, Custom styles and the fork's CSS.
pub const THEME_STYLESHEET: &str = "hermes/theme.css";

/// Hermes fork: what [`head_tags`] links on every page, installed by the app at boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageAssets {
    /// The product theme (`CAMPFIRE_THEME`, on by default).
    pub theme: bool,
}

static PAGE_ASSETS: std::sync::RwLock<Option<PageAssets>> = std::sync::RwLock::new(None);

/// Installs (or, with `None`, removes) the page assets. The app does it once at boot.
pub fn install_page_assets(assets: Option<PageAssets>) {
    *PAGE_ASSETS.write().unwrap_or_else(|e| e.into_inner()) = assets;
}

fn page_assets() -> Option<PageAssets> {
    *PAGE_ASSETS.read().unwrap_or_else(|e| e.into_inner())
}

/// The head seam, right after Custom styles (`custom_styles_tag`): the tokens, `hermes.css`,
/// the workspace's stylesheet while it's on, then the theme, in that order, so that the theme wins
/// every tie. Each `<link>` has `data-turbo-track="reload"`, like upstream's stylesheets: a page
/// still open when a deploy changes one reloads at its next visit. The list is the same on every
/// page of a signed-in person (a different list would make Turbo reload on each visit). Empty while
/// nothing is installed, so the views' tests and goldens get upstream's bytes.
pub fn head_tags(ctx: &ViewContext) -> String {
    let assets = page_assets();
    let workspace = workspace_hooks().map(|hooks| hooks.stylesheets(ctx)).unwrap_or_default();
    if assets.is_none() && workspace.is_empty() {
        return String::new();
    }
    // The tokens go with any of the fork's stylesheets: they all read them.
    let mut hrefs = vec![ctx.asset(TOKENS_STYLESHEET)];
    if assets.is_some() {
        hrefs.push(ctx.asset(HERMES_STYLESHEET));
    }
    hrefs.extend(workspace);
    if assets.is_some_and(|assets| assets.theme) {
        hrefs.push(ctx.asset(THEME_STYLESHEET));
    }
    hrefs.iter().map(|href| format!("\n    <link rel=\"stylesheet\" href=\"{}\" data-turbo-track=\"reload\" />", escape(href))).collect()
}
