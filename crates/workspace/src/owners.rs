//! Ticket owners: owner pings, and the sheet's owner control (docs/hermes-workspace.md, "Owners").
//!
//! A ticket's **owner** is Fizzy's assignee. Fizzy allows several per card; the app manages one
//! (decision O6): setting an owner from the sheet replaces the others, and a card Fizzy gives
//! several shows them all.
//!
//! **Pings.** At every poll, [`detect`] compares each open card's owners with the ones last known
//! ([`KnownOwners`], kept in `notified.json`, so a restart compares with what was known before it):
//!
//! - someone added: "Karim made you owner of #57 — Guest slip in lobby", with a link to the card's
//!   sheet in the app (never Fizzy);
//! - someone removed: "Karim removed you as owner of #57", or, when someone else has it now, "Karim
//!   gave #57 to Maya" ("Maya took over #57" when Maya took it herself).
//!
//! Who did it comes from the card's `card_assigned` / `card_unassigned` activities (Sky for
//! Hermes's Fizzy user); with none found, the message says no name. Nobody is pinged about their
//! own action (taking a ticket, letting go of it). Closed cards are skipped, and so is a change
//! older than [`MAX_AGE`] (found after a long downtime). A change from the sheet is known at once:
//! `Workspace::owners_changed` queues its pings with the person who made it as the actor,
//! and records the card's new owners so that the next poll doesn't ping again.
//!
//! The recipient is the Campfire person with the Fizzy owner's email address (unverified, as for
//! mentions: decision D12). Someone with no Fizzy user can't be an owner; someone with no Campfire
//! user (or two people sharing the address) gets nothing. A recipient who may not see the card
//! (a restricted department, phase 2.7) gets the ping without its title or link. Pings go out
//! through the alerts' path ([`crate::alerts`]): one message per person per poll, a direct message
//! from Sky, so Campfire's Web Push applies, and each is sent once ([`crate::alerts::NotifiedStore`],
//! recorded after the message is posted). `notifications.owner_pings` (default on) turns them off;
//! so does `notifications.enabled`.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Workspace;
use crate::alerts::{Event, EventKind};
use crate::cache::Snapshot;
use crate::fizzy::{Card, HttpClient, UserRef};
use crate::home::Viewer;
use crate::settings::Act;
use crate::writes::ActionError;

/// A change found later than this after it happened (Campfire was down) isn't pinged.
pub const MAX_AGE: SignedDuration = SignedDuration::from_hours(2);
/// A known card missing from a poll's picture keeps its owners this long after its last activity.
const KEEP_MISSING: SignedDuration = SignedDuration::from_hours(24);

/// An owner as last known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownOwner {
    /// The Fizzy user id.
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Lowercase.
    #[serde(default)]
    pub email: Option<String>,
}

impl KnownOwner {
    fn of(user: &UserRef) -> Self {
        let email = user.email_address.as_deref().map(|email| email.trim().to_lowercase()).filter(|email| !email.is_empty());
        Self { id: user.id.clone(), name: user.name.clone(), email }
    }
}

/// A card's owners as last known, and the card's `last_active_at` then (an older read of the card
/// isn't compared with them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownCard {
    pub owners: Vec<KnownOwner>,
    #[serde(default)]
    pub active_at: Option<Timestamp>,
}

/// The open cards' owners, as last known (in `notified.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownOwners {
    /// When they were recorded: an open card unknown then, created since, starts with no owner.
    pub at: Option<Timestamp>,
    #[serde(default)]
    pub cards: BTreeMap<u64, KnownCard>,
}

/// One person to ping about one card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerPing {
    pub card: u64,
    /// The Fizzy user it's about, and their email address (lowercase), to find their Campfire user.
    pub person: KnownOwner,
    pub change: OwnerChange,
    /// Who did it, as people know them ("Karim", "Sky"); `None`: unknown.
    pub actor: Option<String>,
    /// The Campfire person who did it from the sheet: never pinged about their own action.
    pub actor_user: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerChange {
    /// They're the card's owner now.
    Added,
    /// They aren't any more. `now`: who is (their names); `taken`: one of them did it.
    Removed { now: Vec<String>, taken: bool },
}

/// Hermes's and the workspace's Fizzy users, to name an actor.
#[derive(Debug, Clone, Default)]
pub struct Actors {
    pub hermes: Option<String>,
    pub workspace: Option<String>,
}

impl Actors {
    /// The workspace's own Fizzy user (not shared with Hermes, an older setup).
    fn is_workspace(&self, id: &str) -> bool {
        self.workspace.as_deref() == Some(id) && self.hermes != self.workspace
    }

    /// How people know a Fizzy actor: Hermes is "Sky"; the workspace's own user ("Campfire") means
    /// someone in the app, unknown here (the sheet names them itself).
    fn name(&self, id: &str, name: &str) -> Option<String> {
        if self.hermes.as_deref() == Some(id) {
            Some("Sky".into())
        } else if self.workspace.as_deref() == Some(id) || name.trim().is_empty() {
            None
        } else {
            Some(name.trim().to_string())
        }
    }
}

/// The pings this poll's picture calls for, compared with `known`, and the owners to know from now
/// on. `known` absent (the first poll ever): nothing to ping, only to record.
pub fn detect(known: Option<&KnownOwners>, next: &Snapshot, actors: &Actors, now: Timestamp) -> (Vec<Event>, KnownOwners) {
    let mut events = Vec::new();
    let mut cards = BTreeMap::new();
    for card in next.open.iter().filter_map(|number| next.card(*number)).filter(|card| !card.closed) {
        let current = KnownCard { owners: card.assignees.iter().map(KnownOwner::of).collect(), active_at: card.last_active_at };
        let Some(known) = known else {
            cards.insert(card.number, current);
            continue;
        };
        let (before, since) = match known.cards.get(&card.number) {
            // This read is older than what's known (the sheet changed it meanwhile): keep that.
            Some(before) if before.active_at.zip(card.last_active_at).is_some_and(|(seen, now)| now < seen) => {
                cards.insert(card.number, before.clone());
                continue;
            }
            Some(before) => (before.owners.clone(), before.active_at),
            // New since the owners were recorded: it had nobody. Otherwise (reopened…): only record.
            None if known.at.is_some_and(|at| card.created_at.is_some_and(|created| created > at)) => (Vec::new(), None),
            None => {
                cards.insert(card.number, current);
                continue;
            }
        };
        let fresh = card.last_active_at.is_none_or(|active| now.duration_since(active) <= MAX_AGE);
        if fresh {
            events.extend(changes(card, &before, &current.owners, since, next, actors));
        }
        cards.insert(card.number, current);
    }
    // A known card missing from this picture altogether (its lookup failed, an odd list entry)
    // keeps its owners for a while, rather than coming back as "only recorded" and missing a
    // change. A closed card is in the picture, so it does drop out.
    if let Some(known) = known {
        for (number, card) in &known.cards {
            let recent = card.active_at.is_none_or(|active| now.duration_since(active) <= KEEP_MISSING);
            if next.card(*number).is_none() && recent {
                cards.entry(*number).or_insert_with(|| card.clone());
            }
        }
    }
    (events, KnownOwners { at: Some(now), cards })
}

/// The pings for one card whose owners went from `before` (as of `since`, the card's
/// `last_active_at` then) to `after`, named from its activities since.
fn changes(
    card: &Card,
    before: &[KnownOwner],
    after: &[KnownOwner],
    since: Option<Timestamp>,
    snapshot: &Snapshot,
    actors: &Actors,
) -> Vec<Event> {
    let added: Vec<&KnownOwner> = after.iter().filter(|owner| !before.iter().any(|known| known.id == owner.id)).collect();
    let removed: Vec<&KnownOwner> = before.iter().filter(|owner| !after.iter().any(|now| now.id == owner.id)).collect();
    // The newest activity since `before` that added (or removed) this person on this card: who did it.
    let activity = |id: &str, assigned: bool| {
        snapshot.assignments.iter().find(|a| {
            a.card_number == card.number
                && a.assigned == assigned
                && a.assignees.iter().any(|x| x == id)
                && since.is_none_or(|since| a.created_at >= since)
        })
    };
    // Made from the sheet (the workspace's own Fizzy user): the sheet pings, naming who did it.
    let from_sheet = |by: Option<&crate::cache::Assignment>| by.is_some_and(|by| actors.is_workspace(&by.actor_id));
    let mut events = Vec::new();
    for person in &added {
        let by = activity(&person.id, true);
        if by.is_some_and(|by| by.actor_id == person.id) || from_sheet(by) {
            continue; // They took it themselves.
        }
        let actor = by.and_then(|by| actors.name(&by.actor_id, &by.actor_name));
        let ping = OwnerPing { card: card.number, person: (*person).clone(), change: OwnerChange::Added, actor, actor_user: None };
        events.push(event(ping, card.last_active_at));
    }
    for person in &removed {
        let by = activity(&person.id, false);
        if by.is_some_and(|by| by.actor_id == person.id) || from_sheet(by) {
            continue; // They let go of it themselves.
        }
        let taken = by.is_some_and(|by| added.iter().any(|new| new.id == by.actor_id));
        let now = after.iter().map(|owner| owner.name.clone()).collect();
        let actor = by.and_then(|by| actors.name(&by.actor_id, &by.actor_name));
        let ping = OwnerPing {
            card: card.number,
            person: (*person).clone(),
            change: OwnerChange::Removed { now, taken },
            actor,
            actor_user: None,
        };
        events.push(event(ping, card.last_active_at));
    }
    events
}

/// The event for a ping, keyed by card, person, direction and the card's `last_active_at` (every
/// Fizzy event moves it): one ping per change, whatever the number of polls that see it.
pub(crate) fn event(ping: OwnerPing, active_at: Option<Timestamp>) -> Event {
    let sign = match ping.change {
        OwnerChange::Added => '+',
        OwnerChange::Removed { .. } => '-',
    };
    let at = active_at.map(|at| at.as_millisecond().to_string()).unwrap_or_default();
    Event { key: format!("owner:{}:{sign}:{}:{at}", ping.card, ping.person.id), kind: EventKind::Owner(ping) }
}

/// The sheet's owner change (`POST /workspace/cards/:n/owner` `{"owner": "me" | "none" | "<user
/// id>"}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerTarget {
    /// "Take it": the actor.
    Me,
    /// "Remove": nobody.
    Nobody,
    /// "Assign to…": a Campfire person.
    Person(i64),
}

impl OwnerTarget {
    pub fn parse(params: &Value) -> Result<Self, ActionError> {
        let value = match params.get("owner") {
            Some(Value::String(value)) => value.trim().to_string(),
            Some(Value::Number(number)) => number.to_string(),
            _ => String::new(),
        };
        match value.as_str() {
            "me" => Ok(Self::Me),
            "none" | "" if params.get("owner").is_some() => Ok(Self::Nobody),
            id => id
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .map(Self::Person)
                .ok_or_else(|| ActionError::invalid("invalid_owner", "Pick who should own the ticket.")),
        }
    }
}

/// Someone the sheet may make the owner: a Campfire person with a Fizzy user who may see the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub user_id: i64,
    pub name: String,
    pub fizzy_id: String,
}

impl Workspace {
    /// The Campfire people with a Fizzy user (same email address) who may see a card with `tags`,
    /// by name: the sheet's "Assign to…" list.
    pub fn owner_candidates(&self, tags: &[String]) -> Vec<Candidate> {
        let (snapshot, settings, directory) = (self.snapshot(), self.settings(), self.directory());
        let mut candidates: Vec<Candidate> = directory
            .people
            .iter()
            .filter_map(|(id, (name, _))| {
                let fizzy = snapshot.fizzy_person_by_email(directory.emails.get(id)?)?;
                let viewer = directory.viewer(*id)?;
                settings.card_visible(tags, &settings.audience(&viewer, directory.rooms_of(*id))).then(|| Candidate {
                    user_id: *id,
                    name: name.clone(),
                    fizzy_id: fizzy.id.clone(),
                })
            })
            .collect();
        candidates.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.user_id.cmp(&b.user_id)));
        candidates
    }

    /// The sheet's owner field for `viewer` ([`crate::pages::OwnerControl`]). Nothing on a closed
    /// card.
    pub(crate) fn owner_control(&self, viewer: &Viewer, card: &Card) -> crate::pages::OwnerControl {
        let mut control = crate::pages::OwnerControl::default();
        if card.closed {
            return control;
        }
        let (may_take, may_assign) = (self.may(&Act::Assign { own: true }, viewer), self.may(&Act::Assign { own: false }, viewer));
        let mine = self.fizzy_id_of(viewer);
        let sole_owner = |id: &str| card.assignees.len() == 1 && card.assignees[0].id == id;
        control.can_take = may_take && mine.as_deref().is_some_and(|mine| !sole_owner(mine));
        control.can_remove = !card.assignees.is_empty()
            && (may_assign || (may_take && mine.as_deref().is_some_and(|mine| card.assignees.iter().all(|owner| owner.id == mine))));
        if may_assign {
            let owner = card.assignees.first().map(|owner| owner.id.as_str());
            control.candidates = self
                .owner_candidates(&card.tags)
                .into_iter()
                .map(|candidate| {
                    let selected = Some(candidate.fizzy_id.as_str()) == owner;
                    crate::pages::Choice::new(candidate.user_id.to_string(), candidate.name, selected)
                })
                .collect();
        }
        if may_take && mine.is_none() && self.snapshot().fizzy_people.is_some() {
            control.hint = Some("You can’t own tickets yet: you need a Fizzy user with your email address (ask an administrator).".into());
        }
        control
    }

    /// The active people with no Fizzy user of the same email address (by name), for the settings
    /// page: they can't be owners, nor get owner pings. `None` until Fizzy's users were read.
    pub fn people_without_fizzy_user(&self) -> Option<Vec<String>> {
        let (snapshot, directory) = (self.snapshot(), self.directory());
        snapshot.fizzy_people.as_ref()?;
        let mut names: Vec<String> = directory
            .people
            .iter()
            .filter(|(id, _)| directory.emails.get(id).is_none_or(|email| snapshot.fizzy_person_by_email(email).is_none()))
            .map(|(_, (name, _))| name.clone())
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        Some(names)
    }

    /// `viewer`'s Fizzy user (same email address), if Fizzy's users were read.
    fn fizzy_id_of(&self, viewer: &Viewer) -> Option<String> {
        let email = viewer.email.as_deref()?;
        self.snapshot().fizzy_person_by_email(email).map(|user| user.id.clone())
    }

    /// Fizzy's users, read now if the poll hasn't yet (or not lately): the sheet needs them to
    /// write an owner.
    async fn ensure_fizzy_people(&self, http: &dyn HttpClient) -> Result<(), ActionError> {
        let snapshot = self.snapshot();
        let now = self.now();
        let fresh = snapshot.fizzy_people.is_some()
            && snapshot.fizzy_people_at.is_some_and(|at| now.duration_since(at) < crate::cache::FIZZY_PEOPLE_REFRESH);
        if fresh {
            return Ok(());
        }
        let (account, board) = self.target()?;
        let client = crate::fizzy::Client::new(http, &self.config);
        match client.board_people(&account, &board.id).await {
            Ok(users) => {
                self.set_fizzy_people(users, now);
                Ok(())
            }
            Err(_) if snapshot.fizzy_people.is_some() => Ok(()),
            Err(_) => Err(ActionError::Fizzy("Fizzy’s people can’t be read right now; try again in a moment.".into())),
        }
    }

    fn set_fizzy_people(&self, users: Vec<UserRef>, at: Timestamp) {
        let mut snapshot = self.snapshot.write().unwrap_or_else(|e| e.into_inner());
        let mut next = (**snapshot).clone();
        next.fizzy_people = Some(users);
        next.fizzy_people_at = Some(at);
        *snapshot = std::sync::Arc::new(next);
    }

    /// Sets card `number`'s owner from the sheet, as `actor` (who must see the card): one owner,
    /// replacing the others, or none. "Take it" (`Me`) and removing oneself are the actor's own
    /// business ([`Act::Assign`] `own`); anything else is for duty managers. The new owner must have
    /// a Fizzy user and see the card. Then the pings go out at once (`Workspace::owners_changed`).
    pub async fn set_owner(&self, http: &dyn HttpClient, actor: &Viewer, number: u64, target: OwnerTarget) -> Result<Card, ActionError> {
        // Whatever the target, at least taking a ticket must be allowed (checked again below).
        self.authorize(&Act::Assign { own: true }, actor)?;
        if matches!(target, OwnerTarget::Person(id) if id != actor.id) {
            self.authorize(&Act::Assign { own: false }, actor)?;
        }
        self.ensure_fizzy_people(http).await?;
        let writer = self.writer(http, actor)?;
        let _lock = self.lock_card(number).await;
        let before = writer.card(number).await?;
        let settings = self.settings();
        if !settings.card_visible(&before.tags, &self.audience_of(actor)) {
            return Err(ActionError::NotFound);
        }
        if before.closed {
            return Err(ActionError::invalid("closed_card", "This ticket is closed: reopen it first."));
        }
        let mine = self.fizzy_id_of(actor);
        let wanted = match target {
            OwnerTarget::Nobody => None,
            OwnerTarget::Me => Some(mine.clone().ok_or_else(|| {
                ActionError::invalid("no_fizzy_user", "You don’t have a Fizzy user yet, so you can’t own tickets. Ask an administrator.")
            })?),
            OwnerTarget::Person(id) if id == actor.id => Some(mine.clone().ok_or_else(|| {
                ActionError::invalid("no_fizzy_user", "You don’t have a Fizzy user yet, so you can’t own tickets. Ask an administrator.")
            })?),
            OwnerTarget::Person(id) => {
                let candidate = self.owner_candidates(&before.tags).into_iter().find(|candidate| candidate.user_id == id);
                Some(
                    candidate
                        .ok_or_else(|| {
                            ActionError::invalid(
                                "not_a_candidate",
                                "That person can’t own this ticket: they need a Fizzy user, and to be able to see the ticket.",
                            )
                        })?
                        .fizzy_id,
                )
            }
        };
        // Removing: only one's own ownership is one's own business.
        let own = match (&target, &wanted) {
            (OwnerTarget::Nobody, _) => before.assignees.iter().all(|owner| Some(&owner.id) == mine.as_ref()),
            (_, Some(wanted)) => Some(wanted) == mine.as_ref(),
            _ => false,
        };
        if !own {
            self.authorize(&Act::Assign { own: false }, actor)?;
        }
        let result = writer.set_owner(number, wanted.as_deref()).await;
        let after = match &result {
            Ok(card) => card.clone(),
            // Half done (a toggle failed): the card as it is now, for the pings and the sheet.
            Err(_) => match writer.card(number).await {
                Ok(card) => card,
                Err(_) => return result,
            },
        };
        self.remember(after.clone());
        self.owners_changed(&before, &after, actor, mine.as_deref());
        result
    }

    /// A card's owners changed from the sheet (`before` → `after`, by `actor`, whose Fizzy user is
    /// `actor_fizzy`): its pings, queued at once (the app delivers them right away), and its new
    /// owners recorded, so the next poll doesn't ping again.
    pub(crate) fn owners_changed(&self, before: &Card, after: &Card, actor: &Viewer, actor_fizzy: Option<&str>) {
        let known_before: Vec<KnownOwner> = before.assignees.iter().map(KnownOwner::of).collect();
        let known_after: Vec<KnownOwner> = after.assignees.iter().map(KnownOwner::of).collect();
        let added: Vec<&KnownOwner> = known_after.iter().filter(|owner| !known_before.iter().any(|b| b.id == owner.id)).collect();
        let mut events = Vec::new();
        for person in &added {
            if Some(person.id.as_str()) == actor_fizzy {
                continue;
            }
            let ping = OwnerPing {
                card: after.number,
                person: (*person).clone(),
                change: OwnerChange::Added,
                actor: Some(actor.name.clone()),
                actor_user: Some(actor.id),
            };
            events.push(event(ping, after.last_active_at));
        }
        for person in known_before.iter().filter(|owner| !known_after.iter().any(|a| a.id == owner.id)) {
            if Some(person.id.as_str()) == actor_fizzy {
                continue;
            }
            let taken = added.iter().any(|new| Some(new.id.as_str()) == actor_fizzy);
            let now = known_after.iter().map(|owner| owner.name.clone()).collect();
            let ping = OwnerPing {
                card: after.number,
                person: person.clone(),
                change: OwnerChange::Removed { now, taken },
                actor: Some(actor.name.clone()),
                actor_user: Some(actor.id),
            };
            events.push(event(ping, after.last_active_at));
        }
        let card = KnownCard { owners: known_after, active_at: after.last_active_at };
        self.notified.update_owners(|known| {
            if !after.closed {
                known.cards.insert(after.number, card);
            }
        });
        self.queue_owner_events(events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Assignment;
    use crate::fizzy::BoardRef;

    fn at(minutes: i64) -> Timestamp {
        "2026-09-30T09:00:00Z".parse::<Timestamp>().unwrap() + SignedDuration::from_mins(minutes)
    }

    fn user(id: &str) -> UserRef {
        UserRef { id: id.into(), name: id.to_uppercase(), email_address: Some(format!("{id}@hotel.test")) }
    }

    fn snapshot(owners: &[&str], active: i64, assignments: Vec<Assignment>) -> Snapshot {
        let board = BoardRef { id: "b1".into(), name: "Incidents".into() };
        let card = Card {
            number: 7,
            title: "Leak".into(),
            board: Some(board),
            assignees: owners.iter().map(|id| user(id)).collect(),
            created_at: Some(at(-60)),
            last_active_at: Some(at(active)),
            ..Card::default()
        };
        Snapshot { open: vec![7], cards: [(7, card)].into(), assignments, ..Snapshot::default() }
    }

    fn known(owners: &[&str], active: i64) -> KnownOwners {
        let card = KnownCard { owners: owners.iter().map(|id| KnownOwner::of(&user(id))).collect(), active_at: Some(at(active)) };
        KnownOwners { at: Some(at(active)), cards: [(7, card)].into() }
    }

    fn assignment(assigned: bool, who: &str, by: &str, minutes: i64) -> Assignment {
        Assignment {
            id: format!("{who}{by}{minutes}"),
            card_number: 7,
            assigned,
            assignees: vec![who.into()],
            actor_id: by.into(),
            actor_name: by.to_uppercase(),
            created_at: at(minutes),
        }
    }

    fn pings(events: &[Event]) -> Vec<(String, OwnerChange, Option<String>)> {
        events
            .iter()
            .map(|event| match &event.kind {
                EventKind::Owner(ping) => (ping.person.id.clone(), ping.change.clone(), ping.actor.clone()),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn detection() {
        let actors = Actors { hermes: Some("hermes".into()), workspace: Some("campfire".into()) };
        // Nothing known yet: only recorded.
        let (events, recorded) = detect(None, &snapshot(&["maya"], 1, vec![]), &actors, at(2));
        assert!(events.is_empty() && recorded.cards[&7].owners[0].id == "maya");

        // Added by Sky; the event key carries the card's activity time.
        let next = snapshot(&["maya"], 1, vec![assignment(true, "maya", "hermes", 1)]);
        let (events, _) = detect(Some(&known(&[], 0)), &next, &actors, at(2));
        assert_eq!(pings(&events), [("maya".into(), OwnerChange::Added, Some("Sky".into()))]);
        assert_eq!(events[0].key, format!("owner:7:+:maya:{}", at(1).as_millisecond()));

        // Assigned and unassigned between two polls: nothing changed, nothing said.
        let next = snapshot(&[], 1, vec![assignment(true, "maya", "karim", 1), assignment(false, "maya", "karim", 1)]);
        assert!(detect(Some(&known(&[], 0)), &next, &actors, at(2)).0.is_empty());

        // From the sheet (the workspace's user): the sheet pings, not the poll.
        let next = snapshot(&["maya"], 1, vec![assignment(true, "maya", "campfire", 1)]);
        assert!(detect(Some(&known(&[], 0)), &next, &actors, at(2)).0.is_empty());
        // Unless Campfire and Hermes share one Fizzy user (an older setup): then it's Sky.
        let shared = Actors { hermes: Some("campfire".into()), workspace: Some("campfire".into()) };
        assert_eq!(pings(&detect(Some(&known(&[], 0)), &next, &shared, at(2)).0)[0].2.as_deref(), Some("Sky"));

        // A read older than what's known (the sheet just changed it): the known owners stay.
        let (events, recorded) = detect(Some(&known(&["karim"], 5)), &snapshot(&[], 1, vec![]), &actors, at(6));
        assert!(events.is_empty());
        assert_eq!(recorded.cards[&7].owners[0].id, "karim");

        // A card missing from a poll's picture keeps its known owners; back, its change is pinged.
        let empty = Snapshot { open: vec![], ..Snapshot::default() };
        let (_, kept) = detect(Some(&known(&["karim"], 5)), &empty, &actors, at(6));
        assert_eq!(kept.cards[&7].owners[0].id, "karim");
        let next = snapshot(&["maya"], 7, vec![]);
        assert_eq!(pings(&detect(Some(&kept), &next, &actors, at(8)).0).len(), 2, "Maya added, Karim removed");

        // Reassigned by Karim to Maya: Maya added, Lena told it's Maya's now (not "taken").
        let next = snapshot(&["maya"], 3, vec![assignment(true, "maya", "karim", 3), assignment(false, "lena", "karim", 3)]);
        let found = pings(&detect(Some(&known(&["lena"], 0)), &next, &actors, at(4)).0);
        assert_eq!(found[1], ("lena".into(), OwnerChange::Removed { now: vec!["MAYA".into()], taken: false }, Some("KARIM".into())));
    }

    #[test]
    fn owner_targets() {
        let parse = |value: Value| OwnerTarget::parse(&value).map_err(|error| error.code());
        assert_eq!(parse(serde_json::json!({"owner": "me"})), Ok(OwnerTarget::Me));
        assert_eq!(parse(serde_json::json!({"owner": "none"})), Ok(OwnerTarget::Nobody));
        assert_eq!(parse(serde_json::json!({"owner": ""})), Ok(OwnerTarget::Nobody));
        assert_eq!(parse(serde_json::json!({"owner": "12"})), Ok(OwnerTarget::Person(12)));
        assert_eq!(parse(serde_json::json!({"owner": 12})), Ok(OwnerTarget::Person(12)));
        assert_eq!(parse(serde_json::json!({"owner": "-1"})), Err("invalid_owner"));
        assert_eq!(parse(serde_json::json!({"owner": "maya"})), Err("invalid_owner"));
        assert_eq!(parse(serde_json::json!({})), Err("invalid_owner"));
    }
}
