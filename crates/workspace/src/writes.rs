//! Every write the workspace makes to Fizzy, in one place.
//!
//! - **Who writes**: a [`TokenSource`] gives the token to write with for the Campfire user acting
//!   (the [`ActingIdentity`]). For people that's the workspace's Fizzy account ([`SharedToken`],
//!   `FIZZY_TOKEN`, the "Campfire" Fizzy user, which then needs `write` permission); per-person
//!   Fizzy tokens later only need another `TokenSource`. What Campfire runs for Hermes (phase 2)
//!   uses Hermes's own token (`HERMES_FIZZY_TOKEN`) when it's set.
//! - **On whose behalf**: while the token isn't the person's own, every comment starts with their
//!   Campfire name ("Karim: …"), since Fizzy shows the token's user as the author.
//! - **Audit**: every write is recorded ([`WriteRecord`]: when, Campfire user, card, action, whose
//!   token, for what, outcome; never the token), in the durable write log (`actions.jsonl`,
//!   [`crate::journal`]) and in the server log.
//! - **Toggles**: Fizzy's taggings are toggles (posting a tag the card has removes it), so tag
//!   changes are computed against a fresh read of the card, only the differences are posted, and
//!   the card is read again to check the result. A failed toggle is never retried blindly.
//! - **Incident board only**: [`Writer::card`] answers `NotFound` for a card on any other board,
//!   before anything is written.

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::config::Secret;
use crate::fizzy::{Board, Card, Client, Column, FizzyError, HttpResponse};
use crate::home::Viewer;
use crate::html::escape;

/// The token and the name Fizzy will show for a write.
#[derive(Clone)]
pub struct ActingIdentity {
    pub token: Secret,
    /// The token isn't the actor's own: comments carry their name.
    pub on_behalf: bool,
    /// For the log: whose token (`workspace`, `hermes`, later `own`).
    pub label: &'static str,
}

impl std::fmt::Debug for ActingIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActingIdentity").field("on_behalf", &self.on_behalf).field("label", &self.label).finish_non_exhaustive()
    }
}

/// Where write tokens come from.
pub trait TokenSource: Send + Sync {
    /// The identity `actor`'s writes use, or why they can't write.
    fn identity(&self, actor: &Viewer) -> Result<ActingIdentity, String>;
}

/// Everyone writes through one account's token (the workspace's `FIZZY_TOKEN`).
pub struct SharedToken(pub Secret);

impl TokenSource for SharedToken {
    fn identity(&self, _actor: &Viewer) -> Result<ActingIdentity, String> {
        Ok(ActingIdentity { token: self.0.clone(), on_behalf: true, label: "workspace" })
    }
}

/// One write, for the durable write log and the server log. Never carries the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRecord {
    pub at: Timestamp,
    pub user_id: i64,
    pub card: Option<u64>,
    /// e.g. `tag +sev-high`, `move column:03ab`, `comment`, `create card`.
    pub action: String,
    /// Whose token (`ActingIdentity::label`).
    pub identity: &'static str,
    /// `ok`, or what went wrong.
    pub outcome: String,
    /// `workspace` (a person), `proposal` (Hermes, through Campfire) or `undo`.
    pub via: &'static str,
    /// The proposal or Hermes log entry this write belongs to.
    pub reference: Option<String>,
}

/// What a sequence of writes is for, in the records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purpose {
    pub via: &'static str,
    pub reference: Option<String>,
}

impl Purpose {
    pub fn person() -> Self {
        Self { via: "workspace", reference: None }
    }
}

/// Receives every [`WriteRecord`] (the app logs them).
pub type Audit = dyn Fn(&WriteRecord) + Send + Sync;

/// Why an action didn't (fully) happen. Messages are for the person acting; they never carry the
/// token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// No such card on the incident board (404).
    NotFound,
    /// The policy says no (403).
    Forbidden(String),
    /// Bad input (422).
    Invalid { code: &'static str, message: String },
    /// Fizzy is unreachable, refused, or didn't apply the change; nothing more was written (502).
    Fizzy(String),
    /// Too many heavy reads at once (`?comments=all`); nothing was asked of Fizzy (429).
    Busy(String),
}

impl ActionError {
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid { code, message: message.into() }
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Forbidden(_) => 403,
            Self::Invalid { .. } => 422,
            Self::Fizzy(_) => 502,
            Self::Busy(_) => 429,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Forbidden(_) => "forbidden",
            Self::Invalid { code, .. } => code,
            Self::Fizzy(_) => "fizzy_unavailable",
            Self::Busy(_) => "busy",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "No such card on the incident board.".into(),
            Self::Forbidden(message) | Self::Invalid { message, .. } | Self::Fizzy(message) | Self::Busy(message) => message.clone(),
        }
    }

    fn from_fizzy(error: &FizzyError) -> Self {
        Self::Fizzy(match error {
            FizzyError::Status(401) => {
                "Fizzy refused the change (401): the workspace's Fizzy token can't write (FIZZY_TOKEN needs write permission).".into()
            }
            FizzyError::Status(403) => "Fizzy refused the change (403).".into(),
            FizzyError::Transport(_) => "Fizzy can't be reached right now. Nothing was changed; try again in a moment.".into(),
            error => format!("Fizzy couldn't make the change ({error})."),
        })
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ActionError {}

/// Writes to the incident board's cards as one actor.
pub struct Writer<'a> {
    pub(crate) client: Client<'a>,
    pub(crate) identity: ActingIdentity,
    pub(crate) actor: &'a Viewer,
    pub(crate) account: String,
    pub(crate) board: Board,
    /// Records every write ([`crate::Workspace::record`]) and tells the time.
    pub(crate) workspace: &'a crate::Workspace,
    pub(crate) purpose: Purpose,
}

impl<'a> Writer<'a> {
    /// The card as Fizzy has it now (steps included), if it's on the incident board.
    pub async fn card(&self, number: u64) -> Result<Card, ActionError> {
        match self.client.card(&self.account, number).await {
            Ok(Some(card)) if card.board.as_ref().is_some_and(|board| board.id == self.board.id) => Ok(card),
            Ok(_) => Err(ActionError::NotFound),
            Err(error) => Err(ActionError::from_fizzy(&error)),
        }
    }

    pub async fn columns(&self) -> Result<Vec<Column>, ActionError> {
        self.client.columns(&self.account, &self.board.id).await.map_err(|error| ActionError::from_fizzy(&error))
    }

    /// One write; `ok` lists the statuses that mean it worked.
    async fn write(
        &self,
        card: Option<u64>,
        action: String,
        method: &str,
        path: &str,
        body: Option<Value>,
        ok: &[u16],
    ) -> Result<HttpResponse, ActionError> {
        match self.write_recorded(card, action, method, path, body, ok).await {
            Ok(response) if ok.contains(&response.status) => Ok(response),
            Ok(response) => Err(ActionError::from_fizzy(&FizzyError::Status(response.status))),
            Err(error) => Err(ActionError::from_fizzy(&error)),
        }
    }

    /// [`Writer::write`], recorded, with Fizzy's reply whatever its status.
    async fn write_recorded(
        &self,
        card: Option<u64>,
        action: String,
        method: &str,
        path: &str,
        body: Option<Value>,
        ok: &[u16],
    ) -> Result<HttpResponse, FizzyError> {
        let result = self.client.send_json(method, &format!("/{}{path}", self.account), body.as_ref(), &self.identity.token).await;
        let outcome = match &result {
            Ok(response) if ok.contains(&response.status) => "ok".to_string(),
            Ok(response) => FizzyError::Status(response.status).to_string(),
            Err(error) => error.to_string(),
        };
        // A created card's number, from Fizzy's reply.
        let card = card.or_else(|| match &result {
            Ok(response) if ok.contains(&response.status) => {
                serde_json::from_slice::<Value>(&response.body).ok().and_then(|reply| reply["number"].as_u64())
            }
            _ => None,
        });
        self.workspace.record(&WriteRecord {
            at: self.workspace.now(),
            user_id: self.actor.id,
            card,
            action,
            identity: self.identity.label,
            outcome,
            via: self.purpose.via,
            reference: self.purpose.reference.clone(),
        });
        result
    }

    /// `POST /boards/:id/cards` (201 + the card). Tags can't be set here (Fizzy ignores them).
    pub async fn create_card(&self, title: &str, description_html: &str) -> Result<Card, ActionError> {
        let body = json!({ "card": { "title": title, "description": description_html } });
        let response = self
            .write(None, "create card".into(), "POST", &format!("/boards/{}/cards.json", self.board.id), Some(body), &[200, 201])
            .await?;
        serde_json::from_slice(&response.body)
            .map_err(|_| ActionError::Fizzy("Fizzy created the card but its reply couldn't be read.".into()))
    }

    /// `POST /cards/:n/taggings`: a **toggle**. Use [`Writer::set_tags`].
    async fn toggle_tag(&self, number: u64, tag: &str, adding: bool) -> Result<(), ActionError> {
        let action = format!("tag {}{tag}", if adding { '+' } else { '-' });
        self.write(
            Some(number),
            action,
            "POST",
            &format!("/cards/{number}/taggings.json"),
            Some(json!({ "tag_title": tag })),
            &[200, 201, 204],
        )
        .await
        .map(|_| ())
    }

    /// Makes the card's tags among `managed` exactly `wanted` (both lowercase titles): only the
    /// differences with a fresh read are toggled, then the card is read again and checked. Returns
    /// the card as it is afterwards.
    pub async fn set_tags(&self, number: u64, managed: &[String], wanted: &[String]) -> Result<Card, ActionError> {
        let card = self.card(number).await?;
        let (remove, add) = tag_changes(&card, managed, wanted);
        for tag in &remove {
            self.toggle_tag(number, tag, false).await?;
        }
        for tag in &add {
            self.toggle_tag(number, tag, true).await?;
        }
        let after = if remove.is_empty() && add.is_empty() { card } else { self.card(number).await? };
        let (still_remove, still_add) = tag_changes(&after, managed, wanted);
        if !still_remove.is_empty() || !still_add.is_empty() {
            return Err(ActionError::Fizzy("Fizzy didn't apply every tag change; the card shows its tags as they are now.".into()));
        }
        Ok(after)
    }

    /// `POST /cards/:n/triage` into a column (reopens a closed or "not now" card).
    pub async fn triage(&self, number: u64, column_id: &str) -> Result<(), ActionError> {
        let body = json!({ "column_id": column_id });
        self.write(
            Some(number),
            format!("move column:{column_id}"),
            "POST",
            &format!("/cards/{number}/triage.json"),
            Some(body),
            &[200, 201, 204],
        )
        .await
        .map(|_| ())
    }

    /// `DELETE /cards/:n/triage`: back to "Maybe?" (New).
    pub async fn untriage(&self, number: u64) -> Result<(), ActionError> {
        self.write(Some(number), "move new".into(), "DELETE", &format!("/cards/{number}/triage.json"), None, &[200, 204]).await.map(|_| ())
    }

    /// `POST /cards/:n/not_now`.
    pub async fn not_now(&self, number: u64) -> Result<(), ActionError> {
        self.write(Some(number), "move not_now".into(), "POST", &format!("/cards/{number}/not_now.json"), None, &[200, 201, 204])
            .await
            .map(|_| ())
    }

    /// `POST /cards/:n/closure`.
    pub async fn close(&self, number: u64) -> Result<(), ActionError> {
        self.write(Some(number), "close".into(), "POST", &format!("/cards/{number}/closure.json"), None, &[200, 201, 204]).await.map(|_| ())
    }

    /// `DELETE /cards/:n/closure`: back in its column.
    pub async fn reopen(&self, number: u64) -> Result<(), ActionError> {
        self.write(Some(number), "reopen".into(), "DELETE", &format!("/cards/{number}/closure.json"), None, &[200, 204]).await.map(|_| ())
    }

    /// `PUT /cards/:n/steps/:id` `{"step": {"completed": …}}`.
    pub async fn set_step(&self, number: u64, step_id: &str, completed: bool) -> Result<(), ActionError> {
        let action = format!("step {step_id} {}", if completed { "done" } else { "undone" });
        let body = json!({ "step": { "completed": completed } });
        self.write(Some(number), action, "PUT", &format!("/cards/{number}/steps/{step_id}.json"), Some(body), &[200, 204]).await.map(|_| ())
    }

    /// `POST /cards/:n/comments`, the text as HTML paragraphs, prefixed with the actor's name while
    /// the token isn't theirs. `link` (a URL) is appended as a link. Returns the new comment's id,
    /// when Fizzy's reply says it (for undo).
    pub async fn comment(&self, number: u64, text: &str, link: Option<&str>) -> Result<Option<String>, ActionError> {
        let prefix = self.identity.on_behalf.then_some(self.actor.name.as_str());
        let body = json!({ "comment": { "body": comment_html(prefix, text, link) } });
        let response =
            self.write(Some(number), "comment".into(), "POST", &format!("/cards/{number}/comments.json"), Some(body), &[200, 201]).await?;
        Ok(serde_json::from_slice::<crate::fizzy::Comment>(&response.body).ok().map(|comment| comment.id).filter(|id| !id.is_empty()))
    }

    /// `DELETE /cards/:n/comments/:id`: Fizzy lets only the comment's creator do it.
    pub async fn delete_comment(&self, number: u64, comment_id: &str) -> Result<(), ActionError> {
        let path = format!("/cards/{number}/comments/{comment_id}.json");
        self.write(Some(number), format!("delete comment {comment_id}"), "DELETE", &path, None, &[200, 204]).await.map(|_| ())
    }

    /// `POST /cards/:n/steps` `{"step": {"content"}}`.
    pub async fn add_step(&self, number: u64, content: &str) -> Result<(), ActionError> {
        let body = json!({ "step": { "content": content, "completed": false } });
        self.write(Some(number), "step added".into(), "POST", &format!("/cards/{number}/steps.json"), Some(body), &[200, 201])
            .await
            .map(|_| ())
    }

    /// `POST /cards/:n/assignments` `{"assignee_id"}`: a **toggle**. Use [`Writer::set_owner`].
    async fn toggle_assignment(&self, number: u64, fizzy_user_id: &str, adding: bool) -> Result<(), ActionError> {
        let action = format!("owner {}{fizzy_user_id}", if adding { '+' } else { '-' });
        let body = json!({ "assignee_id": fizzy_user_id });
        let path = format!("/cards/{number}/assignments.json");
        let ok = [200, 201, 204];
        match self.write_recorded(Some(number), action, "POST", &path, Some(body), &ok).await {
            Ok(response) if ok.contains(&response.status) => Ok(()),
            // Fizzy's 404 here: that user can't access the board (or isn't active any more).
            Ok(response) if response.status == 404 && adding => Err(ActionError::Fizzy(
                "Fizzy refused: that person has no access to the incident board in Fizzy. An administrator can add them to the board."
                    .into(),
            )),
            Ok(response) => Err(ActionError::from_fizzy(&FizzyError::Status(response.status))),
            Err(error) => Err(ActionError::from_fizzy(&error)),
        }
    }

    /// Makes the card's owner exactly `wanted` (a Fizzy user id), or nobody: against a fresh read,
    /// the other owners are removed, then `wanted` added if missing (Fizzy's assignments are
    /// toggles), then the card is read again and checked. Returns the card as it is afterwards.
    pub async fn set_owner(&self, number: u64, wanted: Option<&str>) -> Result<Card, ActionError> {
        let card = self.card(number).await?;
        let (remove, add) = owner_changes(&card, wanted);
        for id in &remove {
            self.toggle_assignment(number, id, false).await?;
        }
        if let Some(id) = &add {
            self.toggle_assignment(number, id, true).await?;
        }
        let after = if remove.is_empty() && add.is_none() { card } else { self.card(number).await? };
        let (still_remove, still_add) = owner_changes(&after, wanted);
        if !still_remove.is_empty() || still_add.is_some() {
            return Err(ActionError::Fizzy("Fizzy didn't apply the owner change; the ticket shows its owner as it is now.".into()));
        }
        Ok(after)
    }

    /// `PUT /cards/:n` `{"card": {"title"}}`.
    pub async fn set_title(&self, number: u64, title: &str) -> Result<(), ActionError> {
        let body = json!({ "card": { "title": title } });
        self.write(Some(number), "title".into(), "PUT", &format!("/cards/{number}.json"), Some(body), &[200, 204]).await.map(|_| ())
    }
}

/// The owners to remove, then the one to add, so that the card's only owner is `wanted` (or nobody).
pub fn owner_changes(card: &Card, wanted: Option<&str>) -> (Vec<String>, Option<String>) {
    let remove = card.assignees.iter().map(|user| user.id.clone()).filter(|id| Some(id.as_str()) != wanted).collect();
    let add = wanted.filter(|wanted| !card.assignees.iter().any(|user| user.id == *wanted)).map(str::to_string);
    (remove, add)
}

/// The tags to remove, then to add, so that the card's tags among `managed` are `wanted`.
pub fn tag_changes(card: &Card, managed: &[String], wanted: &[String]) -> (Vec<String>, Vec<String>) {
    let remove = card
        .tags
        .iter()
        .filter(|tag| managed.iter().any(|managed| tag.trim_start_matches('#').eq_ignore_ascii_case(managed)))
        .filter(|tag| !wanted.iter().any(|wanted| tag.trim_start_matches('#').eq_ignore_ascii_case(wanted)))
        .cloned()
        .collect();
    let add = wanted.iter().filter(|tag| !card.has_tag(tag)).cloned().collect();
    (remove, add)
}

/// `<p>Karim: first paragraph</p><p>second<br>line</p>`, escaped.
pub fn comment_html(prefix: Option<&str>, text: &str, link: Option<&str>) -> String {
    let mut paragraphs: Vec<String> = text
        .replace("\r\n", "\n")
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| paragraph.lines().map(escape).collect::<Vec<_>>().join("<br>"))
        .collect();
    if let Some(link) = link {
        let link = format!(r#"<a href="{0}">{0}</a>"#, escape(link));
        match paragraphs.last_mut() {
            Some(last) => {
                last.push(' ');
                last.push_str(&link);
            }
            None => paragraphs.push(link),
        }
    }
    if let (Some(prefix), Some(first)) = (prefix, paragraphs.first_mut()) {
        *first = format!("{}: {first}", escape(prefix));
    }
    paragraphs.into_iter().map(|paragraph| format!("<p>{paragraph}</p>")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_escaped_and_prefixed() {
        assert_eq!(
            comment_html(Some("Karim"), "On it <now>\nmat & door\n\nDone.", None),
            "<p>Karim: On it &lt;now&gt;<br>mat &amp; door</p><p>Done.</p>"
        );
        assert_eq!(
            comment_html(None, "Hi", Some("https://c/rooms/1/@2?a=1&b")),
            r#"<p>Hi <a href="https://c/rooms/1/@2?a=1&amp;b">https://c/rooms/1/@2?a=1&amp;b</a></p>"#
        );
        assert_eq!(comment_html(Some("A"), "", Some("u")), r#"<p>A: <a href="u">u</a></p>"#);
    }

    #[test]
    fn tag_changes_only_touch_managed_tags() {
        let card = Card { tags: vec!["incident".into(), "sev-low".into(), "engineering".into()], ..Card::default() };
        let severities: Vec<String> = ["sev-low", "sev-medium", "sev-high", "sev-critical"].map(String::from).to_vec();
        assert_eq!(tag_changes(&card, &severities, &["sev-high".into()]), (vec!["sev-low".into()], vec!["sev-high".into()]));
        assert_eq!(tag_changes(&card, &severities, &["sev-low".into()]), (vec![], vec![]), "already right: nothing to toggle");
        assert_eq!(tag_changes(&card, &severities, &[]), (vec!["sev-low".into()], vec![]));
        let departments: Vec<String> = vec!["engineering".into(), "security".into()];
        assert_eq!(tag_changes(&card, &departments, &["security".into()]), (vec!["engineering".into()], vec!["security".into()]));
    }

    #[test]
    fn the_shared_token_writes_on_behalf_as_the_workspace() {
        let identity =
            SharedToken(Secret::new("t0k")).identity(&Viewer { id: 1, name: "K".into(), email: None, administrator: false }).unwrap();
        assert!(identity.on_behalf && identity.label == "workspace");
        assert!(!format!("{identity:?}").contains("t0k"));
        let error = ActionError::from_fizzy(&FizzyError::Status(401));
        assert!(error.message().contains("write permission") && error.status() == 502);
    }
}
