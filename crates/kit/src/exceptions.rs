//! `ActionDispatch::PublicExceptions`: what an error looks like on the wire in production.
//!
//! JSON requests get `{"status":404,"error":"Not Found"}`; everything else gets
//! `public/<status>.html`, or an empty body with the status when that file doesn't exist.

use axum::http::StatusCode;

use crate::app::KitConfig;
use crate::format::{self, Format};
use crate::response::Response;

pub fn render(config: &KitConfig, status: StatusCode, format: Option<Format>, head: bool) -> Response {
    let content_type = format.filter(|f| **f != format::ALL);
    if head {
        let ct = content_type.map(|f| f.string).unwrap_or("text/html");
        return Response::with_body(status, &format!("{ct}; charset=utf-8"), "");
    }
    if content_type == Some(&format::JSON) {
        let body = format!(r#"{{"status":{},"error":"{}"}}"#, status.as_u16(), reason(status));
        return Response::with_body(status, "application/json; charset=utf-8", body);
    }
    let page = config.public_path.as_ref().and_then(|dir| std::fs::read(dir.join(format!("{}.html", status.as_u16()))).ok());
    match page {
        Some(html) => Response::with_body(status, "text/html; charset=utf-8", html),
        // PublicExceptions answers X-Cascade: pass; ShowExceptions#pass_response turns that into
        // an empty page with the error's status.
        None => Response::with_body(status, "text/html; charset=utf-8", ""),
    }
}

/// `Rack::Utils::HTTP_STATUS_CODES`.
fn reason(status: StatusCode) -> &'static str {
    match status.as_u16() {
        422 => "Unprocessable Content",
        413 => "Content Too Large",
        _ => status.canonical_reason().unwrap_or("Internal Server Error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_errors() {
        let response = render(&KitConfig::default(), StatusCode::UNPROCESSABLE_ENTITY, Some(&format::JSON), false);
        assert_eq!(response.body_bytes().unwrap().as_ref(), br#"{"status":422,"error":"Unprocessable Content"}"#);
    }

    #[test]
    fn html_pages_from_public() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("404.html"), "<h1>404</h1>").unwrap();
        let config = KitConfig { public_path: Some(dir.path().into()), ..KitConfig::default() };
        let response = render(&config, StatusCode::NOT_FOUND, Some(&format::HTML), false);
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert_eq!(response.body_bytes().unwrap().as_ref(), b"<h1>404</h1>");
        let missing = render(&config, StatusCode::BAD_REQUEST, None, false);
        assert_eq!(missing.status, StatusCode::BAD_REQUEST);
        assert!(missing.body_bytes().unwrap().is_empty());
    }
}
