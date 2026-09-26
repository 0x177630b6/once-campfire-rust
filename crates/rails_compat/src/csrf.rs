//! `ActionController::RequestForgeryProtection` token handling (Rails main, `load_defaults` 8.2,
//! which turns on `per_form_csrf_tokens` and `forgery_protection_origin_check`).
//!
//! - The session holds the raw secret: `session["_csrf_token"]`, `SecureRandom.urlsafe_base64(32)`.
//! - `csrf_meta_tags` and forms without a known action emit a *masked global token*: the
//!   32-byte `HMAC-SHA256(raw secret, "!real_csrf_token")`, XORed with a fresh one-time pad and
//!   prefixed by the pad, then URL-safe Base64 without padding (86 characters).
//! - `form_with`/`button_to` emit *per-form tokens*: the HMAC of `"<action path>#<method>"`,
//!   masked the same way. The method is the form's real method (`patch`, `delete`, ...).
//! - A submitted token (the `authenticity_token` param or the `X-CSRF-Token` header) is valid
//!   when it decodes to 32 bytes equal to the raw secret, or to 64 bytes that unmask to the
//!   global token, the raw secret, or the per-form token for this request's path and method.
use hmac::{Hmac, Mac};
use rand::RngCore;

use crate::encoding;
use crate::message_verifier::constant_time_eq;

pub const SESSION_KEY: &str = "_csrf_token";
pub const PARAM: &str = "authenticity_token";
pub const HEADER: &str = "X-CSRF-Token";

const TOKEN_LENGTH: usize = 32;
const GLOBAL_CSRF_TOKEN_IDENTIFIER: &str = "!real_csrf_token";

/// A fresh raw CSRF secret to store in the session (`session[:_csrf_token]`).
pub fn generate_session_token() -> String {
    encoding::urlsafe_encode_unpadded(&random_bytes())
}

/// A masked global token for `csrf_meta_tags` and forms without per-form tokens.
pub fn masked_token(session_token: &str) -> String {
    mask(&global_token(&real_token(session_token)))
}

/// A masked per-form token, as `form_with(url: action, method:)` or `button_to` renders it on
/// the page at `request_path` (`action` may be absolute, a path, or relative to the page).
pub fn per_form_masked_token(session_token: &str, action: &str, method: &str, request_path: &str) -> String {
    let action_path = normalize_action_path(action, request_path);
    mask(&per_form_token(&real_token(session_token), &action_path, method))
}

/// `valid_authenticity_token?` for one submitted token, on a request to `request_path` with
/// `request_method` (after the `_method` override). Check both the param and the header.
pub fn valid_token(session_token: &str, submitted: &str, request_path: &str, request_method: &str) -> bool {
    let Some(real) = decode(session_token) else { return false };
    if submitted.is_empty() {
        return false;
    }
    let Some(token) = decode(submitted) else { return false };

    match token.len() {
        TOKEN_LENGTH => constant_time_eq(&token, &real),
        n if n == TOKEN_LENGTH * 2 => {
            let unmasked = unmask(&token);
            let path = request_path.strip_suffix('/').unwrap_or(request_path);
            constant_time_eq(&unmasked, &global_token(&real))
                || constant_time_eq(&unmasked, &real)
                || constant_time_eq(&unmasked, &per_form_token(&real, path, request_method))
        }
        _ => false,
    }
}

/// `valid_request_origin?`: no Origin header, or one equal to the request's base URL
/// (`scheme://host[:port]`, compared as strings). Rails *raises* `InvalidAuthenticityToken` for
/// `Origin: null`, which ends the same way as a failed check, so that's `false` here.
pub fn valid_request_origin(origin: Option<&str>, base_url: &str) -> bool {
    match origin {
        None => true,
        Some("null") => false,
        Some(origin) => origin == base_url,
    }
}

/// `normalize_action_path`: the path of an absolute action, or a relative one resolved against
/// the current request path, without a trailing slash.
pub fn normalize_action_path(action: &str, request_path: &str) -> String {
    let path = uri_path(action);
    if !has_scheme(action) && (action.trim().is_empty() || !action.starts_with('/')) {
        let joined = format!("{request_path}/{path}").replace("/./", "/");
        chomp_slash(&joined).to_string()
    } else {
        chomp_slash(&path).to_string()
    }
}

fn per_form_token(real: &[u8], action_path: &str, method: &str) -> Vec<u8> {
    hmac(real, &format!("{action_path}#{}", method.to_lowercase()))
}

fn global_token(real: &[u8]) -> Vec<u8> {
    hmac(real, GLOBAL_CSRF_TOKEN_IDENTIFIER)
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn mask(raw: &[u8]) -> String {
    mask_with_pad(raw, &random_bytes())
}

fn mask_with_pad(raw: &[u8], pad: &[u8]) -> String {
    let encrypted: Vec<u8> = pad.iter().zip(raw).map(|(p, r)| p ^ r).collect();
    encoding::urlsafe_encode_unpadded(&[pad, &encrypted].concat())
}

fn unmask(masked: &[u8]) -> Vec<u8> {
    let (pad, encrypted) = masked.split_at(TOKEN_LENGTH);
    pad.iter().zip(encrypted).map(|(p, e)| p ^ e).collect()
}

/// The decoded session secret. Rails raises on a session token that isn't Base64 (only Rails or
/// we can write the encrypted session, so it can't happen); a random key here yields tokens that
/// never validate, which is where that request would end up anyway.
fn real_token(session_token: &str) -> Vec<u8> {
    decode(session_token).unwrap_or_else(|| random_bytes().to_vec())
}

fn decode(token: &str) -> Option<Vec<u8>> {
    encoding::urlsafe_decode(token)
}

fn random_bytes() -> [u8; TOKEN_LENGTH] {
    let mut bytes = [0u8; TOKEN_LENGTH];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn chomp_slash(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

fn has_scheme(uri: &str) -> bool {
    match uri.find(':') {
        Some(colon) => {
            let scheme = &uri[..colon];
            scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

/// `URI.parse(uri).path`: drops the scheme and authority, the query and the fragment.
fn uri_path(uri: &str) -> String {
    let uri = uri.split(['?', '#']).next().unwrap_or("");
    let rest = if has_scheme(uri) { &uri[uri.find(':').unwrap() + 1..] } else { uri };
    match rest.strip_prefix("//") {
        Some(authority_and_path) => authority_and_path.find('/').map_or(String::new(), |i| authority_and_path[i..].to_string()),
        None => rest.to_string(),
    }
}

#[cfg(test)]
pub(crate) fn mask_with_pad_for_test(raw: &[u8], pad: &[u8]) -> String {
    mask_with_pad(raw, pad)
}
