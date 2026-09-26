//! `ActiveRecord::SignedId` (`signed_id(purpose:, expires_in:)` / `find_signed`).
use crate::Secrets;

/// `model_name` is the Rails class name, e.g. "User".
pub fn generate(secrets: &Secrets, model_name: &str, id: i64, purpose: Option<&str>, expires_at: Option<jiff::Timestamp>) -> String { let _ = (secrets, model_name, id, purpose, expires_at); todo!() }
pub fn verify(secrets: &Secrets, model_name: &str, signed_id: &str, purpose: Option<&str>, now: jiff::Timestamp) -> Option<i64> { let _ = (secrets, model_name, signed_id, purpose, now); todo!() }
