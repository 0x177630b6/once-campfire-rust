//! The durable write log (phase 2.0): every write the workspace makes to Fizzy, one JSON line each,
//! in `<CAMPFIRE_STORAGE_PATH>/hermes/actions.jsonl` (rotated to `actions.jsonl.1` past
//! [`MAX_BYTES`]). The server log still gets each [`WriteRecord`] too; this copy is the one the app
//! can read back: the Hermes log uses it to tell the writes Campfire made (for a person, or for
//! Hermes through a proposal) from what Hermes did in Fizzy on its own, and undo uses it to see
//! whether anyone changed a card since. Never the token.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::writes::WriteRecord;

/// The log moves to `actions.jsonl.1` past this size.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// The most recent writes kept in memory (read back from the file at boot).
const RECENT: usize = 2000;

/// A [`WriteRecord`] as it is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: Timestamp,
    /// The Campfire user acting (the bot's id when Campfire runs something for Hermes).
    pub user_id: i64,
    #[serde(default)]
    pub card: Option<u64>,
    pub action: String,
    /// Whose token: `workspace` (`FIZZY_TOKEN`) or `hermes` (`HERMES_FIZZY_TOKEN`).
    pub identity: String,
    /// `workspace` (a person, from the workspace), `proposal` (Hermes, through Campfire), `undo`.
    pub via: String,
    /// The proposal or log entry the write belongs to.
    #[serde(default)]
    pub reference: Option<String>,
    pub outcome: String,
}

impl From<&WriteRecord> for Entry {
    fn from(record: &WriteRecord) -> Self {
        Self {
            at: record.at,
            user_id: record.user_id,
            card: record.card,
            action: record.action.clone(),
            identity: record.identity.to_string(),
            via: record.via.to_string(),
            reference: record.reference.clone(),
            outcome: record.outcome.clone(),
        }
    }
}

/// `actions.jsonl` and its recent entries.
#[derive(Debug)]
pub struct ActionLog {
    path: PathBuf,
    recent: Mutex<VecDeque<Entry>>,
    /// The last append that failed (the app logs it); the entry stays in memory.
    error: Mutex<Option<String>>,
}

impl ActionLog {
    /// Reads the recent entries back (the rotated file first).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut recent = VecDeque::new();
        let mut error = None;
        for file in [crate::store::rotated(&path), path.clone()] {
            match crate::store::read_json_lines::<Entry>(&file) {
                Ok(entries) => recent.extend(entries),
                Err(read) => error = Some(format!("couldn't read {}: {read}", file.display())),
            }
        }
        while recent.len() > RECENT {
            recent.pop_front();
        }
        Self { path, recent: Mutex::new(recent), error: Mutex::new(error) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &WriteRecord) {
        let entry = Entry::from(record);
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        // Under the same lock: lines never interleave.
        if let Err(error) = crate::store::append_json_line(&self.path, &entry, MAX_BYTES) {
            *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("couldn't write {}: {error}", self.path.display()));
        }
        recent.push_back(entry);
        if recent.len() > RECENT {
            recent.pop_front();
        }
    }

    /// The recent entries, oldest first.
    pub fn recent(&self) -> Vec<Entry> {
        self.recent.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    /// Writes to `card` after `after`, oldest first.
    pub fn card_writes_after(&self, card: u64, after: Timestamp) -> Vec<Entry> {
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|entry| entry.card == Some(card) && entry.at > after)
            .cloned()
            .collect()
    }

    /// The last write error, once (the app logs it).
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(n: u64) -> WriteRecord {
        WriteRecord {
            at: "2026-09-30T09:00:00Z".parse().unwrap(),
            user_id: 7,
            card: Some(n),
            action: "comment".into(),
            identity: "workspace",
            outcome: "ok".into(),
            via: "workspace",
            reference: None,
        }
    }

    #[test]
    fn writes_survive_a_restart() {
        let dir = crate::store::scratch_dir("journal");
        let path = dir.join("hermes").join("actions.jsonl");
        let log = ActionLog::open(&path);
        log.append(&record(12));
        log.append(&record(13));
        assert_eq!(log.take_error(), None);
        let reopened = ActionLog::open(&path);
        assert_eq!(reopened.recent(), log.recent());
        assert_eq!(reopened.recent()[1].card, Some(13));
        let line = std::fs::read_to_string(&path).unwrap();
        assert!(
            line.starts_with(r#"{"at":"2026-09-30T09:00:00Z","user_id":7,"card":12,"action":"comment","identity":"workspace""#),
            "{line}"
        );
        assert_eq!(reopened.card_writes_after(12, "2026-09-30T08:00:00Z".parse().unwrap()).len(), 1);
        assert!(reopened.card_writes_after(12, "2026-09-30T09:00:00Z".parse().unwrap()).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_append_is_reported_and_kept_in_memory() {
        let dir = crate::store::scratch_dir("journal-ro");
        std::fs::write(dir.join("hermes"), "").unwrap();
        let log = ActionLog::open(dir.join("hermes").join("actions.jsonl"));
        log.append(&record(12));
        assert!(log.take_error().unwrap().contains("couldn't write"));
        assert_eq!(log.take_error(), None, "said once");
        assert_eq!(log.recent().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
