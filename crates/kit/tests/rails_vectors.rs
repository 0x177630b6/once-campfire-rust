//! Old-browser-tab continuity: a session cookie, CSRF tokens and a signed cookie issued by the
//! reference Rails app (`vectors/rails_compat.json`) are accepted by the kit with real
//! `RailsCrypto`, and what the kit writes back decodes to the same session.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use campfire_kit::{Ctx, FrozenClock, Kit, KitConfig, RailsCrypto, Result, StatusCode, action};
use serde_json::{Value, json};
use tower::ServiceExt;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../vectors/rails_compat.json")).unwrap()
}

async fn create_session(c: &mut Ctx) -> Result {
    c.verify_authenticity_token()?;
    let session = c.session();
    let body = json!({ "id": session.id(), "csrf": session.get("_csrf_token") });
    c.json(StatusCode::OK, &body)
}

async fn whoami(c: &mut Ctx) -> Result {
    let token = c.cookies.signed("session_token");
    c.json(StatusCode::OK, &json!({ "token": token }))
}

fn app(vectors: &Value) -> (Router, Arc<rails_compat::Secrets>) {
    let secrets = Arc::new(rails_compat::Secrets::new(vectors["secret_key_base"].as_str().unwrap()));
    let now = vectors["now"].as_str().unwrap().parse().unwrap();
    let kit = Kit::new(KitConfig::default(), Arc::new(RailsCrypto::new(secrets.clone())), Arc::new(FrozenClock::new(now)), ());
    let router = Router::new().route("/session", action_post()).route("/whoami", campfire_kit::get(whoami));
    (campfire_kit::app(router, kit), secrets)
}

fn action_post() -> axum::routing::MethodRouter<Kit> {
    axum::routing::post(action(create_session))
}

async fn post_session(app: &Router, cookie: &str, token: Option<(&str, &str)>, origin: Option<&str>) -> axum::response::Response {
    let mut request = Request::post("/session").header(header::HOST, "localhost:3000").header(header::COOKIE, cookie);
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    let body = match token {
        Some(("param", token)) => {
            request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            format!("authenticity_token={}", campfire_kit::cookies::escape(token))
        }
        Some((_, token)) => {
            request = request.header("x-csrf-token", token);
            String::new()
        }
        None => String::new(),
    };
    app.clone().oneshot(request.body(Body::from(body)).unwrap()).await.unwrap()
}

#[tokio::test]
async fn rails_session_and_csrf_tokens_are_accepted() {
    let vectors = vectors();
    let session = &vectors["session"];
    let (app, secrets) = app(&vectors);
    let cookie = format!("_campfire_session={}", campfire_kit::cookies::escape(session["session_cookie_raw"].as_str().unwrap()));

    let with_form_token = post_session(&app, &cookie, Some(("param", session["session_form_token"].as_str().unwrap())), None).await;
    assert_eq!(with_form_token.status(), StatusCode::OK);
    let set_cookie = with_form_token.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    let body = axum::body::to_bytes(with_form_token.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["id"], session["session"]["session_id"]);
    assert_eq!(body["csrf"], session["session"]["_csrf_token"]);

    // What we write back is the same session, readable by Rails' cookie format.
    let raw = set_cookie.strip_prefix("_campfire_session=").unwrap().split(';').next().unwrap();
    let raw = rails_compat::cookies::unescape(raw);
    let now = vectors["now"].as_str().unwrap().parse().unwrap();
    let decoded = rails_compat::cookies::decrypt(&secrets, "_campfire_session", &raw, now).unwrap();
    assert_eq!(decoded, session["session"]);
    assert!(set_cookie.ends_with("; path=/; expires=Mon, 01 Jan 2046 12:00:00 GMT; httponly; samesite=lax"));

    let with_meta_header = post_session(&app, &cookie, Some(("header", session["csrf_meta_token"].as_str().unwrap())), None).await;
    assert_eq!(with_meta_header.status(), StatusCode::OK);

    let bad = post_session(&app, &cookie, Some(("param", "bogus")), None).await;
    assert_eq!(bad.status().as_u16(), session["post_with_bad_token_status"].as_u64().unwrap() as u16);

    let cross_origin =
        post_session(&app, &cookie, Some(("param", session["session_form_token"].as_str().unwrap())), Some("https://evil.example")).await;
    assert_eq!(cross_origin.status().as_u16(), session["post_with_cross_origin_status"].as_u64().unwrap() as u16);
}

#[tokio::test]
async fn rails_signed_session_token_cookie_is_read() {
    let vectors = vectors();
    let session = &vectors["session"];
    let (app, _) = app(&vectors);
    let cookie = format!("session_token={}", campfire_kit::cookies::escape(session["session_token_raw"].as_str().unwrap()));
    let request = Request::get("/whoami").header(header::COOKIE, cookie).body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["token"], session["session_token_value"]);
}
