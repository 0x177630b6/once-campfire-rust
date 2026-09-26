use serde_json::Value;

/// `ActiveSupport::MessageEncryptor` (aes-256-gcm) as used by encrypted cookies.
pub struct MessageEncryptor;

impl MessageEncryptor {
    pub fn encrypt_and_sign(&self, value: &Value, purpose: Option<&str>, expires_at: Option<jiff::Timestamp>) -> String { let _ = (value, purpose, expires_at); todo!() }
    pub fn decrypt_and_verify(&self, message: &str, purpose: Option<&str>, now: jiff::Timestamp) -> Result<Value, crate::Error> { let _ = (message, purpose, now); todo!() }
}
