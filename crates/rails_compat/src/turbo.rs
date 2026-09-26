//! `Turbo::StreamsChannel.signed_stream_name` / `verified_stream_name`.
use crate::Secrets;

/// `streamables` are already-resolved stream name parts (e.g. a record's GID param, or a symbol).
pub fn signed_stream_name(secrets: &Secrets, streamables: &[&str]) -> String { let _ = (secrets, streamables); todo!() }
pub fn verified_stream_name(secrets: &Secrets, signed: &str) -> Option<String> { let _ = (secrets, signed); todo!() }
