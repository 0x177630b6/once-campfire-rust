//! The signing, encryption and CSRF-token math the HTTP layer needs, behind a trait.
//!
//! Production uses [`RailsCrypto`], which calls into `rails_compat` (verified against golden
//! vectors from the reference app). Tests use [`crate::testing::TestCrypto`], a transparent,
//! insecure stand-in with the same shape, so this crate can be exercised independently of the
//! vectors.

use std::sync::Arc;

use jiff::Timestamp;
use rails_compat::Secrets;
use serde_json::Value;

pub trait Crypto: Send + Sync {
    /// The value for `cookies.signed[name] = value`.
    fn sign_cookie(&self, name: &str, value: &str, expires_at: Option<Timestamp>) -> String;
    /// `cookies.signed[name]`; `None` for anything Rails would read as nil.
    fn verify_signed_cookie(&self, name: &str, raw: &str, now: Timestamp) -> Option<String>;
    /// The value for `cookies.encrypted[name] = value` (the session store's jar).
    fn encrypt_cookie(&self, name: &str, value: &Value, expires_at: Option<Timestamp>) -> String;
    fn decrypt_cookie(&self, name: &str, raw: &str, now: Timestamp) -> Option<Value>;

    /// A fresh raw CSRF secret for `session[:_csrf_token]`.
    fn generate_csrf_token(&self) -> String;
    /// `form_authenticity_token` / `csrf_meta_tags`: a freshly masked global token.
    fn masked_csrf_token(&self, session_token: &str) -> String;
    /// `form_authenticity_token(form_options: { action:, method: })` with per-form tokens on, for
    /// a form rendered on the page at `request_path` (`action` may be relative to it).
    fn per_form_masked_csrf_token(&self, session_token: &str, action: &str, method: &str, request_path: &str) -> String;
    /// `valid_authenticity_token?`, including per-form tokens for the request's path and method.
    fn valid_csrf_token(&self, session_token: &str, submitted: &str, request_path: &str, request_method: &str) -> bool;
}

pub type SharedCrypto = Arc<dyn Crypto>;

/// Rails-compatible crypto derived from `secret_key_base`.
pub struct RailsCrypto {
    secrets: Arc<Secrets>,
}

impl RailsCrypto {
    pub fn new(secrets: Arc<Secrets>) -> Self {
        Self { secrets }
    }
}

impl Crypto for RailsCrypto {
    fn sign_cookie(&self, name: &str, value: &str, expires_at: Option<Timestamp>) -> String {
        rails_compat::cookies::sign(&self.secrets, name, value, expires_at)
    }

    fn verify_signed_cookie(&self, name: &str, raw: &str, now: Timestamp) -> Option<String> {
        rails_compat::cookies::verify_signed(&self.secrets, name, raw, now)
    }

    fn encrypt_cookie(&self, name: &str, value: &Value, expires_at: Option<Timestamp>) -> String {
        rails_compat::cookies::encrypt(&self.secrets, name, value, expires_at)
    }

    fn decrypt_cookie(&self, name: &str, raw: &str, now: Timestamp) -> Option<Value> {
        rails_compat::cookies::decrypt(&self.secrets, name, raw, now)
    }

    fn generate_csrf_token(&self) -> String {
        rails_compat::csrf::generate_session_token()
    }

    fn masked_csrf_token(&self, session_token: &str) -> String {
        rails_compat::csrf::masked_token(session_token)
    }

    fn per_form_masked_csrf_token(&self, session_token: &str, action: &str, method: &str, request_path: &str) -> String {
        rails_compat::csrf::per_form_masked_token(session_token, action, method, request_path)
    }

    fn valid_csrf_token(&self, session_token: &str, submitted: &str, request_path: &str, request_method: &str) -> bool {
        rails_compat::csrf::valid_token(session_token, submitted, request_path, request_method)
    }
}
