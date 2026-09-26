//! GlobalID / SignedGlobalID (`gid://campfire/User/1`, `to_sgid(for: "attachable")`).
use crate::Secrets;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalId { pub app: String, pub model_name: String, pub id: String }

pub fn sgid(secrets: &Secrets, gid: &GlobalId, purpose: &str, expires_at: Option<jiff::Timestamp>) -> String { let _ = (secrets, gid, purpose, expires_at); todo!() }
pub fn locate_signed(secrets: &Secrets, sgid: &str, purpose: &str, now: jiff::Timestamp) -> Option<GlobalId> { let _ = (secrets, sgid, purpose, now); todo!() }
