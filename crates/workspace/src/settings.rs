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
//! log and on the settings page; saving replaces it. Two exceptions, read with a warning
//! ([`SettingsStore::load_warning`]) rather than failing the whole file closed: a handover time zone
//! this system can't resolve is read as UTC (it only moves the shift ends), and a department whose
//! tag is a ticket type ([`TICKET_TYPE_TAGS`], refused on save since the broader tickets) is kept
//! as saved (it worked that way before; turning every department off over it would be worse). Saving also refuses a restricted department linked to an open room
//! ([`Settings::check_open_rooms`], checked by the app, which knows the rooms).
//!
//! ```json
//! {
//!   "version": 3,
//!   "departments": [{"name": "Engineering", "tag": "engineering", "rooms": [3, 7], "restricted": false}],
//!   "duty_managers": [1, 5],
//!   "confirm_policy": "anyone",
//!   "autonomy": {"create": "ask_first", "comment": "alone", "tag": "alone", "move": "ask_first",
//!                "close": "ask_first", "step": "ask_first"},
//!   "hermes_fizzy_user_id": null,
//!   "visibility": {"mode": "everyone", "untagged": "everyone"},
//!   "notifications": {"enabled": true, "severities": ["critical", "high"], "department_rooms": true,
//!                     "new_reminder_min": 15, "draft_reminder_min": 10},
//!   "handover": {"room_id": null, "shift_ends": ["07:00", "15:00", "23:00"], "time_zone": "Europe/Paris",
//!                "reminder": true}
//! }
//! ```
//!
//! `duty_managers` absent (or `null`) means Campfire's administrators. Version 2 (phase 2) added
//! `autonomy` (what Hermes may do alone when it proposes through Campfire, [`Autonomy`]) and
//! `hermes_fizzy_user_id` (Hermes's Fizzy user, for its log; `null` = learned from
//! `HERMES_FIZZY_TOKEN`). Version 3 (phase 2.5–2.7) added a department's `restricted`, `visibility`
//! ([`crate::visibility`]), `notifications` ([`crate::alerts`]) and `handover`
//! ([`crate::handover`]). An older file loads with the defaults of what it lacks (nothing changes:
//! everyone sees every card, no handover room); the next save writes version 3.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

pub use crate::alerts::Notifications;
pub use crate::handover::HandoverSettings;
use crate::home::Viewer;
pub use crate::visibility::Visibility;

pub const SETTINGS_VERSION: u32 = 3;
pub const MAX_FIZZY_ID_CHARS: usize = 64;
pub const MAX_DEPARTMENTS: usize = 50;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_TAG_CHARS: usize = 40;
/// The ticket types the incident-report skill tags cards with (`request`, `fault`…): a
/// department can't use one as its tag, or every ticket of that type would count as that
/// department's.
pub const TICKET_TYPE_TAGS: [&str; 8] = ["request", "task", "fault", "complaint", "incident", "safety", "handover", "inspection"];
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
    /// What Hermes may do alone when it proposes an action through Campfire (version 2).
    #[serde(default)]
    pub autonomy: Autonomy,
    /// Hermes's Fizzy user id, for its log; `None` = learned from `HERMES_FIZZY_TOKEN` (version 2).
    #[serde(default)]
    pub hermes_fizzy_user_id: Option<String>,
    /// Who sees which cards in Campfire (version 3, phase 2.7).
    #[serde(default)]
    pub visibility: Visibility,
    /// Alerts and reminders (version 3, phase 2.5).
    #[serde(default)]
    pub notifications: Notifications,
    /// The end-of-shift handover (version 3, phase 2.6).
    #[serde(default)]
    pub handover: HandoverSettings,
}

/// A file without `version` is a version 1 file.
fn version() -> u32 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            departments: Vec::new(),
            duty_managers: None,
            confirm_policy: Policy::default(),
            autonomy: Autonomy::default(),
            hermes_fizzy_user_id: None,
            visibility: Visibility::default(),
            notifications: Notifications::default(),
            handover: HandoverSettings::default(),
        }
    }
}

/// How far Hermes may go on its own with one kind of action it proposes through Campfire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dial {
    /// Campfire runs it at once (it's logged and can be undone).
    Alone,
    /// Campfire posts it as a draft someone confirms (per the confirm policy); 24 h to answer.
    AskFirst,
    /// Campfire refuses it, and Hermes says so.
    Never,
}

impl Dial {
    pub const ALL: [Dial; 3] = [Dial::Alone, Dial::AskFirst, Dial::Never];

    pub fn as_str(self) -> &'static str {
        match self {
            Dial::Alone => "alone",
            Dial::AskFirst => "ask_first",
            Dial::Never => "never",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dial::Alone => "Alone",
            Dial::AskFirst => "Ask first",
            Dial::Never => "Never",
        }
    }
}

/// The kinds of actions Hermes can propose (the dial is set per kind). Deleting and reassigning
/// cards aren't among them: Campfire never does either for Hermes ("Never", fixed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Create,
    Comment,
    /// Severity and department tags.
    Tag,
    /// To New, a column or Monitoring.
    Move,
    Close,
    Step,
}

impl ActionKind {
    pub const ALL: [ActionKind; 6] =
        [ActionKind::Create, ActionKind::Comment, ActionKind::Tag, ActionKind::Move, ActionKind::Close, ActionKind::Step];

    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Create => "create",
            ActionKind::Comment => "comment",
            ActionKind::Tag => "tag",
            ActionKind::Move => "move",
            ActionKind::Close => "close",
            ActionKind::Step => "step",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Create => "Create a card",
            ActionKind::Comment => "Comment on a card",
            ActionKind::Tag => "Change severity or departments",
            ActionKind::Move => "Move a card (New, a column, Monitoring)",
            ActionKind::Close => "Close a card",
            ActionKind::Step => "Tick a step",
        }
    }
}

/// The dial of every kind (decision D5's defaults: comment and tag alone; create, move and close
/// ask first; steps ask first too, as the decision didn't list them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Autonomy {
    pub create: Dial,
    pub comment: Dial,
    pub tag: Dial,
    #[serde(rename = "move")]
    pub move_card: Dial,
    pub close: Dial,
    pub step: Dial,
}

impl Default for Autonomy {
    fn default() -> Self {
        Self {
            create: Dial::AskFirst,
            comment: Dial::Alone,
            tag: Dial::Alone,
            move_card: Dial::AskFirst,
            close: Dial::AskFirst,
            step: Dial::AskFirst,
        }
    }
}

impl Autonomy {
    /// A damaged settings file: nothing alone.
    pub fn ask_first() -> Self {
        Self {
            create: Dial::AskFirst,
            comment: Dial::AskFirst,
            tag: Dial::AskFirst,
            move_card: Dial::AskFirst,
            close: Dial::AskFirst,
            step: Dial::AskFirst,
        }
    }

    pub fn dial(&self, kind: ActionKind) -> Dial {
        match kind {
            ActionKind::Create => self.create,
            ActionKind::Comment => self.comment,
            ActionKind::Tag => self.tag,
            ActionKind::Move => self.move_card,
            ActionKind::Close => self.close,
            ActionKind::Step => self.step,
        }
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
    /// With `visibility.mode` = `by_department_room`, only the members of its rooms (and the duty
    /// managers and administrators) see its cards (version 3, phase 2.7).
    #[serde(default)]
    pub restricted: bool,
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
            (_, Act::Undo { for_user_id }) => duty_manager || *for_user_id == Some(actor.id),
            (_, Act::Handover) => duty_manager,
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
    /// Undo something Hermes did (phase 2): duty managers, and the person it was done for, whatever
    /// the confirm policy (decision D8).
    Undo {
        for_user_id: Option<i64>,
    },
    /// Prepare and post the end-of-shift handover (phase 2.6): duty managers, whatever the policy.
    Handover,
}

impl Act {
    /// Said to someone the policy refuses.
    pub fn refusal(&self, policy: Policy) -> &'static str {
        match (self, policy) {
            (Act::ConfirmDraft { .. }, Policy::AuthorOrDutyManager) => {
                "Only the person who reported it or a duty manager can confirm this draft."
            }
            (Act::Undo { .. }, _) => "Only duty managers and the person it was done for can undo this.",
            (Act::Handover, _) => "Only duty managers prepare and post the handover.",
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
    /// loosen to `anyone` because the file got damaged. Likewise the visibility: with no department
    /// known, every card is "untagged", and only the duty managers see them (phase 2.7).
    pub fn fail_closed() -> Self {
        Self {
            confirm_policy: Policy::DutyManagersOnly,
            autonomy: Autonomy::ask_first(),
            visibility: Visibility::closed(),
            ..Self::default()
        }
    }

    /// The settings cleaned up (trimmed, tags normalized, duplicates of room and user ids dropped),
    /// or why they can't be saved.
    pub fn validated(self) -> Result<Self, SettingsError> {
        self.validate(false)
    }

    /// [`validated`](Self::validated), except that a department tagged with a ticket type is
    /// accepted (a file saved before that was refused, read at boot).
    fn validate(mut self, allow_ticket_type_tags: bool) -> Result<Self, SettingsError> {
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
            if !allow_ticket_type_tags && TICKET_TYPE_TAGS.contains(&tag.as_str()) {
                return invalid(format!(
                    "“{tag}” is a ticket type (one of {}); pick another tag for “{name}”, e.g. “{tag}-team”.",
                    TICKET_TYPE_TAGS.join(", ")
                ));
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
        self.hermes_fizzy_user_id = self.hermes_fizzy_user_id.map(|id| id.trim().to_string()).filter(|id| !id.is_empty());
        if let Some(id) = &self.hermes_fizzy_user_id
            && (id.len() > MAX_FIZZY_ID_CHARS || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        {
            return invalid(format!("“{id}” isn't a Fizzy user id (letters, digits, “-” and “_”)."));
        }
        self.notifications = self.notifications.validated()?;
        self.handover = self.handover.validated()?;
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
    /// What was read differently from the file at boot (a time zone this system can't resolve).
    load_warning: RwLock<Option<String>>,
    /// One save at a time, so that the file and `current` end as the same save.
    saving: Mutex<()>,
}

impl SettingsStore {
    /// Reads `path`: missing = the defaults; unreadable or invalid = [`Settings::fail_closed`] and
    /// [`load_error`](Self::load_error).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (settings, error, warning) = match read(&path) {
            Ok((settings, warning)) => (settings, None, warning),
            Err(error) => (Settings::fail_closed(), Some(error.0), None),
        };
        Self {
            path,
            current: RwLock::new(Arc::new(settings)),
            load_error: RwLock::new(error),
            load_warning: RwLock::new(warning),
            saving: Mutex::new(()),
        }
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

    /// What was read differently from the file (the app logs it, the settings page shows it) until
    /// the next save.
    pub fn load_warning(&self) -> Option<String> {
        self.load_warning.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Validates, writes atomically (temporary file, then rename), then uses them. Saves run one
    /// at a time: the last one written is the one in use.
    pub fn save(&self, settings: Settings) -> Result<Arc<Settings>, SaveError> {
        let settings = settings.validated().map_err(SaveError::Invalid)?;
        let _saving = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        crate::store::write_json_atomically(&self.path, &settings).map_err(|error| SaveError::Io(error.to_string()))?;
        let settings = Arc::new(settings);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
        *self.load_error.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.load_warning.write().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(settings)
    }
}

/// The file's settings, and a warning when one was read differently. A time zone this system can't
/// resolve (a file written where it could, or a zone table that changed) is UTC, with a warning,
/// rather than failing the whole file closed: it only moves the handover's shift ends. A save still
/// refuses it.
fn read(path: &Path) -> Result<(Settings, Option<String>), SettingsError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Settings::default(), None)),
        Err(error) => return Err(SettingsError(format!("couldn't read {}: {error}", path.display()))),
    };
    let mut settings: Settings =
        serde_json::from_slice(&bytes).map_err(|error| SettingsError(format!("{} isn't valid: {error}", path.display())))?;
    let mut warning = None;
    if crate::shifts::time_zone(&settings.handover.time_zone).is_none() {
        warning = Some(format!(
            "the handover's time zone “{}” in {} isn't one Meshduty knows: UTC is used until an administrator saves the settings",
            settings.handover.time_zone.chars().take(64).collect::<String>(),
            path.display()
        ));
        settings.handover.time_zone = "UTC".into();
    }
    let settings = settings.validate(true).map_err(|error| SettingsError(format!("{} isn't valid: {error}", path.display())))?;
    let reserved: Vec<&str> =
        settings.departments.iter().map(|department| department.tag.as_str()).filter(|tag| TICKET_TYPE_TAGS.contains(tag)).collect();
    if !reserved.is_empty() {
        let note = format!(
            "the department tag(s) {} in {} are ticket types: every card of that type counts as that department's; \
rename them in the settings (a save refuses them)",
            reserved.iter().map(|tag| format!("“{tag}”")).collect::<Vec<_>>().join(", "),
            path.display()
        );
        warning = Some(match warning {
            Some(first) => format!("{first}; {note}"),
            None => note,
        });
    }
    Ok((settings, warning))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(id: i64, administrator: bool) -> Viewer {
        Viewer { id, name: format!("user {id}"), email: None, administrator }
    }

    fn department(name: &str, tag: &str, rooms: &[i64]) -> Department {
        Department { name: name.into(), tag: tag.into(), rooms: rooms.to_vec(), restricted: false }
    }

    use crate::store::scratch_dir;

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
            (vec![department("A", "#Incident", &[])], "“incident” is a ticket type"),
            (vec![department("Front desk", "request", &[])], "pick another tag for “Front desk”, e.g. “request-team”"),
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

    #[test]
    fn a_version_one_file_loads_with_the_phase_two_defaults() {
        let dir = scratch_dir("v1");
        let path = dir.join("workspace.json");
        std::fs::write(
            &path,
            r#"{"version": 1, "departments": [{"name": "Security", "tag": "security", "rooms": [4]}], "duty_managers": [3], "confirm_policy": "author_or_duty_manager"}"#,
        )
        .unwrap();
        let store = SettingsStore::open(&path);
        assert_eq!(store.load_error(), None);
        let settings = store.get();
        assert_eq!(settings.autonomy, Autonomy::default());
        assert_eq!(settings.hermes_fizzy_user_id, None);
        assert_eq!(settings.confirm_policy, Policy::AuthorOrDutyManager, "version 1 values are kept");
        assert_eq!(settings.autonomy.dial(ActionKind::Comment), Dial::Alone);
        assert_eq!(settings.autonomy.dial(ActionKind::Tag), Dial::Alone);
        for kind in [ActionKind::Create, ActionKind::Move, ActionKind::Close, ActionKind::Step] {
            assert_eq!(settings.autonomy.dial(kind), Dial::AskFirst, "{kind:?}");
        }
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 1, "reading doesn't rewrite the file");

        let mut next = (*settings).clone();
        next.autonomy.close = Dial::Never;
        next.hermes_fizzy_user_id = Some(" 03hermes ".into());
        store.save(next).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 3, "the next save upgrades it");
        assert_eq!(json["autonomy"]["close"], "never");
        assert_eq!(json["autonomy"]["move"], "ask_first");
        assert_eq!(json["hermes_fizzy_user_id"], "03hermes");
        assert_eq!(SettingsStore::open(&path).get(), store.get());

        // A partial map keeps the defaults for what it leaves out.
        std::fs::write(&path, r#"{"version": 2, "autonomy": {"create": "alone"}}"#).unwrap();
        let partial = SettingsStore::open(&path).get();
        assert_eq!((partial.autonomy.create, partial.autonomy.comment), (Dial::Alone, Dial::Alone));
        assert_eq!(partial.autonomy.close, Dial::AskFirst);
        // An unknown dial value doesn't validate: the file fails closed (nothing alone).
        std::fs::write(&path, r#"{"version": 2, "autonomy": {"create": "sometimes"}}"#).unwrap();
        let broken = SettingsStore::open(&path);
        assert!(broken.load_error().is_some());
        assert_eq!(broken.get().autonomy, Autonomy::ask_first());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_version_two_file_loads_with_the_phase_2b_defaults() {
        let dir = scratch_dir("v2");
        let path = dir.join("workspace.json");
        std::fs::write(
            &path,
            r#"{"version": 2, "departments": [{"name": "Security", "tag": "security", "rooms": [4]}], "confirm_policy": "anyone",
                "autonomy": {"create": "alone"}, "hermes_fizzy_user_id": null}"#,
        )
        .unwrap();
        let store = SettingsStore::open(&path);
        assert_eq!(store.load_error(), None);
        let settings = store.get();
        assert!(!settings.departments[0].restricted, "not restricted until an administrator says so");
        assert_eq!(settings.visibility, Visibility::default(), "everyone sees every card, as before");
        assert_eq!(settings.notifications, Notifications::default());
        assert!(settings.notifications.enabled && settings.notifications.severities == ["critical", "high"]);
        assert_eq!((settings.notifications.new_reminder_min, settings.notifications.draft_reminder_min), (15, 10));
        assert_eq!(settings.handover, HandoverSettings::default());
        assert_eq!(settings.handover.room_id, None, "no handover room: nothing is posted or reminded");
        assert_eq!(settings.handover.shift_ends, ["07:00", "15:00", "23:00"]);
        assert_eq!(settings.autonomy.create, Dial::Alone, "version 2 values are kept");

        let mut next = (*settings).clone();
        next.departments[0].restricted = true;
        next.visibility.mode = crate::visibility::Mode::ByDepartmentRoom;
        next.handover.room_id = Some(4);
        next.notifications.severities = vec!["critical".into()];
        store.save(next).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 3);
        assert_eq!(json["departments"][0]["restricted"], true);
        assert_eq!(json["visibility"], serde_json::json!({"mode": "by_department_room", "untagged": "everyone"}));
        assert_eq!(json["handover"]["room_id"], 4);
        assert_eq!(json["notifications"]["severities"], serde_json::json!(["critical"]));
        assert_eq!(SettingsStore::open(&path).get(), store.get());

        // Invalid phase 2b values don't validate: the file fails closed (only duty managers see cards).
        for broken in [
            r#"{"version": 3, "visibility": {"mode": "secret"}}"#,
            r#"{"version": 3, "handover": {"shift_ends": ["7h"]}}"#,
            r#"{"version": 3, "notifications": {"severities": ["urgent"]}}"#,
        ] {
            std::fs::write(&path, broken).unwrap();
            let store = SettingsStore::open(&path);
            assert!(store.load_error().is_some(), "{broken}");
            assert_eq!(store.get().visibility, Visibility::closed(), "{broken}");
        }
        // Except a time zone this system can't resolve: UTC, with a warning, the rest as saved.
        std::fs::write(&path, r#"{"version": 3, "visibility": {"mode": "by_department_room"}, "handover": {"time_zone": "Nowhere/Land"}}"#)
            .unwrap();
        let store = SettingsStore::open(&path);
        assert_eq!(store.load_error(), None);
        assert!(store.load_warning().unwrap().contains("“Nowhere/Land”"));
        assert_eq!(store.get().handover.time_zone, "UTC");
        assert_eq!(store.get().visibility.mode, crate::visibility::Mode::ByDepartmentRoom, "not failed closed");
        let mut bad = (*store.get()).clone();
        bad.handover.time_zone = "Nowhere/Land".into();
        assert!(store.save(bad).is_err(), "a save still refuses it");
        store.save((*store.get()).clone()).unwrap();
        assert_eq!(store.load_warning(), None, "saved: nothing read differently any more");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ticket_type_department_tags_are_refused_on_save_but_read_with_a_warning() {
        // The demo's departments are fine.
        let demo = ["front-desk", "housekeeping", "maintenance", "security"].map(|tag| department(tag, tag, &[]));
        assert!(Settings { departments: demo.to_vec(), ..Settings::default() }.validated().is_ok());

        // A file saved before the rule: kept as saved, with a warning, not failed closed.
        let dir = scratch_dir("settings-ticket-types");
        let path = dir.join("workspace.json");
        std::fs::write(
            &path,
            r#"{"version": 3, "departments": [{"name": "Safety", "tag": "safety", "rooms": [3]}, {"name": "Front desk", "tag": "front-desk"}], "handover": {"time_zone": "Nowhere/Land"}}"#,
        )
        .unwrap();
        let store = SettingsStore::open(&path);
        assert_eq!(store.load_error(), None);
        let warning = store.load_warning().unwrap();
        assert!(warning.contains("“Nowhere/Land”") && warning.contains("“safety”") && warning.contains("are ticket types"), "{warning}");
        assert_eq!(store.get().departments.len(), 2, "not failed closed");
        assert_eq!(store.get().department_by_tag("safety").unwrap().rooms, vec![3]);
        let error = store.save((*store.get()).clone()).unwrap_err();
        assert!(matches!(&error, SaveError::Invalid(error) if error.0.contains("“safety” is a ticket type")), "{error:?}");
        let mut fixed = (*store.get()).clone();
        fixed.departments[0].tag = "safety-team".into();
        fixed.handover.time_zone = "UTC".into();
        store.save(fixed).unwrap();
        assert_eq!(store.load_warning(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_hermes_user_id_is_validated() {
        let invalid = Settings { hermes_fizzy_user_id: Some("a b".into()), ..Settings::default() }.validated().unwrap_err();
        assert!(invalid.0.contains("Fizzy user id"));
        let blank = Settings { hermes_fizzy_user_id: Some("  ".into()), ..Settings::default() }.validated().unwrap();
        assert_eq!(blank.hermes_fizzy_user_id, None);
    }

    #[test]
    fn undo_is_for_duty_managers_and_the_person_it_was_for() {
        let (member, person, manager) = (viewer(1, false), viewer(2, false), viewer(3, false));
        for policy in Policy::ALL {
            let settings = Settings { duty_managers: Some(vec![3]), confirm_policy: policy, ..Settings::default() };
            let undo = Act::Undo { for_user_id: Some(2) };
            assert!(!settings.permits(&undo, &member), "{policy:?}: not anyone, even under `anyone`");
            assert!(settings.permits(&undo, &person) && settings.permits(&undo, &manager), "{policy:?}");
            assert!(!settings.permits(&Act::Undo { for_user_id: None }, &person));
        }
        assert!(Act::Undo { for_user_id: None }.refusal(Policy::Anyone).contains("person it was done for"));
    }
}
