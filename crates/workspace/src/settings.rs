//! The workspace's settings, which Campfire administrators edit at `/workspace/settings`: the
//! departments (a name, the Fizzy tag that marks their cards on the incident board, and the rooms
//! linked to them), the duty managers, and who may confirm drafts and act on cards ([`Policy`]).
//!
//! They live in a JSON file under Campfire's storage root (`<CAMPFIRE_STORAGE_PATH>/hermes/
//! workspace.json`), not in Campfire's database, so upstream's schema stays untouched. Writes are
//! atomic (a temporary file in the same directory, `fsync`, a rename, then the directory's
//! `fsync`), one save at a time. A missing file is the defaults (anyone may act). A file that
//! exists but can't be read or doesn't validate **fails closed** ([`Settings::fail_closed`]: no
//! departments, duty managers only, the administrators being the duty managers) and says so in the
//! log and on the settings page; saving replaces it.
//!
//! ```json
//! {
//!   "version": 1,
//!   "departments": [{"name": "Engineering", "tag": "engineering", "rooms": [3, 7]}],
//!   "duty_managers": [1, 5],
//!   "confirm_policy": "anyone"
//! }
//! ```
//!
//! `duty_managers` absent (or `null`) means Campfire's administrators.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::home::Viewer;

pub const SETTINGS_VERSION: u32 = 1;
pub const MAX_DEPARTMENTS: usize = 50;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_TAG_CHARS: usize = 40;
pub const MAX_DUTY_MANAGERS: usize = 200;
pub const MAX_ROOMS_PER_DEPARTMENT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub departments: Vec<Department>,
    /// Campfire user ids; `None` = the administrators.
    #[serde(default)]
    pub duty_managers: Option<Vec<i64>>,
    #[serde(default)]
    pub confirm_policy: Policy,
}

fn version() -> u32 {
    SETTINGS_VERSION
}

impl Default for Settings {
    fn default() -> Self {
        Self { version: SETTINGS_VERSION, departments: Vec::new(), duty_managers: None, confirm_policy: Policy::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Department {
    pub name: String,
    /// A Fizzy tag title: lowercase, no `#` (Fizzy lowercases and strips it).
    pub tag: String,
    /// Campfire room ids whose cards panel shows this department's cards.
    #[serde(default)]
    pub rooms: Vec<i64>,
}

/// Who may confirm a Hermes draft and act on incident cards. The one place this is decided
/// ([`Policy::permits`]); the owner will tighten it from `Anyone`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Every signed-in user who can see it (phase 0 and 1).
    #[default]
    Anyone,
    /// Drafts: the person who reported it, or a duty manager. Changing a card: duty managers.
    /// Creating cards and commenting: anyone.
    AuthorOrDutyManager,
    /// Duty managers only, for everything.
    DutyManagersOnly,
}

impl Policy {
    pub const ALL: [Policy; 3] = [Policy::Anyone, Policy::AuthorOrDutyManager, Policy::DutyManagersOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Anyone => "anyone",
            Policy::AuthorOrDutyManager => "author_or_duty_manager",
            Policy::DutyManagersOnly => "duty_managers_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|policy| policy.as_str() == value.trim())
    }

    pub fn label(self) -> &'static str {
        match self {
            Policy::Anyone => "Anyone who can see it",
            Policy::AuthorOrDutyManager => "The reporter or a duty manager (cards: duty managers)",
            Policy::DutyManagersOnly => "Duty managers only",
        }
    }

    /// Whether `actor` may do `act`; `duty_manager` is [`Settings::is_duty_manager`].
    pub fn permits(self, act: &Act, actor: &Viewer, duty_manager: bool) -> bool {
        match (self, act) {
            (Policy::Anyone, _) => true,
            (_, _) if duty_manager => true,
            (Policy::DutyManagersOnly, _) => false,
            (Policy::AuthorOrDutyManager, Act::ConfirmDraft { reporter_id }) => *reporter_id == Some(actor.id),
            (Policy::AuthorOrDutyManager, Act::CreateCard | Act::Comment) => true,
            (Policy::AuthorOrDutyManager, Act::ChangeCard) => false,
        }
    }
}

/// What someone wants to do, for [`Policy::permits`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Answer a Hermes draft (File / Dismiss); `reporter_id` is who reported it, when known.
    ConfirmDraft {
        reporter_id: Option<i64>,
    },
    CreateCard,
    Comment,
    /// Move, close, "not now", severity, departments, steps.
    ChangeCard,
}

impl Act {
    /// Said to someone the policy refuses.
    pub fn refusal(&self, policy: Policy) -> &'static str {
        match (self, policy) {
            (Act::ConfirmDraft { .. }, Policy::AuthorOrDutyManager) => {
                "Only the person who reported it or a duty manager can confirm this draft."
            }
            (_, _) => "Only duty managers can do this.",
        }
    }
}

/// Why a save didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The settings aren't valid (422); the message says why.
    Invalid(SettingsError),
    /// The file couldn't be written (500); nothing changed, in memory or on disk.
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Io(error) => write!(f, "couldn't write the settings file: {error}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// A settings file or form that can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError(pub String);

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SettingsError {}

/// `#Engineering ` → `engineering`, as Fizzy stores tag titles.
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('#').trim().to_lowercase()
}

impl Settings {
    /// What a settings file that exists but can't be used means: nobody but the duty managers
    /// (Campfire's administrators, since no one is listed) may act, and no departments, until an
    /// administrator saves the settings again. A policy the owner tightened must not silently
    /// loosen to `anyone` because the file got damaged.
    pub fn fail_closed() -> Self {
        Self { confirm_policy: Policy::DutyManagersOnly, ..Self::default() }
    }

    /// The settings cleaned up (trimmed, tags normalized, duplicates of room and user ids dropped),
    /// or why they can't be saved.
    pub fn validated(mut self) -> Result<Self, SettingsError> {
        let invalid = |message: String| Err(SettingsError(message));
        if self.departments.len() > MAX_DEPARTMENTS {
            return invalid(format!("At most {MAX_DEPARTMENTS} departments."));
        }
        let mut names: Vec<String> = Vec::new();
        let mut tags: Vec<String> = Vec::new();
        for department in &mut self.departments {
            department.name = department.name.split_whitespace().collect::<Vec<_>>().join(" ");
            let tag = if department.tag.trim().is_empty() { department.name.replace(' ', "-") } else { department.tag.clone() };
            department.tag = normalize_tag(&tag);
            let (name, tag) = (&department.name, &department.tag);
            if name.is_empty() {
                return invalid("Every department needs a name.".into());
            }
            if name.chars().count() > MAX_NAME_CHARS {
                return invalid(format!("“{name}” is too long ({MAX_NAME_CHARS} characters at most)."));
            }
            if tag.is_empty() || tag.chars().count() > MAX_TAG_CHARS {
                return invalid(format!("The tag of “{name}” must be 1 to {MAX_TAG_CHARS} characters."));
            }
            if !tag.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) {
                return invalid(format!("The tag of “{name}” can only use letters, digits, “-”, “_” and “.” (got “{tag}”)."));
            }
            if tag.starts_with("sev-") {
                return invalid(format!("“{tag}” is a severity tag; pick another tag for “{name}”."));
            }
            if names.contains(&name.to_lowercase()) {
                return invalid(format!("Two departments are named “{name}”."));
            }
            if tags.contains(tag) {
                return invalid(format!("Two departments use the tag “{tag}”."));
            }
            names.push(name.to_lowercase());
            tags.push(tag.clone());
            dedup(&mut department.rooms);
            if department.rooms.len() > MAX_ROOMS_PER_DEPARTMENT || department.rooms.iter().any(|id| *id <= 0) {
                return invalid(format!("The rooms of “{name}” aren't valid."));
            }
        }
        if let Some(managers) = &mut self.duty_managers {
            dedup(managers);
            if managers.len() > MAX_DUTY_MANAGERS || managers.iter().any(|id| *id <= 0) {
                return invalid("The duty managers aren't valid.".into());
            }
        }
        self.version = SETTINGS_VERSION;
        Ok(self)
    }

    /// A duty manager: listed, or, while nobody is listed, a Campfire administrator.
    pub fn is_duty_manager(&self, actor: &Viewer) -> bool {
        match &self.duty_managers {
            Some(managers) => managers.contains(&actor.id),
            None => actor.administrator,
        }
    }

    /// The duty managers' ids, given the administrators' (the default).
    pub fn duty_manager_ids(&self, administrators: &[i64]) -> Vec<i64> {
        self.duty_managers.clone().unwrap_or_else(|| administrators.to_vec())
    }

    pub fn permits(&self, act: &Act, actor: &Viewer) -> bool {
        self.confirm_policy.permits(act, actor, self.is_duty_manager(actor))
    }

    /// The departments linked to a room.
    pub fn departments_of_room(&self, room_id: i64) -> Vec<&Department> {
        self.departments.iter().filter(|department| department.rooms.contains(&room_id)).collect()
    }

    pub fn department_by_tag(&self, tag: &str) -> Option<&Department> {
        let tag = normalize_tag(tag);
        self.departments.iter().find(|department| department.tag == tag)
    }

    /// A card's departments: its tags that are department tags, in settings order.
    pub fn departments_of(&self, tags: &[String]) -> Vec<&Department> {
        self.departments.iter().filter(|department| tags.iter().any(|tag| normalize_tag(tag) == department.tag)).collect()
    }
}

fn dedup(ids: &mut Vec<i64>) {
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
}

/// The settings file and its current contents.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    current: RwLock<Arc<Settings>>,
    /// Why the file couldn't be read at boot ([`Settings::fail_closed`] is in use).
    load_error: RwLock<Option<String>>,
    /// One save at a time, so that the file and `current` end as the same save.
    saving: Mutex<()>,
}

impl SettingsStore {
    /// Reads `path`: missing = the defaults; unreadable or invalid = [`Settings::fail_closed`] and
    /// [`load_error`](Self::load_error).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (settings, error) = match read(&path) {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::fail_closed(), Some(error.0)),
        };
        Self { path, current: RwLock::new(Arc::new(settings)), load_error: RwLock::new(error), saving: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self) -> Arc<Settings> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn load_error(&self) -> Option<String> {
        self.load_error.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Validates, writes atomically (temporary file, then rename), then uses them. Saves run one
    /// at a time: the last one written is the one in use.
    pub fn save(&self, settings: Settings) -> Result<Arc<Settings>, SaveError> {
        let settings = settings.validated().map_err(SaveError::Invalid)?;
        let _saving = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        write_atomically(&self.path, &settings).map_err(|error| SaveError::Io(error.to_string()))?;
        let settings = Arc::new(settings);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
        *self.load_error.write().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(settings)
    }
}

fn read(path: &Path) -> Result<Settings, SettingsError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(SettingsError(format!("couldn't read {}: {error}", path.display()))),
    };
    let settings: Settings =
        serde_json::from_slice(&bytes).map_err(|error| SettingsError(format!("{} isn't valid: {error}", path.display())))?;
    settings.validated().map_err(|error| SettingsError(format!("{} isn't valid: {error}", path.display())))
}

fn write_atomically(path: &Path, settings: &Settings) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("workspace.json");
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let temporary = dir.join(format!(".{name}.{}-{nanos}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        let mut json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
        json.push(b'\n');
        file.write_all(&json)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    sync_dir(dir)
}

/// Makes the rename durable: a crash right after it can't bring the old file back.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Windows can't open a directory as a file; its renames are journaled.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(id: i64, administrator: bool) -> Viewer {
        Viewer { id, name: format!("user {id}"), email: None, administrator }
    }

    fn department(name: &str, tag: &str, rooms: &[i64]) -> Department {
        Department { name: name.into(), tag: tag.into(), rooms: rooms.to_vec() }
    }

    /// A fresh directory under the system's temporary directory.
    pub(crate) fn scratch_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("campfire-workspace-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validation_normalizes_tags_and_ids() {
        let settings = Settings {
            departments: vec![department("  Front   desk ", "", &[3, 3, 4]), department("Engineering", " #Engineering ", &[])],
            duty_managers: Some(vec![5, 5, 1]),
            ..Settings::default()
        }
        .validated()
        .unwrap();
        assert_eq!(settings.departments[0].name, "Front desk");
        assert_eq!(settings.departments[0].tag, "front-desk");
        assert_eq!(settings.departments[0].rooms, vec![3, 4]);
        assert_eq!(settings.departments[1].tag, "engineering");
        assert_eq!(settings.duty_managers, Some(vec![5, 1]));
    }

    #[test]
    fn validation_rejects_bad_departments() {
        for (departments, expected) in [
            (vec![department("", "x", &[])], "needs a name"),
            (vec![department("A", "two words", &[])], "can only use"),
            (vec![department("A", "sev-high", &[])], "severity tag"),
            (vec![department("A", "a", &[]), department("a", "b", &[])], "Two departments are named"),
            (vec![department("A", "x", &[]), department("B", "#X", &[])], "Two departments use the tag"),
            (vec![department("A", "x", &[0])], "rooms"),
            (vec![department(&"n".repeat(61), "x", &[])], "too long"),
        ] {
            let error = Settings { departments, ..Settings::default() }.validated().unwrap_err();
            assert!(error.0.contains(expected), "{error}");
        }
        let error = Settings { duty_managers: Some(vec![-1]), ..Settings::default() }.validated().unwrap_err();
        assert!(error.0.contains("duty managers"));
    }

    #[test]
    fn duty_managers_default_to_administrators() {
        let settings = Settings::default();
        assert!(settings.is_duty_manager(&viewer(1, true)) && !settings.is_duty_manager(&viewer(2, false)));
        assert_eq!(settings.duty_manager_ids(&[1, 9]), vec![1, 9]);
        let listed = Settings { duty_managers: Some(vec![2]), ..Settings::default() };
        assert!(!listed.is_duty_manager(&viewer(1, true)) && listed.is_duty_manager(&viewer(2, false)));
        assert_eq!(listed.duty_manager_ids(&[1]), vec![2]);
    }

    #[test]
    fn the_policy() {
        let (member, reporter, manager) = (viewer(1, false), viewer(2, false), viewer(3, false));
        let draft = Act::ConfirmDraft { reporter_id: Some(2) };
        let unknown_reporter = Act::ConfirmDraft { reporter_id: None };
        let settings = |policy| Settings { duty_managers: Some(vec![3]), confirm_policy: policy, ..Settings::default() };

        let anyone = settings(Policy::Anyone);
        for act in [&draft, &unknown_reporter, &Act::CreateCard, &Act::Comment, &Act::ChangeCard] {
            assert!(anyone.permits(act, &member), "{act:?}");
        }

        let authors = settings(Policy::AuthorOrDutyManager);
        assert!(!authors.permits(&draft, &member));
        assert!(authors.permits(&draft, &reporter) && authors.permits(&draft, &manager));
        assert!(!authors.permits(&unknown_reporter, &reporter) && authors.permits(&unknown_reporter, &manager));
        assert!(authors.permits(&Act::CreateCard, &member) && authors.permits(&Act::Comment, &member));
        assert!(!authors.permits(&Act::ChangeCard, &member) && authors.permits(&Act::ChangeCard, &manager));

        let managers = settings(Policy::DutyManagersOnly);
        for act in [&draft, &Act::CreateCard, &Act::Comment, &Act::ChangeCard] {
            assert!(!managers.permits(act, &reporter) && managers.permits(act, &manager), "{act:?}");
        }
        assert_eq!(Policy::parse("author_or_duty_manager"), Some(Policy::AuthorOrDutyManager));
        assert_eq!(Policy::parse("everyone"), None);
    }

    #[test]
    fn rooms_and_tags_find_departments() {
        let settings = Settings {
            departments: vec![department("Engineering", "engineering", &[3]), department("Housekeeping", "housekeeping", &[3, 4])],
            ..Settings::default()
        };
        let names = |departments: Vec<&Department>| departments.into_iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(settings.departments_of_room(3)), ["Engineering", "Housekeeping"]);
        assert!(settings.departments_of_room(9).is_empty());
        assert_eq!(names(settings.departments_of(&["sev-high".into(), "Housekeeping".into()])), ["Housekeeping"]);
        assert_eq!(settings.department_by_tag("#ENGINEERING").unwrap().name, "Engineering");
    }

    #[test]
    fn the_store_saves_atomically_and_reloads() {
        let dir = scratch_dir("settings");
        let path = dir.join("hermes").join("workspace.json");
        let store = SettingsStore::open(&path);
        assert_eq!(*store.get(), Settings::default());
        assert_eq!(store.load_error(), None, "a missing file is the defaults");

        let saved =
            store.save(Settings { departments: vec![department("Engineering", "#Engineering", &[3])], ..Settings::default() }).unwrap();
        assert_eq!(saved.departments[0].tag, "engineering");
        let entries: Vec<String> =
            std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(entries, vec!["workspace.json".to_string()], "no temporary file left");
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["departments"][0]["tag"], "engineering");
        assert_eq!(json["confirm_policy"], "anyone");
        assert_eq!(SettingsStore::open(&path).get(), saved);

        let refused = store.save(Settings { departments: vec![department("", "x", &[])], ..Settings::default() });
        assert!(refused.is_err());
        assert_eq!(SettingsStore::open(&path).get(), saved, "an invalid save changes nothing");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_broken_file_fails_closed_and_says_so() {
        let dir = scratch_dir("broken");
        let path = dir.join("workspace.json");
        std::fs::write(&path, r#"{"departments": [], "confirm_policy": "anyone", "#).unwrap();
        let store = SettingsStore::open(&path);
        assert_eq!(*store.get(), Settings::fail_closed());
        assert!(store.load_error().unwrap().contains("isn't valid"));
        let settings = store.get();
        assert!(settings.departments.is_empty() && settings.confirm_policy == Policy::DutyManagersOnly);
        let (member, administrator) = (viewer(1, false), viewer(2, true));
        for act in [Act::ConfirmDraft { reporter_id: Some(1) }, Act::CreateCard, Act::Comment, Act::ChangeCard] {
            assert!(!settings.permits(&act, &member) && settings.permits(&act, &administrator), "{act:?}");
        }

        std::fs::write(&path, r#"{"departments": [{"name": "A", "tag": "sev-low"}]}"#).unwrap();
        let invalid = SettingsStore::open(&path);
        assert!(invalid.load_error().unwrap().contains("severity tag"));
        assert_eq!(*invalid.get(), Settings::fail_closed(), "a file that doesn't validate fails closed too");

        let unreadable = dir.join("a-directory.json");
        std::fs::create_dir(&unreadable).unwrap();
        let store_of_a_directory = SettingsStore::open(&unreadable);
        assert!(store_of_a_directory.load_error().unwrap().contains("couldn't read"));
        assert_eq!(*store_of_a_directory.get(), Settings::fail_closed(), "an unreadable file fails closed");

        assert_eq!(*SettingsStore::open(dir.join("missing.json")).get(), Settings::default(), "a missing file is still the defaults");

        store.save(Settings::default()).unwrap();
        assert_eq!(store.load_error(), None);
        assert_eq!(*store.get(), Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_write_is_an_io_error_and_changes_nothing() {
        let dir = scratch_dir("unwritable");
        // The file's directory is a file: nothing can be created in it.
        std::fs::write(dir.join("hermes"), "").unwrap();
        let store = SettingsStore::open(dir.join("hermes").join("workspace.json"));
        let before = store.get();
        let error = store.save(Settings { departments: vec![department("A", "a", &[])], ..Settings::default() }).unwrap_err();
        assert!(matches!(error, SaveError::Io(_)), "{error:?}");
        assert_eq!(store.get(), before);
        let invalid = store.save(Settings { departments: vec![department("", "x", &[])], ..Settings::default() }).unwrap_err();
        assert!(matches!(invalid, SaveError::Invalid(_)), "{invalid:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_saves_leave_the_file_and_memory_agreeing() {
        let dir = scratch_dir("concurrent");
        let path = dir.join("workspace.json");
        let store = std::sync::Arc::new(SettingsStore::open(&path));
        let threads: Vec<_> = (1..=8)
            .map(|n| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for round in 0..5 {
                        let name = format!("D{n}-{round}");
                        store.save(Settings { departments: vec![department(&name, "", &[])], ..Settings::default() }).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(SettingsStore::open(&path).get(), store.get());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
