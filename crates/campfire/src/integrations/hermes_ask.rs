//! Hermes fork: `ask_hermes`, the live voice interviewer's question to the company's Hermes agent
//! (`controllers::voice::ask`, docs/hermes-gemini-live.md).
//!
//! Gemini Live calls `ask_hermes(question)` in the browser; the page POSTs the question to
//! Campfire, which forwards it, server-side, to the Hermes bridge's `POST /ask/<secret>`
//! (`HERMES_ASK_URL`) and waits for `{"answer": "…"}`. The URL carries the bridge's secret: it's
//! never logged, and never reaches the browser.

use std::time::Duration;

use campfire_richtext::uri;
use serde_json::{Value, json};

use crate::config::ApiKey;
use crate::integrations::net::http::{self, Body, Endpoint, HttpError, Timeouts};
use crate::integrations::net::{BoxFuture, Network};

/// The whole question (connect, request, Hermes thinking, reply) must finish within this. The
/// bridge gives Hermes 55 s, so its own 504 normally comes first.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);
/// Connecting to the bridge (on the Docker network) is quick or broken.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest question forwarded (characters; longer ones are cut with `…`).
pub const MAX_QUESTION_CHARS: usize = 1000;
/// The largest bridge reply read; an answer is a few sentences.
const MAX_REPLY_SIZE: usize = 64 * 1024;

/// What the bridge gets: who asks, from which room, and the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub room_id: i64,
    pub user_name: String,
    pub room_name: String,
    pub question: String,
}

impl Question {
    pub fn to_json(&self) -> Value {
        json!({
            "room_id": self.room_id,
            "user_name": self.user_name,
            "room_name": self.room_name,
            "question": self.question,
        })
    }
}

/// Never carries the URL (its path is a secret).
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("the Hermes bridge answered {0}")]
    Status(u16),
    #[error("Hermes did not answer in time")]
    Timeout,
    #[error("could not reach the Hermes bridge: {0}")]
    Transport(String),
    #[error("unexpected reply from the Hermes bridge")]
    BadReply,
    #[error("HERMES_ASK_URL is not an http(s) URL")]
    InvalidUrl,
}

/// Asks Hermes one question, returning its answer. [`HttpAsker`] in production; tests substitute
/// their own.
pub trait HermesAsker: Send + Sync {
    fn ask(&self, question: Question) -> BoxFuture<'_, Result<String, AskError>>;
}

/// `POST HERMES_ASK_URL` with the question as JSON, over the shared `integrations::net` client.
pub struct HttpAsker {
    net: Network,
    url: ApiKey,
}

impl HttpAsker {
    pub fn new(net: Network, url: ApiKey) -> Self {
        Self { net, url }
    }

    async fn post(&self, body: Vec<u8>) -> Result<String, AskError> {
        let uri = uri::parse(self.url.expose()).map_err(|_| AskError::InvalidUrl)?;
        if !uri.is_http() {
            return Err(AskError::InvalidUrl);
        }
        let host = uri.host.clone().filter(|host| !host.is_empty()).ok_or(AskError::InvalidUrl)?;
        let https = uri.scheme.as_deref().is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"));
        let port = uri.port.and_then(|port| u16::try_from(port).ok()).ok_or(AskError::InvalidUrl)?;
        let endpoint = Endpoint { https, host, port, pinned_ip: None };

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), "campfire-hermes".to_string()),
        ];
        let mut request = http::Request::net_http(hyper::Method::POST, http::request_uri(&uri), None, headers).transport(true, &endpoint);
        request.body = body;
        let timeouts = Timeouts { open: CONNECT_TIMEOUT, read: ASK_TIMEOUT };
        let response = http::exchange(&self.net, &endpoint, request, &timeouts).await.map_err(transport_error)?;
        let status = response.status;
        let body = match response.read_body(MAX_REPLY_SIZE).await.map_err(transport_error)? {
            Body::Complete(body) => body,
            Body::TooLarge => return Err(AskError::BadReply),
        };
        match status {
            200..=299 => answer(&body).ok_or(AskError::BadReply),
            // The bridge's own deadline for Hermes.
            504 => Err(AskError::Timeout),
            status => Err(AskError::Status(status)),
        }
    }
}

impl HermesAsker for HttpAsker {
    fn ask(&self, question: Question) -> BoxFuture<'_, Result<String, AskError>> {
        Box::pin(async move {
            let body = serde_json::to_vec(&question.to_json()).map_err(|_| AskError::BadReply)?;
            tokio::time::timeout(ASK_TIMEOUT, self.post(body)).await.unwrap_or(Err(AskError::Timeout))
        })
    }
}

fn transport_error(error: HttpError) -> AskError {
    match error {
        HttpError::OpenTimeout | HttpError::ReadTimeout => AskError::Timeout,
        other => AskError::Transport(other.to_string()),
    }
}

/// `{"answer": "…"}` → the (non-blank) answer.
fn answer(body: &[u8]) -> Option<String> {
    let reply: Value = serde_json::from_slice(body).ok()?;
    reply["answer"].as_str().map(str::trim).filter(|answer| !answer.is_empty()).map(str::to_string)
}

/// At most `limit` characters, cut with an ellipsis.
pub fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let kept: String = value.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::test_support::{FakeServer, Route};

    const ASK_PATH: &str = "/ask/s3cret";

    fn question() -> Question {
        Question { room_id: 7, user_name: "Zoé".into(), room_name: "Atelier".into(), question: "Qui appeler ?".into() }
    }

    fn asker_for(server: &FakeServer) -> HttpAsker {
        HttpAsker::new(Network::system(), ApiKey::new(format!("http://{}{ASK_PATH}", server.addr)))
    }

    #[tokio::test]
    async fn posts_the_question_and_reads_the_answer() {
        let reply =
            Route::new("POST", "*", ASK_PATH, 200).header("Content-Type", "application/json").body(r#"{"answer":" Appelez Marc. "}"#);
        let server = FakeServer::start(vec![reply]).await;
        assert_eq!(asker_for(&server).ask(question()).await.unwrap(), "Appelez Marc.");

        let received = &server.received()[0];
        assert_eq!((received.method.as_str(), received.target.as_str()), ("POST", ASK_PATH));
        assert_eq!(received.header("Content-Type"), Some("application/json"));
        let body: Value = serde_json::from_slice(&received.body).unwrap();
        assert_eq!(body, json!({ "room_id": 7, "user_name": "Zoé", "room_name": "Atelier", "question": "Qui appeler ?" }));
    }

    #[tokio::test]
    async fn bridge_errors_are_errors() {
        let timeout = Route::new("POST", "*", ASK_PATH, 504).body(r#"{"error":"x"}"#);
        let server = FakeServer::start(vec![timeout]).await;
        assert!(matches!(asker_for(&server).ask(question()).await, Err(AskError::Timeout)));

        let failed = Route::new("POST", "*", ASK_PATH, 502).body(r#"{"error":"x"}"#);
        let server = FakeServer::start(vec![failed]).await;
        assert!(matches!(asker_for(&server).ask(question()).await, Err(AskError::Status(502))));

        let blank = Route::new("POST", "*", ASK_PATH, 200).body(r#"{"answer":"  "}"#);
        let server = FakeServer::start(vec![blank]).await;
        assert!(matches!(asker_for(&server).ask(question()).await, Err(AskError::BadReply)));

        let wrong_path = FakeServer::start(vec![]).await; // 404
        let error = asker_for(&wrong_path).ask(question()).await.unwrap_err();
        assert!(matches!(error, AskError::Status(404)));
        assert!(!error.to_string().contains("s3cret"), "{error}");
    }

    #[test]
    fn truncates_on_characters() {
        assert_eq!(truncate_chars("abc", 3), "abc");
        assert_eq!(truncate_chars("ééééé", 3), "éé…");
        assert_eq!(truncate_chars(&"x".repeat(2000), MAX_QUESTION_CHARS).chars().count(), MAX_QUESTION_CHARS);
    }
}
