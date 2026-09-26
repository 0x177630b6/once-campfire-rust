//! `cookies.signed[...]` / `cookies.encrypted[...]` value encoding (the cookie *value* only;
//! attributes like HttpOnly/SameSite/expires are the HTTP layer's job).
use crate::Secrets;

/// Encode `value` as Rails would for `cookies.signed[name] = value`.
pub fn sign(secrets: &Secrets, name: &str, value: &str) -> String { let _ = (secrets, name, value); todo!() }
/// Decode a `cookies.signed[name]` value; `None` on any failure (Rails returns nil).
pub fn verify_signed(secrets: &Secrets, name: &str, raw: &str, now: jiff::Timestamp) -> Option<String> { let _ = (secrets, name, raw, now); todo!() }

/// Encode for `cookies.encrypted[name]` (the Rails session cookie uses this with a JSON hash).
pub fn encrypt(secrets: &Secrets, name: &str, value: &serde_json::Value, expires_at: Option<jiff::Timestamp>) -> String { let _ = (secrets, name, value, expires_at); todo!() }
pub fn decrypt(secrets: &Secrets, name: &str, raw: &str, now: jiff::Timestamp) -> Option<serde_json::Value> { let _ = (secrets, name, raw, now); todo!() }
