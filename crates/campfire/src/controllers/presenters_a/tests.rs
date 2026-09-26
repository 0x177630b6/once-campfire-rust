//! Request-level tests for the session, account and user controllers (controllers A), through the
//! whole stack (`app::boot`, the Rails route table, kit) over a private copy of the reference-built
//! `default` parity seed. Skipped (with a note) when the seed hasn't been built
//! (`parity/bin/seed build default`). Parity against the running reference lives in
//! `reference-tools/campfire/controllers_a/replay.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::app::{Booted, boot};
use crate::config::Config;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const HOST: &str = "campfire.test";
const PASSWORD: &str = "secret123456";
const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

struct Test {
    booted: Booted,
    labels: serde_json::Value,
    _dir: tempfile::TempDir,
}

async fn boot_seed(name: &str) -> Option<Test> {
    let seed = Path::new(ROOT).join("parity/.seed").join(name);
    if !seed.join("db/production.sqlite3").exists() {
        eprintln!("skipping: parity/.seed/{name} isn't built (parity/bin/seed build {name})");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("db")).unwrap();
    std::fs::copy(seed.join("db/production.sqlite3"), dir.path().join("db/production.sqlite3")).unwrap();
    if seed.join("storage").exists() {
        copy_dir(&seed.join("storage"), &dir.path().join("files"));
    }
    let labels = std::fs::read_to_string(seed.join("labels.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let root = dir.path().to_string_lossy().into_owned();
    let secret = parity_env("SECRET_KEY_BASE").unwrap();
    let config = Config::from_lookup(|key| match key {
        "SECRET_KEY_BASE" => Some(secret.clone()),
        "DISABLE_SSL" => Some("true".into()),
        "APP_VERSION" | "GIT_REVISION" => Some("parity".into()),
        "CAMPFIRE_STORAGE_PATH" => Some(root.clone()),
        _ => None,
    })
    .unwrap();
    Some(Test { booted: boot(config).await.unwrap(), labels, _dir: dir })
}

fn parity_env(name: &str) -> Option<String> {
    let env = std::fs::read_to_string(Path::new(ROOT).join("parity/.env.reference")).ok()?;
    env.lines().find_map(|line| line.strip_prefix(&format!("{name}=")).map(str::to_string))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target: PathBuf = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl Test {
    fn label(&self, key: &str) -> String {
        match &self.labels[key] {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// A browser: a cookie jar and a remote IP of its own (the sign-in rate limit is per IP).
    fn browser(&self, ip: &str) -> Browser<'_> {
        Browser { test: self, cookies: BTreeMap::new(), ip: ip.to_string() }
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn location(&self) -> &str {
        self.header("location").unwrap_or("")
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn set_cookies(&self) -> Vec<String> {
        self.headers.get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap().to_string()).collect()
    }

    /// The global token from `csrf_meta_tags`.
    fn csrf_meta_token(&self) -> String {
        let html = self.text();
        let at = html.find("name=\"csrf-token\" content=\"").expect("csrf meta tag") + "name=\"csrf-token\" content=\"".len();
        html[at..].split('"').next().unwrap().to_string()
    }

    /// The per-form token of the `button_to` form for `action` that sends `method`.
    fn button_token(&self, action: &str, method: &str) -> String {
        let html = self.text();
        for form in html.split("<form").skip(1) {
            let form = &form[..form.find("</form>").unwrap_or(form.len())];
            let method_field = format!("name=\"_method\" value=\"{method}\"");
            if form.contains(&format!("action=\"{action}\"")) && form.contains(&method_field) {
                let at = form.find("name=\"authenticity_token\" value=\"").unwrap() + "name=\"authenticity_token\" value=\"".len();
                return form[at..].split('"').next().unwrap().to_string();
            }
        }
        panic!("no {method} button for {action}")
    }

    /// The per-form `authenticity_token` of the form posting to `action`.
    fn form_token(&self, action: &str) -> String {
        let html = self.text();
        let start = html
            .find(&format!("action=\"{action}\""))
            .or_else(|| html.find(&format!("action=\"http://{HOST}{action}\"")))
            .unwrap_or_else(|| panic!("no form for {action} in {html}"));
        let rest = &html[start..];
        let at = rest.find("name=\"authenticity_token\" value=\"").expect("token") + "name=\"authenticity_token\" value=\"".len();
        rest[at..rest[at..].find('"').unwrap() + at].to_string()
    }
}

struct Browser<'a> {
    test: &'a Test,
    cookies: BTreeMap<String, String>,
    ip: String,
}

impl Browser<'_> {
    async fn request(&mut self, method: Method, path: &str, headers: &[(&str, &str)], body: Option<(&str, String)>) -> Reply {
        let mut request = Request::builder().method(method).uri(path).header(header::HOST, HOST).header(header::USER_AGENT, CHROME).header("x-forwarded-for", &self.ip);
        if !self.cookies.is_empty() {
            let cookie = self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
            request = request.header(header::COOKIE, cookie);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = match body {
            Some((content_type, body)) => request.header(header::CONTENT_TYPE, content_type).body(Body::from(body)).unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = self.test.booted.router.clone().oneshot(request).await.unwrap();
        let reply = Reply {
            status: response.status(),
            headers: response.headers().clone(),
            body: axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec(),
        };
        for cookie in reply.set_cookies() {
            let pair = cookie.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            let deleted = cookie.to_ascii_lowercase().contains("max-age=0") || cookie.contains("1970");
            if deleted || value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_string(), value.to_string());
            }
        }
        reply
    }

    async fn get(&mut self, path: &str) -> Reply {
        self.request(Method::GET, path, &[], None).await
    }

    async fn form(&mut self, method: &str, path: &str, token: &str, fields: &[(&str, &str)]) -> Reply {
        let mut pairs = vec![("authenticity_token".to_string(), token.to_string())];
        if method != "post" {
            pairs.push(("_method".into(), method.into()));
        }
        pairs.extend(fields.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let body = pairs.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&");
        self.request(Method::POST, path, &[], Some(("application/x-www-form-urlencoded", body))).await
    }

    async fn sign_in(&mut self, email: &str) {
        let page = self.get("/session/new").await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.text());
        let token = page.form_token("/session");
        let reply = self.form("post", "/session", &token, &[("email_address", email), ("password", PASSWORD)]).await;
        assert_eq!(reply.status, StatusCode::FOUND, "sign in as {email}: {}", reply.text());
    }
}

fn encode(value: &str) -> String {
    super::cgi_escape(value)
}

fn assert_redirect(reply: &Reply, location: &str) {
    assert_eq!((reply.status, reply.location()), (StatusCode::FOUND, location), "{}", reply.text());
}

// --- Sessions ------------------------------------------------------------------------------------

#[tokio::test]
async fn signs_in_with_a_password_and_out_again() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.1");

    // Unauthenticated requests remember where they were going.
    assert_redirect(&browser.get("/account/edit").await, "http://campfire.test/session/new");

    let page = browser.get("/session/new").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("<title>Sign in</title>"));
    assert!(page.header("link").is_some_and(|link| link.contains("rel=preload; as=style")));
    let token = page.form_token("/session");
    let signed_in = browser.form("post", "/session", &token, &[("email_address", &test.label("emails.david")), ("password", PASSWORD)]).await;
    assert_redirect(&signed_in, "http://campfire.test/account/edit");
    let session_cookie = signed_in.set_cookies().into_iter().find(|c| c.starts_with("session_token=")).expect("session cookie");
    assert!(session_cookie.contains("httponly") && session_cookie.contains("samesite=lax") && session_cookie.contains("expires="), "{session_cookie}");

    // Signed in: sign-in and join pages send you home.
    assert_redirect(&browser.get(&format!("/join/{}", test.label("join_codes.signal"))).await, "http://campfire.test/");
    let root = browser.get("/").await;
    assert_eq!(root.status, StatusCode::FOUND);
    assert!(root.location().starts_with("http://campfire.test/rooms/"));

    // Sign out from the profile page's form.
    let profile = browser.get("/users/me/profile").await;
    assert_eq!(profile.status, StatusCode::OK, "{}", profile.text());
    let token = profile.form_token("/session");
    let signed_out = browser.form("delete", "/session", &token, &[]).await;
    assert_redirect(&signed_out, "http://campfire.test/");
    assert!(signed_out.set_cookies().iter().any(|c| c.starts_with("session_token=;")), "{:?}", signed_out.set_cookies());
    assert_redirect(&browser.get("/users/me/profile").await, "http://campfire.test/session/new");
}

#[tokio::test]
async fn rejects_bad_passwords_and_rate_limits_sign_ins() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.2");
    let token = browser.get("/session/new").await.form_token("/session");
    for attempt in 1..=11 {
        let reply = browser.form("post", "/session", &token, &[("email_address", "david@37signals.com"), ("password", "wrong")]).await;
        let expected = if attempt <= 10 { StatusCode::UNAUTHORIZED } else { StatusCode::TOO_MANY_REQUESTS };
        assert_eq!(reply.status, expected, "attempt {attempt}");
        let html = reply.text();
        assert!(html.contains("Too many requests or unauthorized.") && html.contains("shake"), "{html}");
        assert!(html.contains(r#"value="david@37signals.com""#));
    }
    // Deactivated users can't sign in.
    let mut other = test.browser("198.51.100.3");
    let token = other.get("/session/new").await.form_token("/session");
    let reply = other.form("post", "/session", &token, &[("email_address", &test.label("emails.rita")), ("password", PASSWORD)]).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_rails_issued_session_cookie_continues_on_rust() {
    let Some(test) = boot_seed("default").await else { return };
    let vectors: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vectors/campfire_sessions.json"))).unwrap();
    let cookie = vectors["sessions"][0]["cookie_header"].as_str().unwrap();
    let mut browser = test.browser("198.51.100.4");
    for pair in cookie.split("; ") {
        let (name, value) = pair.split_once('=').unwrap();
        browser.cookies.insert(name.into(), value.into());
    }
    let profile = browser.get("/users/me/profile").await;
    assert_eq!(profile.status, StatusCode::OK);
    assert!(profile.text().contains("David"));
}

#[tokio::test]
async fn transfers_sign_in_on_another_device() {
    let Some(test) = boot_seed("default").await else { return };
    let mut admin = test.browser("198.51.100.5");
    admin.sign_in(&test.label("emails.david")).await;
    let kevin_id = test.label("users.kevin");
    let page = admin.get(&format!("/users/{kevin_id}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    let html = page.text();
    let at = html.find("/session/transfers/").expect("transfer link") + "/session/transfers/".len();
    let transfer_id = html[at..].split('"').next().unwrap().to_string();

    let mut phone = test.browser("198.51.100.6");
    let path = format!("/session/transfers/{transfer_id}");
    let show = phone.get(&path).await;
    assert_eq!(show.status, StatusCode::OK);
    let token = show.form_token(&path);
    assert_redirect(&phone.form("put", &path, &token, &[]).await, "http://campfire.test/");
    assert_eq!(phone.get("/users/me/profile").await.status, StatusCode::OK);

    let mut stranger = test.browser("198.51.100.7");
    let token = stranger.get(&path).await.form_token(&path);
    let bogus = "/session/transfers/bogus";
    let reply = stranger.form("put", bogus, &token, &[]).await;
    // The token was minted for another action, so CSRF fails first; with the right one it's a 400.
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    let token = stranger.get(bogus).await.form_token(bogus);
    assert_eq!(stranger.form("put", bogus, &token, &[]).await.status, StatusCode::BAD_REQUEST);
}

// --- Joining and first run -------------------------------------------------------------------------

#[tokio::test]
async fn joins_with_the_join_code() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.8");
    assert_eq!(browser.get("/join/nope").await.status, StatusCode::NOT_FOUND);
    let path = format!("/join/{}", test.label("join_codes.signal"));
    let page = browser.get(&path).await;
    assert_eq!(page.status, StatusCode::OK);
    let token = page.form_token(&path);
    let fields = [("user[name]", "New Person"), ("user[email_address]", "new@example.com"), ("user[password]", PASSWORD)];
    assert_redirect(&browser.form("post", &path, &token, &fields).await, "http://campfire.test/");
    assert_eq!(browser.get("/users/me/profile").await.status, StatusCode::OK);

    // A taken email address goes to sign in instead.
    let mut other = test.browser("198.51.100.9");
    let token = other.get(&path).await.form_token(&path);
    let fields = [("user[name]", "Imposter"), ("user[email_address]", "new@example.com"), ("user[password]", PASSWORD)];
    assert_redirect(&other.form("post", &path, &token, &fields).await, "http://campfire.test/session/new?email_address=new%40example.com");
}

#[tokio::test]
async fn first_run_sets_up_the_account() {
    let Some(test) = boot_seed("first_run").await else { return };
    let mut browser = test.browser("198.51.100.10");
    assert_redirect(&browser.get("/session/new").await, "http://campfire.test/first_run");
    let page = browser.get("/first_run").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    let token = page.form_token("/first_run");
    let fields = [("user[name]", "Owner"), ("user[email_address]", "owner@example.com"), ("user[password]", PASSWORD)];
    assert_redirect(&browser.form("post", "/first_run", &token, &fields).await, "http://campfire.test/");
    assert_redirect(&browser.get("/first_run").await, "http://campfire.test/");
    assert!(browser.get("/").await.location().starts_with("http://campfire.test/rooms/"));
}

// --- Account ---------------------------------------------------------------------------------------

#[tokio::test]
async fn administers_the_account() {
    let Some(test) = boot_seed("default").await else { return };
    let mut admin = test.browser("198.51.100.11");
    admin.sign_in(&test.label("emails.david")).await;
    let account_id = test.label("accounts.signal");

    let edit = admin.get("/account/edit").await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.text());
    let action = format!("/account.{account_id}");
    let token = edit.form_token(&action);
    let updated = admin.form("patch", &action, &token, &[("account[name]", "Renamed")]).await;
    assert_redirect(&updated, "http://campfire.test/account/edit");
    let edit = admin.get("/account/edit").await;
    assert!(edit.text().contains("Renamed"));
    assert!(edit.text().contains("flash"), "the ✓ notice shows once");

    // Join code reset.
    let token = edit.form_token("/account/join_code");
    assert_redirect(&admin.form("post", "/account/join_code", &token, &[]).await, "http://campfire.test/account/edit");
    assert!(!admin.get("/account/edit").await.text().contains(&test.label("join_codes.signal")));

    // Custom styles.
    let page = admin.get("/account/custom_styles/edit").await;
    assert_eq!(page.status, StatusCode::OK);
    let token = page.form_token("/account/custom_styles");
    let reply = admin.form("patch", "/account/custom_styles", &token, &[("account[custom_styles]", "body { --x: 1 }")]).await;
    assert_redirect(&reply, "http://campfire.test/account/custom_styles/edit");
    assert!(admin.get("/account/custom_styles/edit").await.text().contains("<style data-turbo-track=\"reload\">body { --x: 1 }</style>"));

    // The next page of people, as a turbo stream.
    let page = admin.request(Method::GET, "/account/users?page=2", &[("accept", "text/vnd.turbo-stream.html")], None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.header("content-type").unwrap().starts_with("text/vnd.turbo-stream.html"));
    assert_eq!(admin.get("/account/users").await.status, StatusCode::NOT_ACCEPTABLE);

    // Members can see the account but not change it.
    let mut member = test.browser("198.51.100.12");
    member.sign_in(&test.label("emails.kevin")).await;
    let edit = member.get("/account/edit").await;
    assert_eq!(edit.status, StatusCode::OK);
    assert!(!edit.text().contains(&format!("action=\"{action}\"")), "members get no account form");
    let token = edit.csrf_meta_token();
    assert_eq!(member.form("patch", &action, &token, &[("account[name]", "Mine")]).await.status, StatusCode::FORBIDDEN);
    assert_eq!(member.get("/account/bots").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn manages_bots() {
    let Some(test) = boot_seed("default").await else { return };
    let mut admin = test.browser("198.51.100.13");
    admin.sign_in(&test.label("emails.david")).await;
    let index = admin.get("/account/bots").await;
    assert_eq!(index.status, StatusCode::OK);
    assert!(index.text().contains(&test.label("bot_keys.bender")));

    let new = admin.get("/account/bots/new").await;
    let token = new.form_token("/account/bots");
    let reply = admin.form("post", "/account/bots", &token, &[("user[name]", "Robo"), ("user[webhook_url]", "https://example.com/robo")]).await;
    assert_redirect(&reply, "http://campfire.test/account/bots");
    assert!(admin.get("/account/bots").await.text().contains("Robo"));

    let bender = test.label("users.bender");
    let edit = admin.get(&format!("/account/bots/{bender}/edit")).await;
    assert_eq!(edit.status, StatusCode::OK);
    let action = format!("/account/bots/{bender}");
    let token = edit.form_token(&action);
    assert_redirect(&admin.form("patch", &action, &token, &[("user[name]", "Bender 2")]).await, "http://campfire.test/account/bots");

    let edit = admin.get(&format!("/account/bots/{bender}/edit")).await;
    let key_action = format!("/account/bots/{bender}/key");
    let token = edit.button_token(&key_action, "patch");
    assert_redirect(&admin.form("patch", &key_action, &token, &[]).await, "http://campfire.test/account/bots");
    assert!(!admin.get("/account/bots").await.text().contains(&test.label("bot_keys.bender")));

    let token = admin.get(&format!("/account/bots/{bender}/edit")).await.button_token(&action, "delete");
    assert_redirect(&admin.form("delete", &action, &token, &[]).await, "http://campfire.test/account/bots");
    assert_eq!(admin.get(&format!("/account/bots/{bender}/edit")).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serves_the_account_logo_and_avatars() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.14");
    let logo = browser.get("/account/logo?size=small").await;
    assert_eq!(logo.status, StatusCode::OK);
    assert_eq!(logo.header("content-type"), Some("image/png"));
    assert_eq!(logo.header("cache-control"), Some("max-age=300, public, stale-while-revalidate=604800"));
    let etag = logo.header("etag").unwrap().to_string();
    let again = browser.request(Method::GET, "/account/logo?size=small", &[("if-none-match", &etag)], None).await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);

    // Avatars need a session.
    let david_token = test.label("avatar_tokens.david");
    assert_eq!(browser.get(&format!("/users/{david_token}/avatar")).await.status, StatusCode::FOUND);
    browser.sign_in(&test.label("emails.kevin")).await;
    let avatar = browser.get(&format!("/users/{david_token}/avatar")).await;
    assert_eq!(avatar.status, StatusCode::OK);
    assert_eq!(avatar.header("content-type"), Some("image/svg+xml; charset=utf-8"));
    assert!(avatar.text().contains("\n      D\n    </text>"), "{}", avatar.text());
    assert_eq!(avatar.header("cache-control"), Some("max-age=1800, public, stale-while-revalidate=604800"));
    let jason = browser.get(&format!("/users/{}/avatar", test.label("avatar_tokens.jason"))).await;
    assert_eq!((jason.status, jason.header("content-type")), (StatusCode::OK, Some("image/webp")));
    let bad = browser.get("/users/bogus/avatar").await;
    assert_eq!((bad.status, bad.body.len()), (StatusCode::NOT_FOUND, 0));
}

// --- Users -----------------------------------------------------------------------------------------

#[tokio::test]
async fn profile_sidebar_and_user_pages() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.15");
    browser.sign_in(&test.label("emails.kevin")).await;

    let profile = browser.get("/users/me/profile").await;
    assert_eq!(profile.status, StatusCode::OK);
    let token = profile.form_token("/users/me/profile");
    let reply = browser.form("patch", "/users/me/profile", &token, &[("user[name]", "Kev"), ("user[bio]", "Hi")]).await;
    assert_redirect(&reply, "http://campfire.test/users/me/profile");
    assert!(browser.get("/users/me/profile").await.text().contains("Kev"));

    let sidebar = browser.get("/users/me/sidebar").await;
    assert_eq!(sidebar.status, StatusCode::OK);
    assert!(sidebar.text().contains("<!DOCTYPE html>"));
    let frame = browser.request(Method::GET, "/users/me/sidebar", &[("turbo-frame", "user_sidebar")], None).await;
    assert_eq!(frame.status, StatusCode::OK);
    assert!(!frame.text().contains("<!DOCTYPE html>") && frame.text().contains("<turbo-frame"));

    assert_eq!(browser.get(&format!("/users/{}", test.label("users.david"))).await.status, StatusCode::OK);
    assert_eq!(browser.get("/users/999999999").await.status, StatusCode::NOT_FOUND);

    let subscriptions = browser.get("/users/me/push_subscriptions").await;
    assert_eq!(subscriptions.status, StatusCode::OK);
    let body = r#"{"push_subscription":{"endpoint":"http://example.com/push","p256dh_key":"a","auth_key":"b"}}"#;
    let global = subscriptions.csrf_meta_token();
    let reply = browser
        .request(Method::POST, "/users/me/push_subscriptions", &[("x-csrf-token", &global)], Some(("application/json", body.into())))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "an http endpoint fails validation");
}

#[tokio::test]
async fn bans_and_unbans() {
    let Some(test) = boot_seed("default").await else { return };
    let mut admin = test.browser("198.51.100.16");
    admin.sign_in(&test.label("emails.david")).await;
    let jz = test.label("users.jz");
    let page = admin.get(&format!("/users/{jz}")).await;
    let action = format!("/users/{jz}/ban");
    let token = page.form_token(&action);
    assert_redirect(&admin.form("post", &action, &token, &[]).await, &format!("http://campfire.test/users/{jz}"));
    let page = admin.get(&format!("/users/{jz}")).await;
    let token = page.button_token(&action, "delete");
    assert_redirect(&admin.form("delete", &action, &token, &[]).await, &format!("http://campfire.test/users/{jz}"));
}

#[tokio::test]
async fn autocompletes_users() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.17");
    browser.sign_in(&test.label("emails.david")).await;
    let html = browser.get("/autocompletable/users?filter=a").await;
    assert_eq!(html.status, StatusCode::OK);
    assert!(html.text().contains("<lexxy-prompt-item") && !html.text().contains("<!DOCTYPE"));
    let json = browser.get("/autocompletable/users.json?query=a").await;
    assert_eq!(json.header("content-type"), Some("application/json; charset=utf-8"));
    assert!(json.header("x-total-count").is_some());
    let users: serde_json::Value = serde_json::from_slice(&json.body).unwrap();
    assert!(users.as_array().unwrap().iter().all(|user| user["avatar_url"].as_str().unwrap().starts_with("http://campfire.test/users/")));
    assert_eq!(browser.get("/autocompletable/users?room_id=999999").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn qr_codes_and_the_pwa() {
    let Some(test) = boot_seed("default").await else { return };
    let mut browser = test.browser("198.51.100.18");
    let qr = browser.get("/qr_code/aHR0cDovL2NhbXBmaXJlLnRlc3Q").await;
    assert_eq!((qr.status, qr.header("content-type")), (StatusCode::OK, Some("image/svg+xml; charset=utf-8")));
    assert_eq!(qr.header("cache-control"), Some("max-age=31556952, public"));
    assert!(qr.text().starts_with("<?xml version=\"1.0\" standalone=\"yes\"?><svg"));

    let manifest = browser.get("/webmanifest.json").await;
    assert_eq!((manifest.status, manifest.header("content-type")), (StatusCode::OK, Some("application/json; charset=utf-8")));
    let worker = browser.get("/service-worker.js").await;
    assert_eq!((worker.status, worker.header("content-type")), (StatusCode::OK, Some("text/javascript; charset=utf-8")));
}
