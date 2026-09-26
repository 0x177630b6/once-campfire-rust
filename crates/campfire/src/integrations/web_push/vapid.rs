//! VAPID identification (RFC 8292) as `WebPush::Request#build_vapid_header` writes it:
//! `vapid t=<ES256 JWT>,k=<public key>`, the JWT carrying `aud`, `exp` (12 hours out) and `sub`.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};

use super::{DeliveryError, decode64, encode64_nopad};

/// `WebPush::Notification#vapid_identification`: the `mailto:support@37signals.com` subject,
/// merged with `Rails.configuration.x.vapid` (`VAPID_PUBLIC_KEY`/`VAPID_PRIVATE_KEY`).
#[derive(Debug, Clone)]
pub struct VapidConfig {
    pub subject: String,
    pub public_key: String,
    pub private_key: String,
}

/// `WebPush::Request#expiration`
const EXPIRATION_SECONDS: i64 = 12 * 60 * 60;

impl VapidConfig {
    pub fn new(public_key: impl Into<String>, private_key: impl Into<String>) -> Self {
        Self { subject: "mailto:support@37signals.com".into(), public_key: public_key.into(), private_key: private_key.into() }
    }

    /// The `Authorization` header for a push service at `audience` (`scheme://host`).
    pub fn authorization(&self, audience: &str, now: i64) -> Result<String, DeliveryError> {
        let (signing_key, public_key) = self.keys()?;
        let header = r#"{"typ":"JWT","alg":"ES256"}"#;
        let claims = serde_json::json!({ "aud": audience, "exp": now + EXPIRATION_SECONDS, "sub": self.subject }).to_string();
        let signing_input = format!("{}.{}", encode64_nopad(header.as_bytes()), encode64_nopad(claims.as_bytes()));
        let signature: Signature = signing_key.sign(signing_input.as_bytes());
        Ok(format!("vapid t={signing_input}.{},k={}", encode64_nopad(&signature.to_bytes()), encode64_nopad(&public_key)))
    }

    /// `VapidKey.from_keys(public_key, private_key)`: the private scalar signs; the public key
    /// is sent as given.
    fn keys(&self) -> Result<(SigningKey, Vec<u8>), DeliveryError> {
        let invalid = || DeliveryError::OpenSsl("invalid VAPID key".into());
        let public_key = decode64(&self.public_key)?;
        p256::PublicKey::from_sec1_bytes(&public_key).map_err(|_| invalid())?;
        let private_key = decode64(&self.private_key)?;
        if private_key.len() > 32 {
            return Err(invalid());
        }
        let mut scalar = [0u8; 32];
        scalar[32 - private_key.len()..].copy_from_slice(&private_key);
        let signing_key = SigningKey::from_slice(&scalar).map_err(|_| invalid())?;
        Ok((signing_key, public_key))
    }
}
