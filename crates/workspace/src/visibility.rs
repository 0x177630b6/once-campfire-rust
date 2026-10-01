//! Phase 2.7: who sees which incident cards in Campfire (decision D2, as the owner chose it).
//!
//! A setting ([`Visibility`], `settings.visibility`):
//!
//! - `everyone` (the default, what phases 0 to 2a do): every signed-in user sees every card of the
//!   incident board;
//! - `by_department_room`: the cards of a department marked **restricted** (`restricted: true` in
//!   the settings, e.g. Security or HR) are seen only by the members of that department's linked
//!   rooms, the duty managers and the administrators. A card carrying several restricted
//!   departments is seen by the members of any one of them. Cards of departments that aren't
//!   restricted stay visible to everyone. Cards with no department (decision D3) are visible to
//!   everyone, or, with `untagged: "duty_managers"`, to the duty managers (and administrators) only.
//!
//! Applied through [`Audience`] everywhere the workspace shows card data: chips (filled in per
//! viewer by the page, from `cards.json`), Home, the board, a room's panel, the card sheet and its
//! changes, mentions, the Hermes tab, proposals, alert recipients and the handover. It only hides
//! cards **in Campfire**: anyone with a Fizzy login sees the whole board in Fizzy, and a room's
//! members are whoever its creator (or an administrator) added (hence "only administrators can
//! create rooms").

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::home::Viewer;
use crate::settings::{Settings, SettingsError, normalize_tag};

/// `settings.visibility`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Visibility {
    pub mode: Mode,
    /// Cards with no department tag (decision D3).
    pub untagged: Untagged,
}

impl Visibility {
    /// A damaged settings file: no departments are known any more, so every card is "untagged"
    /// and only the duty managers see them, rather than a restricted department's cards leaking.
    pub fn closed() -> Self {
        Self { mode: Mode::ByDepartmentRoom, untagged: Untagged::DutyManagers }
    }

    pub fn restricts(&self) -> bool {
        self.mode == Mode::ByDepartmentRoom
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Everyone signed in sees every card (phases 0 to 2a).
    #[default]
    Everyone,
    /// Restricted departments' cards: their linked rooms' members, duty managers, administrators.
    ByDepartmentRoom,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Everyone, Mode::ByDepartmentRoom];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Everyone => "everyone",
            Mode::ByDepartmentRoom => "by_department_room",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Everyone => "Everyone sees every card",
            Mode::ByDepartmentRoom => {
                "Restricted departments’ cards: only the members of their linked rooms, the duty managers and the administrators"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Untagged {
    /// Decision D3: visible to all, listed as "No department" on the duty managers' Home.
    #[default]
    Everyone,
    DutyManagers,
}

impl Untagged {
    pub const ALL: [Untagged; 2] = [Untagged::Everyone, Untagged::DutyManagers];

    pub fn as_str(self) -> &'static str {
        match self {
            Untagged::Everyone => "everyone",
            Untagged::DutyManagers => "duty_managers",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Untagged::Everyone => "Everyone (they’re listed as “No department” on the duty managers’ Home)",
            Untagged::DutyManagers => "Duty managers and administrators only",
        }
    }
}

/// Who is looking, for visibility: everything (the `everyone` mode, a duty manager, an
/// administrator), or what the rooms they're a member of allow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audience {
    pub all: bool,
    pub rooms: BTreeSet<i64>,
}

impl Audience {
    /// Sees every card.
    pub fn everything() -> Self {
        Self { all: true, rooms: BTreeSet::new() }
    }
}

impl Settings {
    /// `viewer`'s audience, given the rooms they're a member of.
    pub fn audience(&self, viewer: &Viewer, rooms: impl IntoIterator<Item = i64>) -> Audience {
        let all = !self.visibility.restricts() || viewer.administrator || self.is_duty_manager(viewer);
        Audience { all, rooms: if all { BTreeSet::new() } else { rooms.into_iter().collect() } }
    }

    /// Whether a card with these tags is visible to `audience` (see the module's rules).
    pub fn card_visible(&self, tags: &[String], audience: &Audience) -> bool {
        if audience.all || !self.visibility.restricts() {
            return true;
        }
        let departments = self.departments_of(tags);
        if departments.is_empty() {
            return self.visibility.untagged == Untagged::Everyone;
        }
        let restricted: Vec<_> = departments.iter().filter(|department| department.restricted).collect();
        restricted.is_empty() || restricted.iter().any(|department| department.rooms.iter().any(|room| audience.rooms.contains(room)))
    }

    /// The rooms whose every member may see a card with these tags: where a notice about it can be
    /// posted. A card of restricted departments: their linked rooms; otherwise all its departments'
    /// linked rooms. None for a card without a department.
    pub fn notice_rooms(&self, tags: &[String]) -> Vec<i64> {
        let departments = self.departments_of(tags);
        let restricted: Vec<_> = departments.iter().filter(|department| department.restricted).collect();
        let chosen: Vec<_> = if self.visibility.restricts() && !restricted.is_empty() {
            restricted.into_iter().copied().collect()
        } else {
            departments.clone()
        };
        let mut rooms: Vec<i64> = Vec::new();
        for department in chosen {
            for room in &department.rooms {
                if !rooms.contains(room) {
                    rooms.push(*room);
                }
            }
        }
        rooms
    }

    /// A restricted department can't be linked to an open room (Campfire's `Rooms::Open`): every
    /// user is a member of it, so its cards would be visible to everyone. `open_rooms`: `(id, name)`.
    pub fn check_open_rooms(&self, open_rooms: &[(i64, String)]) -> Result<(), SettingsError> {
        for department in self.departments.iter().filter(|department| department.restricted) {
            if let Some((_, room)) = open_rooms.iter().find(|(id, _)| department.rooms.contains(id)) {
                return Err(SettingsError(format!(
                    "“{}” is restricted, so it can’t be linked to “{room}”: that room is open to everyone, so everyone would see its cards. Link a room with chosen members instead, or don’t restrict the department.",
                    department.name
                )));
            }
        }
        Ok(())
    }

    /// Any department is restricted (for the settings page's warning).
    pub fn has_restricted_departments(&self) -> bool {
        self.departments.iter().any(|department| department.restricted)
    }
}

/// The tags of a proposed new card read from its request (`department`, `departments` and `tags`,
/// which [`crate::actions::NewCard::parse`] maps to departments too), for a proposal stored without
/// its resolved departments ([`crate::proposals::Proposal::new_card_tags`]). Only department tags
/// matter to visibility, so the other tags do no harm.
pub fn proposed_tags(request: &serde_json::Value) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    if let Some(tag) = request.get("department").and_then(serde_json::Value::as_str) {
        tags.push(normalize_tag(tag));
    }
    for key in ["departments", "tags"] {
        if let Some(list) = request.get(key).and_then(serde_json::Value::as_array) {
            tags.extend(list.iter().filter_map(serde_json::Value::as_str).map(normalize_tag));
        }
    }
    tags.retain(|tag| !tag.is_empty());
    tags
}

/// Who is in which room, and who the administrators are, as the app last read them (every poll):
/// alert recipients, the room panel of the layout (which can't wait on the database) and the
/// handover room's audience.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    /// Active people (not bots): id → (name, administrator).
    pub people: HashMap<i64, (String, bool)>,
    /// User id (people and bots) → the rooms they're a member of.
    pub memberships: HashMap<i64, BTreeSet<i64>>,
    /// Room id → its name, for notices.
    pub rooms: HashMap<i64, String>,
    /// Active people's email addresses (lowercase), for owner pings: a Fizzy owner is matched to a
    /// Campfire person by address ([`crate::owners`]).
    pub emails: HashMap<i64, String>,
    /// Whether it has been read at least once.
    pub loaded: bool,
}

impl Directory {
    pub fn administrators(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.people.iter().filter(|(_, (_, admin))| *admin).map(|(id, _)| *id).collect();
        ids.sort();
        ids
    }

    pub fn viewer(&self, id: i64) -> Option<Viewer> {
        self.people.get(&id).map(|(name, administrator)| Viewer {
            id,
            name: name.clone(),
            email: self.emails.get(&id).cloned(),
            administrator: *administrator,
        })
    }

    /// The active person with this email address (any case). Two people sharing an address (which
    /// Campfire allows) match nobody: an owner ping can't tell them apart.
    pub fn person_by_email(&self, email: &str) -> Option<i64> {
        let email = email.trim().to_lowercase();
        if email.is_empty() {
            return None;
        }
        let mut matches = self.emails.iter().filter(|(id, address)| **address == email && self.people.contains_key(id)).map(|(id, _)| *id);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    pub fn rooms_of(&self, user_id: i64) -> BTreeSet<i64> {
        self.memberships.get(&user_id).cloned().unwrap_or_default()
    }

    pub fn is_member(&self, user_id: i64, room_id: i64) -> bool {
        self.memberships.get(&user_id).is_some_and(|rooms| rooms.contains(&room_id))
    }

    /// The active people who are members of `room_id`.
    pub fn members_of(&self, room_id: i64) -> Vec<i64> {
        let mut ids: Vec<i64> =
            self.people.keys().copied().filter(|id| self.memberships.get(id).is_some_and(|rooms| rooms.contains(&room_id))).collect();
        ids.sort();
        ids
    }

    /// The duty managers who are active people (`settings.duty_managers`, else the administrators).
    pub fn duty_managers(&self, settings: &Settings) -> Vec<i64> {
        let mut ids: Vec<i64> =
            settings.duty_manager_ids(&self.administrators()).into_iter().filter(|id| self.people.contains_key(id)).collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Department;

    fn viewer(id: i64, administrator: bool) -> Viewer {
        Viewer { id, name: format!("user {id}"), email: None, administrator }
    }

    fn settings(mode: Mode, untagged: Untagged) -> Settings {
        Settings {
            departments: vec![
                Department { name: "Engineering".into(), tag: "engineering".into(), rooms: vec![3], restricted: false },
                Department { name: "Security".into(), tag: "security".into(), rooms: vec![4, 5], restricted: true },
                Department { name: "HR".into(), tag: "hr".into(), rooms: vec![6], restricted: true },
            ],
            duty_managers: Some(vec![9]),
            visibility: Visibility { mode, untagged },
            ..Settings::default()
        }
    }

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|tag| tag.to_string()).collect()
    }

    #[test]
    fn everyone_sees_everything_by_default() {
        let settings = Settings::default();
        let audience = settings.audience(&viewer(1, false), []);
        assert!(audience.all);
        assert!(settings.card_visible(&tags(&["security"]), &audience));
    }

    #[test]
    fn the_visibility_matrix() {
        let restricted = settings(Mode::ByDepartmentRoom, Untagged::Everyone);
        let member_of = |rooms: &[i64]| restricted.audience(&viewer(1, false), rooms.iter().copied());
        let (nobody, engineering, security, hr) = (member_of(&[]), member_of(&[3]), member_of(&[5]), member_of(&[6]));
        let manager = restricted.audience(&viewer(9, false), []);
        let administrator = restricted.audience(&viewer(2, true), []);
        assert!(manager.all && administrator.all);
        for (card, expected) in [
            (tags(&["engineering", "sev-high"]), [true, true, true, true]),
            (tags(&["security"]), [false, false, true, false]),
            (tags(&["#Security", "engineering"]), [false, false, true, false]),
            (tags(&["security", "hr"]), [false, false, true, true]),
            (tags(&["sev-low", "incident"]), [true, true, true, true]),
        ] {
            let seen = [&nobody, &engineering, &security, &hr].map(|audience| restricted.card_visible(&card, audience));
            assert_eq!(seen, expected, "{card:?}");
            assert!(restricted.card_visible(&card, &manager) && restricted.card_visible(&card, &administrator));
        }

        let closed_untagged = settings(Mode::ByDepartmentRoom, Untagged::DutyManagers);
        let member = closed_untagged.audience(&viewer(1, false), [3]);
        assert!(!closed_untagged.card_visible(&tags(&["incident"]), &member));
        assert!(closed_untagged.card_visible(&tags(&["engineering"]), &member));
        assert!(closed_untagged.card_visible(&tags(&["incident"]), &closed_untagged.audience(&viewer(9, false), [])));

        let open = settings(Mode::Everyone, Untagged::DutyManagers);
        assert!(open.card_visible(&tags(&["security"]), &open.audience(&viewer(1, false), [])), "the mode decides");
    }

    #[test]
    fn a_damaged_file_hides_everything_but_from_duty_managers() {
        let settings = Settings::fail_closed();
        let member = settings.audience(&viewer(1, false), [3]);
        assert!(!settings.card_visible(&tags(&["security", "sev-high"]), &member));
        assert!(settings.card_visible(&tags(&["security"]), &settings.audience(&viewer(2, true), [])));
    }

    #[test]
    fn notices_go_where_everyone_may_see_the_card() {
        let restricted = settings(Mode::ByDepartmentRoom, Untagged::Everyone);
        assert_eq!(restricted.notice_rooms(&tags(&["engineering"])), [3]);
        assert_eq!(restricted.notice_rooms(&tags(&["engineering", "security"])), [4, 5]);
        assert_eq!(restricted.notice_rooms(&tags(&["security", "hr"])), [4, 5, 6]);
        assert!(restricted.notice_rooms(&tags(&["incident"])).is_empty());
        let open = settings(Mode::Everyone, Untagged::Everyone);
        assert_eq!(open.notice_rooms(&tags(&["engineering", "security"])), [3, 4, 5]);
    }

    #[test]
    fn a_restricted_department_can_t_be_linked_to_an_open_room() {
        let settings = settings(Mode::ByDepartmentRoom, Untagged::Everyone);
        let error = settings.check_open_rooms(&[(5, "Lobby".into())]).unwrap_err();
        assert!(error.0.contains("“Security” is restricted") && error.0.contains("“Lobby”"), "{error}");
        assert!(settings.check_open_rooms(&[(3, "Everyone".into())]).is_ok(), "Engineering isn't restricted");
        assert!(settings.check_open_rooms(&[]).is_ok());
    }

    #[test]
    fn proposed_departments_are_read_from_the_request() {
        let request =
            serde_json::json!({"action": "create", "department": "#Security", "departments": ["hr", ""], "tags": ["Engineering"]});
        assert_eq!(proposed_tags(&request), ["security", "hr", "engineering"]);
    }
}
