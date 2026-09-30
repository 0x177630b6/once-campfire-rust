//! Hermes's incident drafts: a bot message that shows a report and asks the reporter to confirm
//! before anything is filed (the incident-report skill's step 4: "Reply **confirm** to file it in
//! Fizzy, or tell me what to change."). Such a message gets File / Edit / Dismiss buttons; File and
//! Dismiss post "confirm" or "cancel" in the room as the user, with a mention of the bot so its
//! webhook fires in a shared room too. Edit puts a mention and "Change: " in the composer.
//!
//! Phase 0 recognizes drafts by their wording, in English or French. A structured marker in the
//! bot's message would be more reliable (a later phase).

use std::sync::LazyLock;

use jiff::{SignedDuration, Timestamp};
use regex::Regex;

use crate::fizzy::Severity;
use crate::html::{self, escape};

/// A draft nobody answered expires from the Home page after this (it can still be answered in
/// the room).
pub const DRAFT_TTL: SignedDuration = SignedDuration::from_hours(24);

/// What the workspace shows of a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub severity: Option<Severity>,
}

/// The draft in a bot message's body, if it is one.
pub fn detect(body_html: &str) -> Option<Draft> {
    // Asks to reply with a confirmation word...
    static INVITE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"(?i)\b(reply|respond|answer|type|say|send|write|r[ée]ponds?|r[ée]pondez|tape[sz]?|[ée]cri(?:s|vez)|envoie[sz]?|dites|dis)\b",
            r#"[^\n.!?]{0,30}?[\s"“«'*]"#,
            r"(confirm|confirme[rz]?|confirmation|yes|oui|ok|valide[rz]?)\b",
        ))
        .unwrap()
    });
    // ...before filing something.
    static FILING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(fizzy|card|carte|incident|report|rapport|file|log|enregistr\w*|cr[ée]er|ticket)\b").unwrap());
    let text = html::to_text(body_html);
    let tail_start = text.char_indices().rev().nth(600).map_or(0, |(at, _)| at);
    let invite = INVITE.find(&text[tail_start..])?;
    if !FILING.is_match(&text) {
        return None;
    }
    let body = &text[..tail_start + invite.start()];
    Some(Draft { title: title(body_html, body), severity: severity(&text) })
}

/// A title line (`Title: …`), else the first heading, else the first line with an em dash (the
/// skill's "<type> — <summary> — <location>"), else the first bold text that isn't a label, else
/// the first line.
fn title(body_html: &str, text: &str) -> String {
    static LABELLED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^\W{0,3}(?:title|titre)\W{0,3}\s*[:：]\s*\**\s*(.+)$").unwrap());
    static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<h[1-4]\b[^>]*>(.*?)</h[1-4]>").unwrap());
    static BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<(?:b|strong)\b[^>]*>(.*?)</(?:b|strong)>").unwrap());
    let clean = |value: &str| html::truncate(value.trim().trim_matches('*').trim(), 120);
    if let Some(caps) = LABELLED.captures(text) {
        return clean(&caps[1]);
    }
    if let Some(caps) = HEADING.captures(body_html) {
        let heading = html::to_text(&caps[1]);
        if !heading.is_empty() {
            return clean(&heading);
        }
    }
    if let Some(line) = text.lines().find(|line| line.contains(" — ") && line.chars().count() <= 160) {
        return clean(line.split_once(':').filter(|(label, _)| label.chars().count() < 20).map_or(line, |(_, rest)| rest));
    }
    let bold = BOLD
        .captures_iter(body_html)
        .map(|caps| html::to_text(&caps[1]))
        .find(|bold| bold.chars().count() > 3 && !bold.trim_end().ends_with(':'));
    if let Some(bold) = bold {
        return clean(&bold);
    }
    text.lines().next().map(clean).unwrap_or_else(|| "Incident draft".into())
}

fn severity(text: &str) -> Option<Severity> {
    static SEV_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bsev-(low|medium|high|critical)\b").unwrap());
    static LABELLED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(?:severity|gravit[ée])\W{0,4}\s*[:：]?\s*\**\s*(low|medium|high|critical|faible|moyenne|[ée]lev[ée]e|critique)\b",
        )
        .unwrap()
    });
    SEV_TAG.captures(text).or_else(|| LABELLED.captures(text)).and_then(|caps| Severity::parse(&caps[1]))
}

/// The user's answer to a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// File it: "confirm".
    Confirm,
    /// Don't: "cancel".
    Dismiss,
}

impl Decision {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "confirm" | "file" => Some(Self::Confirm),
            "dismiss" | "cancel" => Some(Self::Dismiss),
            _ => None,
        }
    }

    /// The word the incident-report skill waits for.
    pub fn reply(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Dismiss => "cancel",
        }
    }

    /// The reply's body: a mention of the bot (the attachment the composer inserts, so Campfire
    /// delivers it to the bot in a shared room), then the word.
    pub fn reply_html(self, bot_sgid: &str) -> String {
        format!(
            r#"<p><action-text-attachment sgid="{}" content-type="application/vnd.campfire.mention"></action-text-attachment> {}</p>"#,
            escape(bot_sgid),
            self.reply()
        )
    }
}

/// A short message that answers a draft (yes or no, in either language), whatever mention it
/// starts with.
pub fn is_decision_reply(body_html: &str) -> bool {
    const WORDS: [&str; 17] = [
        "confirm", "confirmed", "confirme", "confirmer", "yes", "oui", "ok", "okay", "valide", "valider", "go", "cancel", "dismiss",
        "annule", "annuler", "no", "non",
    ];
    let text = html::to_text(body_html).to_lowercase();
    let words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect();
    words.len() <= 6 && words.iter().any(|word| WORDS.contains(word))
}

/// A message as the workspace sees it (the app fills these in from its database).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: i64,
    pub room_id: i64,
    pub room_name: String,
    /// The message in its room (`/rooms/:room_id/@:id`).
    pub url: String,
    pub creator_id: i64,
    pub creator_name: String,
    pub creator_is_bot: bool,
    pub created_at: Timestamp,
    /// The stored body (Action Text HTML).
    pub body_html: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDraft {
    pub message: ChatMessage,
    pub draft: Draft,
}

/// The drafts still waiting for an answer: bot drafts younger than [`DRAFT_TTL`] with no later
/// message from that bot (it filed, re-drafted or moved on) and no later yes/no from anyone in
/// the room. Newest first.
pub fn pending(messages: &[ChatMessage], now: Timestamp) -> Vec<PendingDraft> {
    let mut sorted: Vec<&ChatMessage> = messages.iter().collect();
    sorted.sort_by_key(|message| (message.created_at, message.id));
    let mut pending = Vec::new();
    for (at, message) in sorted.iter().enumerate() {
        if !message.creator_is_bot || now.duration_since(message.created_at) > DRAFT_TTL {
            continue;
        }
        let Some(draft) = detect(&message.body_html) else { continue };
        let answered = sorted[at + 1..]
            .iter()
            .filter(|later| later.room_id == message.room_id)
            .any(|later| later.creator_id == message.creator_id || (!later.creator_is_bot && is_decision_reply(&later.body_html)));
        if !answered {
            pending.push(PendingDraft { message: (*message).clone(), draft });
        }
    }
    pending.reverse();
    pending
}

/// A bot as the draft buttons need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    pub id: i64,
    pub name: String,
    /// The bot's attachable sgid, for the mention.
    pub sgid: String,
}

/// `POST` here with `{"decision": "confirm" | "dismiss"}`.
pub fn reply_path(message_id: i64) -> String {
    format!("/workspace/drafts/{message_id}/reply")
}

/// The buttons under a draft (`hermes/workspace.js` handles them). `message_url` is set where the
/// composer isn't (the Home page): Edit then opens the message in its room.
pub fn buttons(message_id: i64, bot: &Bot, message_url: Option<&str>) -> String {
    let edit = match message_url {
        Some(url) => format!(r#"<a class="btn ws-draft__btn" href="{}" data-turbo-frame="_top">Edit</a>"#, escape(url)),
        None => r#"<button type="button" class="btn ws-draft__btn" data-ws-draft-action="edit">Edit</button>"#.to_string(),
    };
    format!(
        concat!(
            r#"<div class="ws-draft" data-ws-draft="{id}" data-ws-draft-url="{url}" data-ws-bot-name="{name}" data-ws-bot-sgid="{sgid}">"#,
            r#"<span class="ws-draft__hint">Draft awaiting confirmation</span>"#,
            r#"<span class="ws-draft__actions">"#,
            r#"<button type="button" class="btn btn--reversed ws-draft__btn" data-ws-draft-action="confirm">File</button>"#,
            "{edit}",
            r#"<button type="button" class="btn ws-draft__btn ws-draft__btn--quiet" data-ws-draft-action="dismiss">Dismiss</button>"#,
            r#"</span><span class="ws-draft__status" role="status"></span></div>"#
        ),
        id = message_id,
        url = escape(&reply_path(message_id)),
        name = escape(&bot.name),
        sgid = escape(&bot.sgid),
        edit = edit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A draft as the bridge posts Hermes's markdown (`markdown_to_paragraphs`).
    const SKILL_DRAFT: &str = concat!(
        "<p>Here is the draft report:</p>",
        "<p><b>Incident — AC leak — room 103</b><br>",
        "<b>Type:</b> incident<br><b>Severity:</b> high<br><b>Location:</b> room 103</p>",
        "<p><b>What happened:</b> water dripping from the AC unit since last night. Guest moved.</p>",
        "<p>Reply <b>confirm</b> to file it in Fizzy, or tell me what to change.</p>"
    );

    #[test]
    fn recognizes_the_skill_draft() {
        let draft = detect(SKILL_DRAFT).unwrap();
        assert_eq!(draft.title, "Incident — AC leak — room 103");
        assert_eq!(draft.severity, Some(Severity::High));
    }

    #[test]
    fn recognizes_french_and_labelled_drafts() {
        let french = "<p>Titre : Chute d’un client dans le hall</p><p>Gravité : critique</p><p>Répondez « confirmer » pour créer la carte Fizzy.</p>";
        let draft = detect(french).unwrap();
        assert_eq!((draft.title.as_str(), draft.severity), ("Chute d’un client dans le hall", Some(Severity::Critical)));

        let headed = "<h3>Lost property — laptop bag</h3><p>Tags: incident, sev-low</p><p>Type <code>yes</code> and I'll log it.</p>";
        let draft = detect(headed).unwrap();
        assert_eq!((draft.title.as_str(), draft.severity), ("Lost property — laptop bag", Some(Severity::Low)));
    }

    #[test]
    fn ignores_other_bot_messages() {
        for other in [
            "<p>Card #13 filed: http://fizzy/1/cards/13</p>",
            "<p>Reply confirm to subscribe to the newsletter.</p>",
            "<p>Hello! How can I help?</p>",
            "<p>The incident was confirmed by security yesterday.</p>",
        ] {
            assert_eq!(detect(other), None, "{other}");
        }
    }

    #[test]
    fn decisions() {
        assert_eq!(Decision::parse("File"), Some(Decision::Confirm));
        assert_eq!(Decision::parse("dismiss"), Some(Decision::Dismiss));
        assert_eq!(Decision::parse("maybe"), None);
        assert_eq!(
            Decision::Confirm.reply_html("sg\"id"),
            r#"<p><action-text-attachment sgid="sg&quot;id" content-type="application/vnd.campfire.mention"></action-text-attachment> confirm</p>"#
        );
        assert!(is_decision_reply(r#"<p><action-text-attachment sgid="x"></action-text-attachment> confirm</p>"#));
        assert!(is_decision_reply("<p>@Hermes oui, valide</p>"));
        assert!(!is_decision_reply("<p>change the location to room 107 please, not 117, ok</p>"));
    }

    fn message(id: i64, room_id: i64, creator_id: i64, bot: bool, minutes_ago: i64, body: &str) -> ChatMessage {
        ChatMessage {
            id,
            room_id,
            room_name: format!("room {room_id}"),
            url: format!("/rooms/{room_id}/@{id}"),
            creator_id,
            creator_name: if bot { "Hermes".into() } else { "Maya".into() },
            creator_is_bot: bot,
            created_at: now() - SignedDuration::from_mins(minutes_ago),
            body_html: body.into(),
        }
    }

    fn now() -> Timestamp {
        "2026-09-30T09:00:00Z".parse().unwrap()
    }

    #[test]
    fn pending_drafts() {
        let messages = vec![
            // Room 1: a draft, then its confirmation: answered.
            message(1, 1, 9, true, 60, SKILL_DRAFT),
            message(2, 1, 5, false, 59, "<p>@Hermes confirm</p>"),
            // Room 2: a draft, then a new draft from the bot: only the second is pending.
            message(3, 2, 9, true, 50, SKILL_DRAFT),
            message(4, 2, 5, false, 49, "<p>@Hermes the guest was moved to 107</p>"),
            message(5, 2, 9, true, 48, SKILL_DRAFT),
            // Room 3: a draft with unrelated chatter after it: pending.
            message(6, 3, 9, true, 30, SKILL_DRAFT),
            message(7, 3, 6, false, 29, "<p>Anyone seen the keys to the plant room?</p>"),
            // Room 4: too old.
            message(8, 4, 9, true, 25 * 60, SKILL_DRAFT),
        ];
        let ids: Vec<i64> = pending(&messages, now()).iter().map(|p| p.message.id).collect();
        assert_eq!(ids, vec![6, 5]);
    }

    #[test]
    fn buttons_carry_what_the_page_needs() {
        let bot = Bot { id: 9, name: "Hermes".into(), sgid: "abc".into() };
        let html = buttons(42, &bot, None);
        assert!(html.contains(r#"data-ws-draft-url="/workspace/drafts/42/reply""#));
        assert!(html.contains(r#"data-ws-bot-sgid="abc""#) && html.contains(r#"data-ws-draft-action="edit""#));
        let home = buttons(42, &bot, Some("/rooms/1/@42"));
        assert!(home.contains(r#"<a class="btn ws-draft__btn" href="/rooms/1/@42""#));
    }
}
