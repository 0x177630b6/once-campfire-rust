//! Test support: an insecure, transparent [`Crypto`] and a frozen clock.
//!
//! `TestCrypto` has the same *shape* as Rails' (signed values are `data--digest`, encrypted values
//! are opaque, CSRF tokens are masked with a one-time pad) but none of the byte compatibility,
//! which `rails_compat` owns. Never use it outside tests.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jiff::Timestamp;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::clock::{FrozenClock, SharedClock};
use crate::crypto::{Crypto, SharedCrypto};

pub const TEST_TIME: &str = "2024-06-01T12:00:00Z";

pub fn crypto() -> SharedCrypto {
    Arc::new(TestCrypto::default())
}

pub fn frozen_clock() -> SharedClock {
    Arc::new(FrozenClock::new(TEST_TIME.parse().unwrap()))
}

#[derive(Debug, Clone)]
pub struct TestCrypto {
    secret: String,
}

impl Default for TestCrypto {
    fn default() -> Self {
        Self { secret: "test-secret".into() }
    }
}

impl TestCrypto {
    fn digest(&self, parts: &[&str]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.secret.as_bytes());
        for part in parts {
            hasher.update(b"\0");
            hasher.update(part.as_bytes());
        }
        hex(&hasher.finalize()[..16])
    }

    fn envelope(&self, kind: &str, name: &str, value: Value, expires_at: Option<Timestamp>) -> String {
        let payload = json!({ "v": value, "exp": expires_at.map(|t| t.to_string()), "pur": format!("cookie.{name}") });
        let data = STANDARD.encode(payload.to_string());
        let digest = self.digest(&[kind, &data]);
        format!("{data}--{digest}")
    }

    fn open(&self, kind: &str, name: &str, raw: &str, now: Timestamp) -> Option<Value> {
        let (data, digest) = raw.rsplit_once("--")?;
        if self.digest(&[kind, data]) != digest {
            return None;
        }
        let payload: Value = serde_json::from_slice(&STANDARD.decode(data).ok()?).ok()?;
        if payload["pur"] != format!("cookie.{name}") {
            return None;
        }
        if let Some(exp) = payload["exp"].as_str()
            && exp.parse::<Timestamp>().ok()? <= now {
                return None;
            }
        Some(payload["v"].clone())
    }

    fn global_token(&self, session_token: &str) -> Vec<u8> {
        Sha256::digest(format!("{}!real_csrf_token{session_token}", self.secret)).to_vec()
    }

    fn per_form_token(&self, session_token: &str, action_path: &str, method: &str) -> Vec<u8> {
        Sha256::digest(format!("{}{action_path}#{}{session_token}", self.secret, method.to_lowercase())).to_vec()
    }

    fn mask(raw: &[u8]) -> String {
        let pad: Vec<u8> = (0..raw.len()).map(|_| rand::random::<u8>()).collect();
        let encrypted: Vec<u8> = pad.iter().zip(raw).map(|(p, r)| p ^ r).collect();
        URL_SAFE_NO_PAD.encode([pad, encrypted].concat())
    }
}

impl Crypto for TestCrypto {
    fn sign_cookie(&self, name: &str, value: &str, expires_at: Option<Timestamp>) -> String {
        self.envelope("signed", name, Value::String(value.into()), expires_at)
    }

    fn verify_signed_cookie(&self, name: &str, raw: &str, now: Timestamp) -> Option<String> {
        self.open("signed", name, raw, now)?.as_str().map(str::to_string)
    }

    fn encrypt_cookie(&self, name: &str, value: &Value, expires_at: Option<Timestamp>) -> String {
        // A random prefix so repeated encryptions differ, like AES-GCM with a fresh IV.
        let nonce: u32 = rand::random();
        format!("{nonce:08x}{}", self.envelope("encrypted", name, value.clone(), expires_at))
    }

    fn decrypt_cookie(&self, name: &str, raw: &str, now: Timestamp) -> Option<Value> {
        self.open("encrypted", name, raw.get(8..)?, now)
    }

    fn generate_csrf_token(&self) -> String {
        let bytes: [u8; 32] = rand::random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn masked_csrf_token(&self, session_token: &str) -> String {
        Self::mask(&self.global_token(session_token))
    }

    fn per_form_masked_csrf_token(&self, session_token: &str, action: &str, method: &str, request_path: &str) -> String {
        let action_path = rails_compat::csrf::normalize_action_path(action, request_path);
        Self::mask(&self.per_form_token(session_token, &action_path, method))
    }

    fn valid_csrf_token(&self, session_token: &str, submitted: &str, request_path: &str, request_method: &str) -> bool {
        let Ok(masked) = URL_SAFE_NO_PAD.decode(submitted.trim_end_matches('=')) else { return false };
        if masked.len() != 64 {
            return false;
        }
        let (pad, encrypted) = masked.split_at(32);
        let token: Vec<u8> = pad.iter().zip(encrypted).map(|(p, e)| p ^ e).collect();
        let path = request_path.strip_suffix('/').unwrap_or(request_path);
        token == self.global_token(session_token) || token == self.per_form_token(session_token, path, request_method)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
