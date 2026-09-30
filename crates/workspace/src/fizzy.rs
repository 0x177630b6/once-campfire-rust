//! A client for Fizzy's JSON API (docs/api/fizzy-rest-api.md in the Hermes repo), over whatever
//! [`HttpClient`] the app provides. Every request carries `Accept: application/json` (Fizzy only
//! accepts a bearer token on JSON requests) and `Authorization: Bearer <token>`; paths are
//! `/<account>/…json`. Reads use `FIZZY_TOKEN`; writes ([`Client::send_json`], used by
//! [`crate::writes`]) take the acting identity's token. Lists are paginated with `?page=N` and a `Link: …; rel="next"` header;
//! the header's URL carries Fizzy's own `BASE_URL`, which may not be reachable from here, so the
//! client only reads whether there's a next page and asks for it on its own base URL.

use std::sync::LazyLock;

use base64::Engine;
use jiff::Timestamp;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::BoxFuture;
use crate::config::{Secret, WorkspaceConfig};

/// What the app's HTTP client returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// The `Link` header, if any.
    pub link: Option<String>,
    /// The `X-Total-Count` header (`geared_pagination` sets it on JSON lists), if any.
    pub total: Option<u64>,
}

/// One HTTP request. The app implements it over its own client (`integrations::net`); tests fake
/// it. Errors are plain descriptions and must never contain the request's headers.
pub trait HttpClient: Send + Sync {
    /// `method` is `GET`, `POST`, `PUT` or `DELETE`; `body`, when present, is JSON.
    fn send<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'static str, String)],
        body: Option<&'a [u8]>,
    ) -> BoxFuture<'a, Result<HttpResponse, String>>;

    fn get<'a>(&'a self, url: &'a str, headers: &'a [(&'static str, String)]) -> BoxFuture<'a, Result<HttpResponse, String>> {
        self.send("GET", url, headers, None)
    }
}

/// Never carries the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FizzyError {
    /// Fizzy answered with an unexpected status.
    Status(u16),
    Transport(String),
    Decode(String),
    /// Nothing to poll: the token has no account, or the incident board doesn't exist.
    Setup(String),
}

impl std::fmt::Display for FizzyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FizzyError::Status(401) => f.write_str("Fizzy refused the token (401)"),
            FizzyError::Status(403) => f.write_str("the token's user can't read this Fizzy account (403)"),
            FizzyError::Status(status) => write!(f, "Fizzy answered {status}"),
            FizzyError::Transport(error) => write!(f, "could not reach Fizzy: {error}"),
            FizzyError::Decode(error) => write!(f, "unexpected reply from Fizzy: {error}"),
            FizzyError::Setup(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for FizzyError {}

// --- Resources ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UserRef {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email_address: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BoardRef {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Column {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `{"name": "Lime", "value": "var(--color-card-4)"}` (the docs show a string).
    #[serde(default, deserialize_with = "color_name")]
    pub color: Option<String>,
}

/// A card as `cards/_card.json.jbuilder` renders it (the fields the workspace reads).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Card {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub postponed: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default, deserialize_with = "lenient_timestamp")]
    pub created_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "lenient_timestamp")]
    pub last_active_at: Option<Timestamp>,
    #[serde(default)]
    pub board: Option<BoardRef>,
    /// Kept on closed cards too (docs §5.3).
    #[serde(default)]
    pub column: Option<Column>,
    #[serde(default)]
    pub assignees: Vec<UserRef>,
    #[serde(default)]
    pub creator: Option<UserRef>,
    /// Plain text (`description_html` isn't shown: it's Fizzy's HTML).
    #[serde(default, deserialize_with = "lenient_string")]
    pub description: String,
    /// Only in `GET /cards/:n` (`show.json.jbuilder`).
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// A card's checklist item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Step {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub completed: bool,
}

/// A comment (`cards/comments/_comment.json.jbuilder`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Comment {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default, deserialize_with = "lenient_timestamp")]
    pub created_at: Option<Timestamp>,
    #[serde(default)]
    pub body: CommentBody,
    #[serde(default)]
    pub creator: Option<UserRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CommentBody {
    #[serde(default, deserialize_with = "lenient_string")]
    pub plain_text: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub html: String,
}

impl Comment {
    /// What the comment says, as text (the plain text Fizzy renders, else the HTML's text).
    pub fn text(&self) -> String {
        if self.body.plain_text.trim().is_empty() { crate::html::to_text(&self.body.html) } else { self.body.plain_text.trim().to_string() }
    }
}

impl Card {
    /// The most severe `sev-*` tag (a card should have one; a hand-tagged card may have several).
    pub fn severity(&self) -> Option<Severity> {
        self.tags.iter().filter_map(|tag| Severity::from_tag(tag)).max()
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|mine| mine.trim_start_matches('#').eq_ignore_ascii_case(tag))
    }

    /// Where the card is, in the workspace's words: Fizzy's "Maybe?" is "New", "Not Now" is
    /// "Monitoring", "Done" is "Closed".
    pub fn state(&self) -> CardState {
        if self.closed {
            CardState::Closed
        } else if self.postponed {
            CardState::Monitoring
        } else {
            match &self.column {
                Some(column) => CardState::Column(column.name.clone(), column.color.clone()),
                None => CardState::New,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardState {
    New,
    /// A board column: its name and Fizzy colour name.
    Column(String, Option<String>),
    Monitoring,
    Closed,
}

impl CardState {
    pub fn label(&self) -> &str {
        match self {
            CardState::New => "New",
            CardState::Column(name, _) => name,
            CardState::Monitoring => "Monitoring",
            CardState::Closed => "Closed",
        }
    }

    /// The CSS tone (`ws-cc--<tone>` in hermes/workspace.css).
    pub fn tone(&self) -> &'static str {
        const COLORS: [&str; 9] = ["blue", "gray", "tan", "yellow", "lime", "aqua", "violet", "purple", "pink"];
        match self {
            CardState::New => "new",
            CardState::Monitoring => "later",
            CardState::Closed => "done",
            CardState::Column(_, color) => {
                let color = color.as_deref().unwrap_or("").to_ascii_lowercase();
                COLORS.iter().find(|known| **known == color).copied().unwrap_or("doing")
            }
        }
    }
}

/// The `sev-*` tags the incident-report skill puts on cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.trim().trim_start_matches('#').strip_prefix("sev-")?)
    }

    /// The English values, the French labels the voice report writes, and a few short forms.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "low" | "faible" | "basse" => Some(Self::Low),
            "medium" | "med" | "moderate" | "moyenne" | "modérée" => Some(Self::Medium),
            "high" | "élevée" | "elevee" | "haute" => Some(Self::High),
            "critical" | "crit" | "critique" => Some(Self::Critical),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// The Fizzy tag: `sev-high`.
    pub fn tag(self) -> String {
        format!("sev-{}", self.as_str())
    }

    /// Most severe first.
    pub const ALL: [Severity; 4] = [Self::Critical, Self::High, Self::Medium, Self::Low];
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Board {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
}

/// An entry of `GET /:account/activities` (`activities/_activity.json.jbuilder`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Activity {
    #[serde(default, deserialize_with = "id_string")]
    pub id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default, deserialize_with = "lenient_timestamp")]
    pub created_at: Option<Timestamp>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub eventable_type: Option<String>,
    /// A card or a comment, in its current state.
    #[serde(default)]
    pub eventable: Value,
    #[serde(default)]
    pub board: Option<BoardRef>,
    #[serde(default)]
    pub creator: Option<UserRef>,
    /// `{column}` for triaged, `{old_title, new_title}`, … (`/activities` only).
    #[serde(default)]
    pub particulars: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Identity {
    #[serde(default)]
    accounts: Vec<IdentityAccount>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct IdentityAccount {
    #[serde(default)]
    slug: String,
    /// The token's user in that account.
    #[serde(default)]
    user: Option<UserRef>,
}

// --- Client ---------------------------------------------------------------------------------------

/// The most pages read of one list, so that a huge board can't stall a poll.
pub const MAX_PAGES: u32 = 10;
/// The most comment pages walked when Fizzy doesn't say how many comments there are.
pub const MAX_COMMENT_PAGES: u32 = 20;
/// `geared_pagination`'s default page sizes (Fizzy sets none): 15, 30, 50, then 100 per page.
const PAGE_SIZES: [u64; 4] = [15, 30, 50, 100];
/// The largest reply read (the app's client enforces it).
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct Client<'a> {
    http: &'a dyn HttpClient,
    config: &'a WorkspaceConfig,
}

impl<'a> Client<'a> {
    pub fn new(http: &'a dyn HttpClient, config: &'a WorkspaceConfig) -> Self {
        Self { http, config }
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        headers(&self.config.token)
    }

    pub fn config(&self) -> &WorkspaceConfig {
        self.config
    }

    async fn get(&self, path: &str) -> Result<HttpResponse, FizzyError> {
        let url = format!("{}{path}", self.config.fizzy_url);
        let headers = self.headers();
        self.http.get(&url, &headers).await.map_err(FizzyError::Transport)
    }

    /// One JSON resource, `None` on 404.
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, FizzyError> {
        let response = self.get(path).await?;
        match response.status {
            200 => serde_json::from_slice(&response.body).map(Some).map_err(|e| FizzyError::Decode(e.to_string())),
            404 => Ok(None),
            status => Err(FizzyError::Status(status)),
        }
    }

    /// A paginated list: pages 1.. while `Link` announces a next page, at most `max_pages`.
    pub async fn get_list<T: DeserializeOwned>(&self, path: &str, max_pages: u32) -> Result<Vec<T>, FizzyError> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut items = Vec::new();
        for page in 1..=max_pages.max(1) {
            let paged = if page == 1 { path.to_string() } else { format!("{path}{separator}page={page}") };
            let response = self.get(&paged).await?;
            if response.status != 200 {
                return Err(FizzyError::Status(response.status));
            }
            // One odd entry (a card without a number…) shouldn't lose the whole list.
            items.extend(decode_list(&response.body)?);
            if !has_next_page(response.link.as_deref()) {
                break;
            }
        }
        Ok(items)
    }

    /// The account to read: `FIZZY_ACCOUNT`, else the token's first account.
    pub async fn account(&self) -> Result<String, FizzyError> {
        if let Some(account) = &self.config.account {
            return Ok(account.clone());
        }
        let identity: Identity = self.get_json("/my/identity.json").await?.unwrap_or_default();
        identity
            .accounts
            .into_iter()
            .map(|account| account.slug.trim_matches('/').to_string())
            .find(|slug| !slug.is_empty())
            .ok_or_else(|| FizzyError::Setup("the Fizzy token has no account".into()))
    }

    /// The Fizzy user `token` belongs to in `account` (`GET /my/identity`), `None` if it has none
    /// there.
    pub async fn user_of(&self, token: &Secret, account: &str) -> Result<Option<String>, FizzyError> {
        let url = format!("{}/my/identity.json", self.config.fizzy_url);
        let response = self.http.get(&url, &headers(token)).await.map_err(FizzyError::Transport)?;
        if response.status != 200 {
            return Err(FizzyError::Status(response.status));
        }
        let identity: Identity = serde_json::from_slice(&response.body).map_err(|e| FizzyError::Decode(e.to_string()))?;
        Ok(identity
            .accounts
            .into_iter()
            .find(|each| each.slug.trim_matches('/') == account)
            .and_then(|each| each.user)
            .map(|user| user.id)
            .filter(|id| !id.is_empty()))
    }

    /// One page of a list, and whether there's a next one.
    pub async fn get_page<T: DeserializeOwned>(&self, path: &str, page: u32) -> Result<(Vec<T>, bool), FizzyError> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let paged = if page <= 1 { path.to_string() } else { format!("{path}{separator}page={page}") };
        let response = self.get(&paged).await?;
        if response.status != 200 {
            return Err(FizzyError::Status(response.status));
        }
        Ok((decode_list(&response.body)?, has_next_page(response.link.as_deref())))
    }

    /// `/activities` of the incident board, optionally by one creator.
    pub fn activities_path(account: &str, board_id: &str, creator: Option<&str>) -> String {
        let mut path = format!("/{account}/activities.json?");
        if let Some(creator) = creator {
            path.push_str(&format!("creator_ids%5B%5D={creator}&"));
        }
        path.push_str(&format!("board_ids%5B%5D={board_id}"));
        path
    }

    pub async fn boards(&self, account: &str) -> Result<Vec<Board>, FizzyError> {
        self.get_list(&format!("/{account}/boards.json"), 5).await
    }

    pub async fn columns(&self, account: &str, board_id: &str) -> Result<Vec<Column>, FizzyError> {
        Ok(self.get_json(&format!("/{account}/boards/{board_id}/columns.json")).await?.unwrap_or_default())
    }

    /// `GET /cards.json?board_ids[]=…` plus `extra` query parameters (`indexed_by=closed`…).
    pub async fn board_cards(&self, account: &str, board_id: &str, extra: &str, max_pages: u32) -> Result<Vec<Card>, FizzyError> {
        let mut path = format!("/{account}/cards.json?board_ids%5B%5D={board_id}");
        if !extra.is_empty() {
            path.push('&');
            path.push_str(extra);
        }
        self.get_list(&path, max_pages).await
    }

    pub async fn card(&self, account: &str, number: u64) -> Result<Option<Card>, FizzyError> {
        self.get_json(&format!("/{account}/cards/{number}.json")).await
    }

    pub async fn activities(&self, account: &str, pages: u32) -> Result<Vec<Activity>, FizzyError> {
        self.get_list(&format!("/{account}/activities.json"), pages).await
    }

    pub async fn user(&self, account: &str, id: &str) -> Result<Option<UserRef>, FizzyError> {
        self.get_json(&format!("/{account}/users/{id}.json")).await
    }

    /// A card's newest `keep` comments, oldest first, and whether earlier ones were left out.
    ///
    /// Fizzy lists comments oldest first (`comments.chronologically`), in `geared_pagination`
    /// pages with a `Link: rel="next"` but no "last" link. Its `X-Total-Count` gives the page
    /// holding the `keep`-th newest comment, so the sheet reads page 1, then that page and the
    /// ones after it (three requests at most for 100 comments). Without that header the pages are
    /// walked (at most [`MAX_COMMENT_PAGES`]), keeping the last `keep`.
    pub async fn latest_comments(&self, account: &str, number: u64, keep: usize) -> Result<(Vec<Comment>, bool), FizzyError> {
        let path = format!("/{account}/cards/{number}/comments.json");
        let first = self.get(&path).await?;
        match first.status {
            200 => {}
            404 => return Ok((Vec::new(), false)),
            status => return Err(FizzyError::Status(status)),
        }
        let mut items: Vec<Comment> = decode_list(&first.body)?;
        // How many comments come before `items`.
        let mut skipped = 0u64;
        let mut link = first.link;
        let mut page = 1;
        if let Some(total) = first.total.filter(|total| *total > keep as u64)
            && has_next_page(link.as_deref())
        {
            let from = page_of(total - keep as u64);
            if from > 1 {
                items.clear();
                skipped = page_offset(from);
                page = from - 1;
            }
        }
        let last = page + MAX_COMMENT_PAGES;
        while has_next_page(link.as_deref()) && page < last {
            page += 1;
            let response = self.get(&format!("{path}?page={page}")).await?;
            if response.status != 200 {
                return Err(FizzyError::Status(response.status));
            }
            items.extend(decode_list(&response.body)?);
            link = response.link;
            let extra = items.len().saturating_sub(keep);
            items.drain(..extra);
            skipped += extra as u64;
        }
        let extra = items.len().saturating_sub(keep);
        items.drain(..extra);
        skipped += extra as u64;
        Ok((items, skipped > 0))
    }

    /// A write (or any request) with `token`, and a JSON body. The reply is returned whatever its
    /// status; only a transport failure is an error.
    pub async fn send_json(&self, method: &str, path: &str, body: Option<&Value>, token: &Secret) -> Result<HttpResponse, FizzyError> {
        let url = format!("{}{path}", self.config.fizzy_url);
        let mut headers = headers(token);
        let body = body.map(|body| body.to_string().into_bytes());
        if body.is_some() {
            headers.push(("Content-Type", "application/json".into()));
        }
        self.http.send(method, &url, &headers, body.as_deref()).await.map_err(FizzyError::Transport)
    }
}

fn headers(token: &Secret) -> Vec<(&'static str, String)> {
    vec![
        ("Accept", "application/json".into()),
        ("Authorization", format!("Bearer {}", token.expose())),
        ("User-Agent", "campfire-workspace".into()),
    ]
}

/// A list page's items; one odd entry doesn't lose the page.
fn decode_list<T: DeserializeOwned>(body: &[u8]) -> Result<Vec<T>, FizzyError> {
    let values: Vec<Value> = serde_json::from_slice(body).map_err(|e| FizzyError::Decode(e.to_string()))?;
    Ok(values.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect())
}

fn page_size(page: u32) -> u64 {
    PAGE_SIZES[(page.max(1) as usize - 1).min(PAGE_SIZES.len() - 1)]
}

/// The index of the first item of `page` (from 1), as `geared_pagination`'s `PortionAtOffset`.
pub fn page_offset(page: u32) -> u64 {
    (1..page.max(1)).map(page_size).sum()
}

/// The page holding the item at `index` (from 0).
pub fn page_of(index: u64) -> u32 {
    let (mut page, mut end) = (1, page_size(1));
    while index >= end {
        page += 1;
        end += page_size(page);
    }
    page
}

/// `Link: <…?page=2>; rel="next"`.
pub fn has_next_page(link: Option<&str>) -> bool {
    link.is_some_and(|link| {
        link.split(',').any(|part| part.split(';').skip(1).any(|param| param.trim().replace(' ', "") == "rel=\"next\""))
    })
}

// --- Mentions in rich text ------------------------------------------------------------------------

/// Fizzy user ids @mentioned in a comment's `body.html`, in order, no repeats. A mention is an
/// `<action-text-attachment content-type="application/vnd.actiontext.mention" sgid=…>`; the sgid's
/// payload (before `--`) is base64 JSON naming `gid://fizzy/User/<id>`, read, not verified (Fizzy
/// resolved it already). The avatar path inside the attachment is the fallback. The Hermes bridge
/// reads them the same way (`mentioned_fizzy_user_ids`).
pub fn mentioned_user_ids(body_html: &str) -> Vec<String> {
    static ATTACHMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<action-text-attachment\b[^>]*>").unwrap());
    static ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"([\w-]+)="([^"]*)""#).unwrap());
    static AVATAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/users/([A-Za-z0-9_-]+)/avatar").unwrap());
    let mut found: Vec<String> = Vec::new();
    for tag in ATTACHMENT.find_iter(body_html) {
        let attribute = |name: &str| {
            ATTRIBUTE
                .captures_iter(tag.as_str())
                .find(|caps| caps[1].eq_ignore_ascii_case(name))
                .map(|caps| crate::html::decode_entities(&caps[2]))
        };
        if !attribute("content-type").is_some_and(|kind| kind.contains("mention")) {
            continue;
        }
        let user_id = attribute("sgid").and_then(|sgid| user_id_from_sgid(&sgid)).or_else(|| {
            let tail = &body_html[tag.end()..];
            let end = tail.to_ascii_lowercase().find("</action-text-attachment>").unwrap_or(tail.len());
            AVATAR.captures(&tail[..end]).map(|caps| caps[1].to_string())
        });
        if let Some(id) = user_id
            && !found.contains(&id)
        {
            found.push(id);
        }
    }
    found
}

fn user_id_from_sgid(sgid: &str) -> Option<String> {
    static GID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"gid://[\w.-]+/User/([A-Za-z0-9_-]+)").unwrap());
    static NESTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""(?:message|data)"\s*:\s*"([A-Za-z0-9+/=_-]{8,})""#).unwrap());
    let mut text = decode_base64(sgid.split("--").next().unwrap_or(""));
    for _ in 0..3 {
        if let Some(caps) = GID.captures(&text) {
            return Some(caps[1].to_string());
        }
        let nested: Vec<String> = NESTED.captures_iter(&text).map(|caps| decode_base64(&caps[1])).collect();
        if nested.is_empty() {
            break;
        }
        text = nested.join(" ");
    }
    None
}

fn decode_base64(data: &str) -> String {
    let data = percent_decode(data);
    let trimmed = data.trim_end_matches('=');
    let engines = [base64::engine::general_purpose::STANDARD_NO_PAD, base64::engine::general_purpose::URL_SAFE_NO_PAD];
    engines
        .iter()
        .find_map(|engine| engine.decode(trimmed).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = value.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The card number in a card URL (`…/<account>/cards/<n>…`).
pub fn card_number_in(url: &str) -> Option<u64> {
    static CARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/cards/(\d+)(?:[/?#.]|$)").unwrap());
    CARD.captures(url).and_then(|caps| caps[1].parse().ok())
}

// --- Lenient decoding -----------------------------------------------------------------------------

/// Ids are strings (base36 UUIDs), but accept numbers too.
fn id_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(id) => id,
        Value::Number(id) => id.to_string(),
        _ => String::new(),
    })
}

fn lenient_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => text,
        _ => String::new(),
    })
}

fn color_name<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Object(color) => color.get("name").and_then(Value::as_str).map(str::to_string),
        Value::String(color) => Some(color),
        _ => None,
    })
}

/// An RFC 3339 time, or `None` for anything else (never an error: one odd field shouldn't drop a
/// card).
fn lenient_timestamp<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Timestamp>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(time) => time.parse().ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_a_card_leniently() {
        let card: Card = serde_json::from_value(json!({
            "id": "03fa", "number": 12, "title": "Lift B out of service", "status": "published",
            "tags": ["incident", "sev-high"], "closed": false, "postponed": false,
            "created_at": "2026-09-29T08:00:00.123Z", "last_active_at": "not a time",
            "url": "http://localhost:8484/897362094/cards/12",
            "board": {"id": "b1", "name": "Incident Log", "all_access": true},
            "column": {"id": "c1", "name": "In progress", "color": {"name": "Lime", "value": "var(--color-card-4)"}},
            "assignees": [{"id": "u1", "name": "Karim"}]
        }))
        .unwrap();
        assert_eq!(card.number, 12);
        assert_eq!(card.severity(), Some(Severity::High));
        assert_eq!(card.created_at.unwrap().to_string(), "2026-09-29T08:00:00.123Z");
        assert_eq!(card.last_active_at, None);
        assert_eq!(card.state(), CardState::Column("In progress".into(), Some("Lime".into())));
        assert_eq!(card.state().tone(), "lime");
    }

    #[test]
    fn card_states() {
        let card = |closed, postponed, column: Option<&str>| Card {
            closed,
            postponed,
            column: column.map(|name| Column { name: name.into(), ..Column::default() }),
            ..Card::default()
        };
        assert_eq!(card(false, false, None).state().label(), "New");
        assert_eq!(card(false, true, None).state().label(), "Monitoring");
        assert_eq!(card(true, false, Some("In progress")).state().label(), "Closed");
        assert_eq!(card(false, false, Some("Vendor")).state().tone(), "doing");
    }

    #[test]
    fn severities() {
        assert_eq!(Severity::from_tag("sev-critical"), Some(Severity::Critical));
        assert_eq!(Severity::from_tag("#sev-low"), Some(Severity::Low));
        assert_eq!(Severity::from_tag("incident"), None);
        assert_eq!(Severity::parse("Élevée"), Some(Severity::High));
        assert!(Severity::Critical > Severity::High);
    }

    #[test]
    fn geared_pages() {
        assert_eq!([1, 2, 3, 4, 5, 6].map(page_offset), [0, 15, 45, 95, 195, 295]);
        assert_eq!([0, 14, 15, 44, 45, 94, 95, 194, 195, 250].map(page_of), [1, 1, 2, 2, 3, 3, 4, 4, 5, 5]);
    }

    #[test]
    fn next_page_links() {
        assert!(has_next_page(Some(r#"<http://localhost:8484/1/cards.json?page=2>; rel="next""#)));
        assert!(!has_next_page(Some(r#"<http://x/1/cards.json?page=1>; rel="prev""#)));
        assert!(!has_next_page(None));
    }

    #[test]
    fn reads_mentioned_users() {
        let gid = base64::engine::general_purpose::STANDARD.encode(r#"{"_rails":{"data":"gid://fizzy/User/03abc","pur":"attachable"}}"#);
        let nested_inner = base64::engine::general_purpose::STANDARD.encode("gid://fizzy/User/03def");
        let nested = base64::engine::general_purpose::STANDARD.encode(format!(r#"{{"_rails":{{"message":"{nested_inner}"}}}}"#));
        let html = format!(
            concat!(
                r#"<p>Hi <action-text-attachment content-type="application/vnd.actiontext.mention" sgid="{gid}--sig"></action-text-attachment>"#,
                r#" and <action-text-attachment sgid="{nested}--sig" content-type="application/vnd.actiontext.mention"></action-text-attachment>"#,
                r#" and <action-text-attachment content-type="application/vnd.actiontext.mention" sgid="bad"><img src="/1/users/03ghi/avatar"></action-text-attachment>"#,
                r#" <action-text-attachment content-type="image/png" sgid="{gid}--x"></action-text-attachment>"#,
                r#" again <action-text-attachment content-type="application/vnd.actiontext.mention" sgid="{gid}--sig"></action-text-attachment></p>"#
            ),
            gid = gid,
            nested = nested
        );
        assert_eq!(mentioned_user_ids(&html), vec!["03abc", "03def", "03ghi"]);
    }

    #[test]
    fn card_numbers_in_urls() {
        assert_eq!(card_number_in("http://fizzy/1/cards/12"), Some(12));
        assert_eq!(card_number_in("http://fizzy/1/cards/12#comment_3"), Some(12));
        assert_eq!(card_number_in("http://fizzy/1/cards/12x"), None);
    }
}
