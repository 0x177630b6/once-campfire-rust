use serde_json::Value;

/// `ActiveSupport::MessageVerifier` with the envelope/serializer/digest Rails 8.2 uses for the
/// given use (cookies, signed ids, SGIDs, Turbo stream names each configure it differently).
pub struct MessageVerifier;

impl MessageVerifier {
    pub fn generate(&self, value: &Value, purpose: Option<&str>, expires_at: Option<jiff::Timestamp>) -> String { let _ = (value, purpose, expires_at); todo!() }
    pub fn verify(&self, message: &str, purpose: Option<&str>, now: jiff::Timestamp) -> Result<Value, crate::Error> { let _ = (message, purpose, now); todo!() }
}
