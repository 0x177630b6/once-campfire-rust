//! Hermes fork: live voice incident reports (docs/hermes-gemini-live.md). Not in the reference.
//!
//! - `GET /rooms/:room_id/voice`: the page, around the `voice` Stimulus controller.
//! - `POST /rooms/:room_id/voice/token`: a single-use Gemini Live token, the session setup locked
//!   in it (see `integrations::gemini_live`).
//! - `POST /rooms/:room_id/voice/report`: the confirmed report (the `submit_incident` arguments
//!   plus the transcript, as JSON), posted in the room as the current user with an @mention of the
//!   Hermes bot, through the same path as `MessagesController#create`.
//! - `POST /rooms/:room_id/voice/ask`: the interviewer's `ask_hermes` question (`{"question"}`),
//!   forwarded to the Hermes bridge (`HERMES_ASK_URL`); answers `{"answer"}`. 404 unless
//!   `HERMES_ASK_URL` is set too.
//!
//! All four 404 unless `GEMINI_API_KEY` is set, and run `ApplicationController`'s chain
//! (session only, no bots, `Sec-Fetch-Site` forgery protection for the POSTs) and `RoomScoped`'s
//! membership check.

use campfire_db::{Room, User};
use campfire_kit::{Ctx, Error, Param, ParamMap, Result, StatusCode};
use serde_json::json;

use crate::app::AppCtx;
use crate::concerns::{self, Before, before_actions, require_current_user};
use crate::controllers::messages::{self, MessageParams};
use crate::controllers::presenters::Presenter;
use crate::controllers::presenters::accounts::attachable_sgid;
use crate::controllers::presenters::page::{self, db_error};
use crate::integrations::gemini_live::{self, GeminiLive, Interview, MintError};
use crate::integrations::hermes_ask::{self, AskError, Question};

/// The page's path, for the room nav's mic button.
pub fn voice_path(room_id: i64) -> String {
    format!("/rooms/{room_id}/voice")
}

pub fn voice_token_path(room_id: i64) -> String {
    format!("/rooms/{room_id}/voice/token")
}

pub fn voice_report_path(room_id: i64) -> String {
    format!("/rooms/{room_id}/voice/report")
}

pub fn voice_ask_path(room_id: i64) -> String {
    format!("/rooms/{room_id}/voice/ask")
}

/// The AudioWorklet the page's controller loads (`crates/assets/overrides/voice/pcm-worklet.js`).
pub const WORKLET_ASSET: &str = "voice/pcm-worklet.js";

// --- Actions ------------------------------------------------------------------------------------

pub async fn show(c: &mut Ctx) -> Result {
    feature(c)?;
    before_actions(c, Before::default()).await?;
    let (_, room) = concerns::set_room(c).await?;
    let room_name = room_display_name(c, &room).await?;
    let worklet_url = campfire_assets::try_asset_path(WORKLET_ASSET).map_err(Error::internal)?;
    let ask_url = feature(c)?.ask_enabled().then(|| voice_ask_path(room.id));
    let view = campfire_views::hermes::VoiceView {
        room_id: room.id,
        room_name,
        room_url: campfire_routes::room(room.id),
        token_url: voice_token_path(room.id),
        report_url: voice_report_path(room.id),
        ask_url,
        worklet_url,
    };
    page::framed_page!(c, StatusCode::OK, |ctx| campfire_views::hermes::VoiceShow { ctx, voice: &view }).await
}

pub async fn token(c: &mut Ctx) -> Result {
    feature(c)?;
    before_actions(c, Before::default()).await?;
    let (_, room) = concerns::set_room(c).await?;
    let user = require_current_user(c)?.clone();
    let now = c.now();
    let live = feature(c)?;
    if !live.allow(user.id, now) {
        return json_error(c, StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Trop de sessions vocales demandées ; réessayez plus tard.");
    }

    let room_name = room_display_name(c, &room).await?;
    let live = feature(c)?;
    let config = &live.config;
    let request = Interview {
        model: &config.model,
        room_name: &room_name,
        user_name: &user.name,
        extra_instructions: config.extra_instructions.as_deref(),
        ask_hermes: live.ask_enabled(),
        now,
    }
    .token_request();
    let model = config.model.clone();
    match live.minter().mint(request).await {
        Ok(token) => c.json(
            StatusCode::OK,
            &json!({
                "token": token,
                "ws_url": gemini_live::WS_URL,
                "model": model,
                "expires_at": gemini_live::rfc3339(now + gemini_live::TOKEN_LIFETIME),
            }),
        ),
        Err(error) => {
            tracing::warn!(room_id = room.id, user_id = user.id, %error, "could not mint a Gemini Live token");
            let code = if matches!(error, MintError::Timeout) { "upstream_timeout" } else { "upstream_error" };
            json_error(c, StatusCode::BAD_GATEWAY, code, "Le service vocal est indisponible ; réessayez dans un instant.")
        }
    }
}

pub async fn report(c: &mut Ctx) -> Result {
    feature(c)?;
    before_actions(c, Before::default()).await?;
    let (_, room) = concerns::set_room(c).await?;
    let Some(report) = IncidentReport::from_params(&c.request_params) else {
        return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "invalid_report", "Le titre et le résumé sont obligatoires.");
    };
    let Some(bot) = voice_bot(c, &room).await? else {
        return json_error(
            c,
            StatusCode::UNPROCESSABLE_ENTITY,
            "bot_not_in_room",
            "Le bot chargé des comptes rendus n'est pas membre de ce salon.",
        );
    };

    let body = report.to_html(&attachable_sgid(&c.app().secrets, bot.id));
    let attributes = MessageParams { body: Some(body), attachment: None, client_message_id: None };
    // MessagesController#create's path: create, broadcast, then the bots' webhooks (the mention
    // above is what makes `deliver_webhooks_to_bots` pick the bot in a shared room).
    let message = messages::create_message(c, &room, attributes).await?;
    messages::broadcast_create(c, &room, &message).await?;
    messages::deliver_webhooks_to_bots(c, &room, &message).await?;

    let message_url = c.url_for(&campfire_routes::room_at_message(room.id, message.id));
    c.json(StatusCode::CREATED, &json!({ "message_id": message.id, "message_url": message_url }))
}

pub async fn ask(c: &mut Ctx) -> Result {
    ask_feature(c)?;
    before_actions(c, Before::default()).await?;
    let (_, room) = concerns::set_room(c).await?;
    let user = require_current_user(c)?.clone();
    let question = c.request_params.get("question").and_then(Param::as_str).map(|q| q.split_whitespace().collect::<Vec<_>>().join(" "));
    let Some(question) = question.filter(|q| !q.is_empty()) else {
        return json_error(c, StatusCode::UNPROCESSABLE_ENTITY, "invalid_question", "La question est vide.");
    };
    let question = hermes_ask::truncate_chars(&question, hermes_ask::MAX_QUESTION_CHARS);
    if !ask_feature(c)?.allow_ask(user.id, c.now()) {
        return json_error(c, StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Trop de questions posées à Hermes ; réessayez plus tard.");
    }

    let room_name = room_display_name(c, &room).await?;
    let asker = ask_feature(c)?.asker().ok_or(Error::NotFound)?;
    let length = question.chars().count();
    let started = std::time::Instant::now();
    let result = asker.ask(Question { room_id: room.id, user_name: user.name.clone(), room_name, question }).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(answer) => {
            tracing::info!(
                room_id = room.id,
                user_id = user.id,
                question_chars = length,
                answer_chars = answer.chars().count(),
                elapsed_ms,
                "Hermes answered a live voice question"
            );
            c.json(StatusCode::OK, &json!({ "answer": answer }))
        }
        Err(AskError::Timeout) => {
            tracing::warn!(room_id = room.id, user_id = user.id, elapsed_ms, "Hermes did not answer a live voice question in time");
            json_error(c, StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", "Hermes n’a pas répondu à temps.")
        }
        Err(error) => {
            tracing::warn!(room_id = room.id, user_id = user.id, elapsed_ms, %error, "could not ask Hermes a live voice question");
            json_error(c, StatusCode::BAD_GATEWAY, "upstream_error", "Hermes est injoignable pour le moment.")
        }
    }
}

// --- Helpers ------------------------------------------------------------------------------------

/// The feature's state, or 404 while it's off.
fn feature(c: &Ctx) -> Result<&GeminiLive> {
    c.app().gemini_live.as_ref().ok_or(Error::NotFound)
}

/// The feature's state when `ask_hermes` is on too (`HERMES_ASK_URL`), or 404.
fn ask_feature(c: &Ctx) -> Result<&GeminiLive> {
    feature(c).ok().filter(|live| live.ask_enabled()).ok_or(Error::NotFound)
}

fn json_error(c: &mut Ctx, status: StatusCode, code: &str, message: &str) -> Result {
    c.json(status, &json!({ "error": code, "message": message }))
}

/// `room_display_name(room)` for `Current.user`.
async fn room_display_name(c: &Ctx, room: &Room) -> Result<String> {
    let (app, room, user) = (c.app().clone(), room.clone(), require_current_user(c)?.clone());
    c.app()
        .db
        .read(move |conn| {
            let presenter = Presenter::new(conn, &app, None);
            Ok(presenter.room_view(&room, &user)?.display_name)
        })
        .await
        .map_err(db_error)
}

/// The bot reports are addressed to: `GEMINI_LIVE_VOICE_BOT` (an id or an exact name) among the
/// room's active bots, or without it the room's only active bot.
async fn voice_bot(c: &Ctx, room: &Room) -> Result<Option<User>> {
    let wanted = feature(c)?.config.voice_bot.clone();
    let room = room.clone();
    let bots = c.app().db.read(move |conn| room.active_bots(conn)).await.map_err(db_error)?;
    Ok(pick_bot(bots, wanted.as_deref()))
}

fn pick_bot(mut bots: Vec<User>, wanted: Option<&str>) -> Option<User> {
    match wanted {
        Some(wanted) => match wanted.parse::<i64>() {
            Ok(id) => bots.into_iter().find(|bot| bot.id == id),
            Err(_) => bots.into_iter().find(|bot| bot.name == wanted),
        },
        None if bots.len() == 1 => bots.pop(),
        None => None,
    }
}

// --- The report -----------------------------------------------------------------------------------

/// The most kept of each field, and of the transcript (bytes; longer text is cut with a note).
pub const MAX_FIELD_BYTES: usize = 2 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 20 * 1024;

/// `submit_incident`'s arguments plus the transcript. Everything in it came from a model and a
/// browser, so it's escaped into the message, never trusted as markup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IncidentReport {
    pub title: String,
    pub summary: String,
    pub what_happened: Option<String>,
    pub location: Option<String>,
    pub occurred_at: Option<String>,
    pub people_involved: Option<String>,
    pub injuries: Option<String>,
    pub actions_taken: Option<String>,
    pub severity: Option<Severity>,
    pub transcript: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }

    /// French label, plus the value the incident-report skill tags with (`sev-<value>`).
    fn label(self) -> &'static str {
        match self {
            Self::Low => "faible (low)",
            Self::Medium => "moyenne (medium)",
            Self::High => "élevée (high)",
            Self::Critical => "critique (critical)",
        }
    }
}

impl IncidentReport {
    /// From the JSON body; `None` without a title and a summary.
    pub fn from_params(params: &ParamMap) -> Option<Self> {
        let text = |key: &str, limit: usize| {
            params.get(key).and_then(Param::as_str).map(str::trim).filter(|value| !value.is_empty()).map(|value| truncate(value, limit))
        };
        let field = |key: &str| text(key, MAX_FIELD_BYTES);
        Some(Self {
            title: field("title")?,
            summary: field("summary")?,
            what_happened: field("what_happened"),
            location: field("location"),
            occurred_at: field("occurred_at"),
            people_involved: field("people_involved"),
            injuries: field("injuries"),
            actions_taken: field("actions_taken"),
            severity: params.get("severity").and_then(Param::as_str).and_then(Severity::parse),
            transcript: text("transcript", MAX_TRANSCRIPT_BYTES),
        })
    }

    /// The message body: the bot's mention (an Action Text attachment, as the composer inserts
    /// it), then the report and the transcript, in tags the rich text sanitizer keeps.
    pub fn to_html(&self, bot_sgid: &str) -> String {
        let mut html = format!(
            r#"<p><action-text-attachment sgid="{}" content-type="application/vnd.campfire.mention"></action-text-attachment> Compte rendu d’incident dicté en direct (voix), confirmé par l’auteur.</p>"#,
            escape(bot_sgid)
        );
        html.push_str(&format!("<h3>{}</h3>", escape(&self.title)));
        html.push_str(&format!("<p><strong>Résumé :</strong> {}</p>", multiline(&self.summary)));
        let details = [
            ("Ce qui s’est passé", &self.what_happened),
            ("Lieu", &self.location),
            ("Date et heure", &self.occurred_at),
            ("Personnes impliquées", &self.people_involved),
            ("Blessures", &self.injuries),
            ("Mesures prises", &self.actions_taken),
        ];
        html.push_str("<ul>");
        for (label, value) in details {
            let value = value.as_deref().map(multiline).unwrap_or_else(|| "<em>non précisé</em>".into());
            html.push_str(&format!("<li><strong>{label} :</strong> {value}</li>"));
        }
        let severity = self.severity.map(|severity| escape(severity.label())).unwrap_or_else(|| "<em>non précisée</em>".into());
        html.push_str(&format!("<li><strong>Gravité :</strong> {severity}</li></ul>"));
        if let Some(transcript) = &self.transcript {
            let note = if transcript.ends_with(TRUNCATED) { " (tronquée)" } else { "" };
            html.push_str(&format!("<p><strong>Transcription de l’entretien{note} :</strong></p>"));
            html.push_str(&format!("<blockquote>{}</blockquote>", multiline(transcript)));
        }
        html
    }
}

const TRUNCATED: &str = "…";

/// At most `limit` bytes, cut on a character boundary and marked with an ellipsis.
fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit.saturating_sub(TRUNCATED.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", value[..end].trim_end())
}

/// `ERB::Util.html_escape`
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Escaped, with its line breaks kept as `<br>`.
fn multiline(value: &str) -> String {
    value.lines().map(escape).collect::<Vec<_>>().join("<br>")
}

#[cfg(test)]
mod tests;
