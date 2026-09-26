//! `ActionController::RequestForgeryProtection` token handling.

/// A fresh raw CSRF secret to store in the session (`session[:_csrf_token]`).
pub fn generate_session_token() -> String { todo!() }
/// A masked token for `csrf_meta_tags` / hidden `authenticity_token` fields (fresh mask per call).
pub fn masked_token(session_token: &str) -> String { let _ = session_token; todo!() }
/// Rails' `valid_authenticity_token?` for a token submitted via param or `X-CSRF-Token`.
pub fn valid_token(session_token: &str, submitted: &str) -> bool { let _ = (session_token, submitted); todo!() }
