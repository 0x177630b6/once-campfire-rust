//! Owners: pings when a ticket's owner changes (from Fizzy, from Sky, from the sheet), the sheet's
//! owner control, and alerts recorded as sent only once posted, against the fake Fizzy.

use std::collections::{BTreeSet, HashMap};

use super::*;
use crate::alerts::{Delivery, To};
use crate::owners::OwnerTarget;
use crate::visibility::{Mode, Untagged, Visibility};

const LIST: &str = "/897/cards.json?board_ids%5B%5D=b1";
const CLOSED: &str = "/897/cards.json?board_ids%5B%5D=b1&indexed_by=closed";
const ACTIVITIES: &str = "/897/activities.json";

fn person(id: i64, name: &str, administrator: bool) -> Viewer {
    Viewer { id, name: name.into(), email: Some(format!("{}@hotel.test", name.to_lowercase())), administrator }
}

/// An administrator: the duty manager (none are listed).
fn boss() -> Viewer {
    person(1, "Manager", true)
}
fn maya() -> Viewer {
    person(5, "Maya", false)
}
fn karim() -> Viewer {
    person(7, "Karim", false)
}
/// Has no Fizzy user.
fn sam() -> Viewer {
    person(8, "Sam", false)
}

/// Engineering (room 3), Security (room 4, restricted); Hermes's Fizzy user named.
fn settings() -> Settings {
    Settings {
        departments: vec![
            Department { name: "Engineering".into(), tag: "engineering".into(), rooms: vec![3], restricted: false },
            Department { name: "Security".into(), tag: "security".into(), rooms: vec![4], restricted: true },
        ],
        visibility: Visibility { mode: Mode::ByDepartmentRoom, untagged: Untagged::Everyone },
        hermes_fizzy_user_id: Some("fz-hermes".into()),
        ..Settings::default()
    }
}

/// Maya and Karim in front-desk (3), Sam in front-desk and security (4), the Manager in neither;
/// Hermes's bot (9) in both. Everyone has an email address.
fn directory() -> Directory {
    let people = [boss(), maya(), karim(), sam()];
    let memberships = [(1, vec![]), (5, vec![3]), (7, vec![3]), (8, vec![3, 4]), (9, vec![3, 4])];
    Directory {
        people: people.iter().map(|p| (p.id, (p.name.clone(), p.administrator))).collect(),
        emails: people.iter().map(|p| (p.id, p.email.clone().unwrap())).collect(),
        memberships: memberships.into_iter().map(|(id, rooms)| (id, rooms.into_iter().collect::<BTreeSet<i64>>())).collect(),
        rooms: HashMap::from([(3, "front-desk".to_string()), (4, "security".to_string())]),
        loaded: true,
    }
}

fn user(id: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "role": "member", "active": true, "email_address": format!("{}@hotel.test", name.to_lowercase())})
}

/// The Fizzy users: no Sam; Lena has no Campfire user.
fn users() -> Value {
    json!([
        user("fz-campfire", "Campfire"),
        user("fz-hermes", "Hermes"),
        user("fz-karim", "Karim"),
        user("fz-lena", "Lena"),
        user("fz-manager", "Manager"),
        user("fz-maya", "Maya")
    ])
}

fn owned(number: u64, title: &str, tags: &[&str], owners: &[&str], active_at: &str) -> Value {
    let mut card = card(number, title, tags, None);
    card["assignees"] = json!(owners.iter().map(|id| user(id, name_of(id))).collect::<Vec<_>>());
    card["last_active_at"] = json!(active_at);
    card
}

/// `fz-maya` → `Maya`.
fn name_of(id: &str) -> &'static str {
    match id {
        "fz-karim" => "Karim",
        "fz-lena" => "Lena",
        "fz-manager" => "Manager",
        "fz-maya" => "Maya",
        other => panic!("no Fizzy user {other}"),
    }
}

/// An owner change in `/activities`.
fn assigned(id: &str, number: u64, assigned: bool, assignee: &str, by: (&str, &str), at: &str) -> Value {
    json!({
        "id": id, "action": if assigned { "card_assigned" } else { "card_unassigned" }, "created_at": at,
        "url": format!("http://localhost:8484/897/cards/{number}"),
        // The card is in the lists already (Fizzy's `eventable` is the whole card, as it is now).
        "eventable_type": "Card.Stub", "eventable": {"number": number},
        "particulars": {"assignee_ids": [assignee]},
        "board": {"id": "b1", "name": "Incident Log"},
        "creator": {"id": by.0, "name": by.1}
    })
}

fn owners_fizzy(cards: &[Value]) -> FakeFizzy {
    let fizzy = fizzy();
    fizzy.reply(LIST, json!(cards));
    fizzy.reply(ACTIVITIES, json!([]));
    fizzy.reply("/897/users.json", users());
    fizzy.identities.lock().unwrap().insert(TOKEN.into(), "fz-campfire".into());
    for card in cards {
        fizzy.live(card.clone());
    }
    fizzy
}

fn initial_cards() -> Vec<Value> {
    vec![
        owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &[], "2026-09-30T08:00:00Z"),
        owned(12, "Lift B out of service", &["security", "sev-high"], &[], "2026-09-30T08:00:00Z"),
        owned(15, "Puddle by the bar", &["sev-low"], &[], "2026-09-30T08:00:00Z"),
    ]
}

async fn pinging(settings: Settings) -> (FakeFizzy, Workspace, Clock) {
    let fizzy = owners_fizzy(&initial_cards());
    let clock: Clock = std::sync::Arc::new(Mutex::new(now()));
    let time = clock.clone();
    let workspace = Workspace::new(config()).with_settings(scratch_store(settings)).with_clock(move || *time.lock().unwrap());
    workspace.set_bots(vec![hermes_bot()]);
    workspace.set_directory(directory());
    (fizzy, workspace, clock)
}

fn at(minutes: i64) -> Timestamp {
    now() + SignedDuration::from_mins(minutes)
}

fn stamp(minutes: i64) -> String {
    at(minutes).to_string()
}

/// A poll at `minutes`, and the messages it calls for (taken: in flight until settled).
async fn poll_at(fizzy: &FakeFizzy, workspace: &Workspace, clock: &Clock, minutes: i64) -> Vec<Delivery> {
    *clock.lock().unwrap() = at(minutes);
    workspace.poll(fizzy, at(minutes)).await.unwrap();
    workspace.take_alerts(Some(9)).unwrap()
}

fn settle(workspace: &Workspace, deliveries: &[Delivery], posted: bool) -> usize {
    let outcomes: Vec<(&Delivery, bool)> = deliveries.iter().map(|delivery| (delivery, posted)).collect();
    workspace.settle_alerts(&outcomes)
}

fn html_for(deliveries: &[Delivery], target: To) -> String {
    deliveries.iter().find(|delivery| delivery.to == target).map(|delivery| delivery.html.clone()).unwrap_or_default()
}

fn to(deliveries: &[Delivery]) -> Vec<To> {
    deliveries.iter().map(|delivery| delivery.to).collect()
}

fn set_cards(fizzy: &FakeFizzy, cards: &[Value], activities: &[Value]) {
    fizzy.reply(LIST, json!(cards));
    fizzy.reply(ACTIVITIES, json!(activities));
}

#[tokio::test]
async fn owner_changes_in_fizzy_ping_the_people_concerned_once() {
    let (fizzy, workspace, clock) = pinging(settings()).await;
    assert!(poll_at(&fizzy, &workspace, &clock, 0).await.is_empty(), "the first poll ever only records the owners");
    assert!(workspace.notified().owners().unwrap().cards.contains_key(&13));

    // Karim makes Maya the owner of 13, in Fizzy.
    let mut cards = initial_cards();
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(1));
    set_cards(&fizzy, &cards, &[assigned("a1", 13, true, "fz-maya", ("fz-karim", "Karim"), &stamp(1))]);
    let sent = poll_at(&fizzy, &workspace, &clock, 2).await;
    assert_eq!(to(&sent), [To::Person(5)], "Maya only");
    let html = html_for(&sent, To::Person(5));
    assert_eq!(
        html,
        r#"<p><strong>Karim made you owner</strong> of #13 — Guest slip in lobby<br><a href="/workspace/cards/13">Open ticket #13</a></p>"#
    );
    assert!(!html.contains("fizzy"), "never a Fizzy link");
    assert_eq!(sent[0].keys.len(), 1);

    // In flight: not again. Not posted: tried again (to Maya only), then recorded once posted.
    assert!(poll_at(&fizzy, &workspace, &clock, 3).await.is_empty());
    assert!(!workspace.notified().contains(&sent[0].keys[0]), "not recorded before it's posted");
    assert_eq!(settle(&workspace, &sent, false), 0);
    let again = poll_at(&fizzy, &workspace, &clock, 4).await;
    assert_eq!(to(&again), [To::Person(5)]);
    assert_eq!(again[0].html, html);
    settle(&workspace, &again, true);
    assert!(workspace.notified().contains(&sent[0].keys[0]));
    assert!(poll_at(&fizzy, &workspace, &clock, 5).await.is_empty(), "once");

    // Karim takes it over himself: Maya is told who has it; Karim isn't told about his own action.
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-karim"], &stamp(6));
    set_cards(
        &fizzy,
        &cards,
        &[
            assigned("a3", 13, true, "fz-karim", ("fz-karim", "Karim"), &stamp(6)),
            assigned("a2", 13, false, "fz-maya", ("fz-karim", "Karim"), &stamp(6)),
        ],
    );
    let sent = poll_at(&fizzy, &workspace, &clock, 7).await;
    assert_eq!(to(&sent), [To::Person(5)]);
    assert_eq!(
        html_for(&sent, To::Person(5)),
        r#"<p><strong>Karim took over</strong> <a href="/workspace/cards/13">#13</a> from you.</p>"#
    );
    settle(&workspace, &sent, true);

    // The Manager gives it to Maya in Fizzy: Maya made owner, Karim told it went to Maya.
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(8));
    set_cards(
        &fizzy,
        &cards,
        &[
            assigned("a5", 13, true, "fz-maya", ("fz-manager", "Manager"), &stamp(8)),
            assigned("a4", 13, false, "fz-karim", ("fz-manager", "Manager"), &stamp(8)),
        ],
    );
    let sent = poll_at(&fizzy, &workspace, &clock, 9).await;
    assert_eq!(to(&sent), [To::Person(5), To::Person(7)]);
    assert!(html_for(&sent, To::Person(5)).contains("<strong>Manager made you owner</strong> of #13"));
    assert_eq!(
        html_for(&sent, To::Person(7)),
        r#"<p><strong>Manager gave</strong> <a href="/workspace/cards/13">#13</a> <strong>to Maya</strong>.</p>"#
    );
    settle(&workspace, &sent, true);

    // Maya lets go of it herself: nobody is told. Then she takes it back herself: nobody either.
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &[], &stamp(10));
    set_cards(&fizzy, &cards, &[assigned("a6", 13, false, "fz-maya", ("fz-maya", "Maya"), &stamp(10))]);
    assert!(poll_at(&fizzy, &workspace, &clock, 11).await.is_empty());
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(12));
    set_cards(&fizzy, &cards, &[assigned("a7", 13, true, "fz-maya", ("fz-maya", "Maya"), &stamp(12))]);
    assert!(poll_at(&fizzy, &workspace, &clock, 13).await.is_empty());

    // Removed by someone else, no new owner; and no activity found: no name.
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &[], &stamp(14));
    set_cards(&fizzy, &cards, &[]);
    let sent = poll_at(&fizzy, &workspace, &clock, 15).await;
    assert_eq!(
        html_for(&sent, To::Person(5)),
        r#"<p><strong>You’re no longer the owner</strong> of <a href="/workspace/cards/13">#13</a>.</p>"#
    );
    settle(&workspace, &sent, true);
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(16));
    set_cards(&fizzy, &cards, &[assigned("a8", 13, true, "fz-maya", ("fz-karim", "Karim"), &stamp(16))]);
    settle(&workspace, &poll_at(&fizzy, &workspace, &clock, 17).await, true);
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &[], &stamp(18));
    set_cards(&fizzy, &cards, &[assigned("a9", 13, false, "fz-maya", ("fz-karim", "Karim"), &stamp(18))]);
    let sent = poll_at(&fizzy, &workspace, &clock, 19).await;
    assert_eq!(
        html_for(&sent, To::Person(5)),
        r#"<p><strong>Karim removed you as owner</strong> of <a href="/workspace/cards/13">#13</a>.</p>"#
    );
}

#[tokio::test]
async fn sky_restricted_cards_closed_cards_and_people_without_campfire_users() {
    let (fizzy, workspace, clock) = pinging(settings()).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;

    // Sky gives Security's 12 to Maya, who can't see it: no title, no link.
    let mut cards = initial_cards();
    cards[1] = owned(12, "Lift B out of service", &["security", "sev-high"], &["fz-maya"], &stamp(1));
    // Lena (no Campfire user) gets 15: nobody to tell.
    cards[2] = owned(15, "Puddle by the bar", &["sev-low"], &["fz-lena"], &stamp(1));
    set_cards(
        &fizzy,
        &cards,
        &[
            assigned("b1", 12, true, "fz-maya", ("fz-hermes", "Hermes"), &stamp(1)),
            assigned("b2", 15, true, "fz-lena", ("fz-karim", "Karim"), &stamp(1)),
        ],
    );
    let sent = poll_at(&fizzy, &workspace, &clock, 2).await;
    assert_eq!(to(&sent), [To::Person(5)]);
    assert_eq!(
        html_for(&sent, To::Person(5)),
        "<p><strong>Sky made you owner</strong> of #12. You can’t open it in Meshduty: ask a duty manager.</p>"
    );
    let lena = format!("owner:15:+:fz-lena:{}", at(1).as_millisecond());
    assert!(workspace.notified().contains(&lena), "Lena's recorded: nobody to send it to");
    settle(&workspace, &sent, true);

    // A closed card's owner change: nothing (and it's no longer known).
    let mut closed = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(3));
    closed["closed"] = json!(true);
    set_cards(&fizzy, &cards[1..], &[assigned("b3", 13, true, "fz-maya", ("fz-karim", "Karim"), &stamp(3))]);
    fizzy.reply(CLOSED, json!([closed]));
    assert!(poll_at(&fizzy, &workspace, &clock, 4).await.is_empty());
    assert!(!workspace.notified().owners().unwrap().cards.contains_key(&13));

    // A new card, filed and given an owner between two polls: pinged.
    let mut new = owned(16, "Smoke in kitchen", &["engineering", "sev-low"], &["fz-karim"], &stamp(5));
    new["created_at"] = json!(stamp(5));
    let mut list = cards[1..].to_vec();
    list.push(new);
    set_cards(&fizzy, &list, &[assigned("b4", 16, true, "fz-karim", ("fz-manager", "Manager"), &stamp(5))]);
    let sent = poll_at(&fizzy, &workspace, &clock, 6).await;
    assert_eq!(to(&sent), [To::Person(7)]);
    assert!(html_for(&sent, To::Person(7)).contains("<strong>Manager made you owner</strong> of #16 — Smoke in kitchen"));
}

#[tokio::test]
async fn owner_pings_survive_a_restart_and_follow_the_setting() {
    let (fizzy, workspace, clock) = pinging(settings()).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;

    // While Campfire is down, Karim gives 13 to Maya; and 15 becomes critical.
    let mut cards = initial_cards();
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(1));
    cards[2] = owned(15, "Puddle by the bar", &["sev-critical"], &[], &stamp(1));
    set_cards(&fizzy, &cards, &[assigned("c1", 13, true, "fz-maya", ("fz-karim", "Karim"), &stamp(1))]);
    let restarted = Workspace::new(workspace.config().clone()).with_settings(scratch_store(settings())).with_clock(move || at(5));
    restarted.set_bots(vec![hermes_bot()]);
    restarted.set_directory(directory());
    restarted.poll(&fizzy, at(5)).await.unwrap();
    let sent = restarted.take_alerts(Some(9)).unwrap();
    assert_eq!(to(&sent), [To::Person(5)], "the owner ping, compared with the owners known before; not the alert (first poll)");
    assert!(sent[0].html.contains("Karim made you owner"));

    // Owner pings off: recorded, not sent; back on: nothing stale.
    let mut off = settings();
    off.notifications.owner_pings = false;
    let (fizzy, workspace, clock) = pinging(off.clone()).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    set_cards(&fizzy, &cards, &[assigned("c1", 13, true, "fz-maya", ("fz-karim", "Karim"), &stamp(1))]);
    assert!(poll_at(&fizzy, &workspace, &clock, 2).await.iter().all(|delivery| !delivery.html.contains("owner")));
    off.notifications.owner_pings = true;
    workspace.settings_store().save(off).unwrap();
    assert!(poll_at(&fizzy, &workspace, &clock, 3).await.iter().all(|delivery| !delivery.html.contains("owner")));

    // A change found long after it happened (over 2 hours): not pinged.
    let (fizzy, workspace, clock) = pinging(settings()).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    cards[0] = owned(13, "Guest slip in lobby", &["engineering", "sev-critical"], &["fz-maya"], &stamp(1));
    set_cards(&fizzy, &cards, &[]);
    assert!(poll_at(&fizzy, &workspace, &clock, 200).await.iter().all(|delivery| !delivery.html.contains("owner")));
}

#[tokio::test]
async fn alerts_are_recorded_only_once_posted_and_given_up_after_repeated_failures() {
    let mut settings = settings();
    settings.notifications.new_reminder_min = 0;
    let (fizzy, workspace, clock) = pinging(settings).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    let mut cards = initial_cards();
    let mut smoke = owned(16, "Smoke in kitchen", &["engineering", "sev-critical"], &[], &stamp(1));
    smoke["created_at"] = json!(stamp(1));
    cards.push(smoke);
    set_cards(&fizzy, &cards, &[]);
    let sent = poll_at(&fizzy, &workspace, &clock, 2).await;
    assert_eq!(to(&sent), [To::Person(1), To::Room(3)], "the duty manager and Engineering's room");
    // The room's post fails, the DM's works: only the room is tried again.
    let outcomes = [(&sent[0], true), (&sent[1], false)];
    assert_eq!(workspace.settle_alerts(&outcomes), 0);
    assert!(!workspace.notified().contains("sev:16:critical"));
    let mut retried = poll_at(&fizzy, &workspace, &clock, 3).await;
    assert_eq!(to(&retried), [To::Room(3)]);
    for attempt in 2..crate::alerts::MAX_ATTEMPTS {
        settle(&workspace, &retried, false);
        retried = poll_at(&fizzy, &workspace, &clock, 3 + attempt as i64).await;
        assert_eq!(to(&retried), [To::Room(3)], "attempt {attempt}");
    }
    assert_eq!(settle(&workspace, &retried, false), 1, "given up");
    assert!(workspace.notified().contains("sev:16:critical"));
    assert!(poll_at(&fizzy, &workspace, &clock, 30).await.is_empty());
}

// --- The sheet's owner control ----------------------------------------------------------------------

async fn sheet_workspace(settings: Settings) -> (FakeFizzy, Workspace) {
    let (fizzy, workspace, clock) = pinging(settings).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    fizzy.requests.lock().unwrap().clear();
    (fizzy, workspace)
}

fn owners_of(fizzy: &FakeFizzy, number: u64) -> Vec<String> {
    fizzy.live_card(number)["assignees"].as_array().unwrap().iter().map(|user| user["id"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn take_it_assign_to_and_remove() {
    let (fizzy, workspace) = sheet_workspace(settings()).await;

    // Maya takes 13: she's its owner; nobody is pinged about their own action.
    let card = workspace.set_owner(&fizzy, &maya(), 13, OwnerTarget::Me).await.unwrap();
    assert_eq!(card.assignees.iter().map(|user| user.id.as_str()).collect::<Vec<_>>(), ["fz-maya"]);
    assert_eq!(owners_of(&fizzy, 13), ["fz-maya"]);
    let writes = fizzy.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].1, "/897/cards/13/assignments.json");
    assert_eq!(writes[0].2, Some(json!({"assignee_id": "fz-maya"})), "her Fizzy user, never self_assignment (that'd be Campfire's)");
    assert!(workspace.take_alerts(Some(9)).unwrap().is_empty());
    let record = workspace.journal().recent().into_iter().last().unwrap();
    assert_eq!((record.user_id, record.action.as_str(), record.via.as_str()), (5, "owner +fz-maya", "workspace"), "the real actor, logged");

    // The Manager gives it to Karim: one owner (Maya removed, Karim added), both told at once.
    workspace.set_owner(&fizzy, &boss(), 13, OwnerTarget::Person(7)).await.unwrap();
    assert_eq!(owners_of(&fizzy, 13), ["fz-karim"]);
    let sent = workspace.take_alerts(Some(9)).unwrap();
    assert_eq!(to(&sent), [To::Person(5), To::Person(7)]);
    assert!(html_for(&sent, To::Person(7)).starts_with("<p><strong>Manager made you owner</strong> of #13 — Guest slip in lobby"));
    assert_eq!(
        html_for(&sent, To::Person(5)),
        r#"<p><strong>Manager gave</strong> <a href="/workspace/cards/13">#13</a> <strong>to Karim</strong>.</p>"#
    );
    settle(&workspace, &sent, true);

    // The next poll sees the same change in Fizzy (by "Campfire"): not pinged again.
    let cards = vec![fizzy.live_card(13), fizzy.live_card(12), fizzy.live_card(15)];
    set_cards(
        &fizzy,
        &cards,
        &[
            assigned("d2", 13, true, "fz-karim", ("fz-campfire", "Campfire"), fizzy.live_card(13)["last_active_at"].as_str().unwrap()),
            assigned("d1", 13, false, "fz-maya", ("fz-campfire", "Campfire"), fizzy.live_card(13)["last_active_at"].as_str().unwrap()),
        ],
    );
    workspace.poll(&fizzy, at(1)).await.unwrap();
    assert!(workspace.take_alerts(Some(9)).unwrap().is_empty());

    // Maya takes it back (anyone may take a ticket): Karim is told she took over.
    workspace.set_owner(&fizzy, &maya(), 13, OwnerTarget::Me).await.unwrap();
    let sent = workspace.take_alerts(Some(9)).unwrap();
    assert_eq!(to(&sent), [To::Person(7)]);
    assert_eq!(html_for(&sent, To::Person(7)), r#"<p><strong>Maya took over</strong> <a href="/workspace/cards/13">#13</a> from you.</p>"#);
    settle(&workspace, &sent, true);

    // Karim can't give it to someone else, nor remove Maya; Maya may remove herself.
    let refused = workspace.set_owner(&fizzy, &karim(), 13, OwnerTarget::Person(5)).await.unwrap_err();
    assert!(matches!(&refused, ActionError::Forbidden(message) if message.contains("someone else")), "{refused:?}");
    assert!(matches!(workspace.set_owner(&fizzy, &karim(), 13, OwnerTarget::Nobody).await, Err(ActionError::Forbidden(_))));
    workspace.set_owner(&fizzy, &maya(), 13, OwnerTarget::Nobody).await.unwrap();
    assert!(owners_of(&fizzy, 13).is_empty());
    assert!(workspace.take_alerts(Some(9)).unwrap().is_empty(), "her own action");

    // The Manager removes an owner: they're told.
    workspace.set_owner(&fizzy, &boss(), 13, OwnerTarget::Person(7)).await.unwrap();
    settle(&workspace, &workspace.take_alerts(Some(9)).unwrap(), true);
    workspace.set_owner(&fizzy, &boss(), 13, OwnerTarget::Nobody).await.unwrap();
    let sent = workspace.take_alerts(Some(9)).unwrap();
    assert_eq!(
        html_for(&sent, To::Person(7)),
        r#"<p><strong>Manager removed you as owner</strong> of <a href="/workspace/cards/13">#13</a>.</p>"#
    );
}

#[tokio::test]
async fn who_can_own_a_ticket() {
    let (fizzy, workspace) = sheet_workspace(settings()).await;

    // Sam has no Fizzy user: he can't take it, nor be given it, and the settings name him.
    let error = workspace.set_owner(&fizzy, &sam(), 13, OwnerTarget::Me).await.unwrap_err();
    assert_eq!(error.code(), "no_fizzy_user");
    assert_eq!(workspace.set_owner(&fizzy, &boss(), 13, OwnerTarget::Person(8)).await.unwrap_err().code(), "not_a_candidate");
    assert_eq!(workspace.people_without_fizzy_user(), Some(vec!["Sam".to_string()]));
    // Security's 12: Maya can't see it, so she can't be given it; Karim can't take what he can't see.
    assert_eq!(workspace.set_owner(&fizzy, &boss(), 12, OwnerTarget::Person(5)).await.unwrap_err().code(), "not_a_candidate");
    assert_eq!(workspace.set_owner(&fizzy, &karim(), 12, OwnerTarget::Me).await.unwrap_err(), ActionError::NotFound);
    assert!(fizzy.writes().is_empty(), "nothing written");
    let names = |number| workspace.owner_candidates(&fizzy_tags(&fizzy, number)).into_iter().map(|c| c.name).collect::<Vec<_>>();
    assert_eq!(names(13), ["Karim", "Manager", "Maya"], "people with a Fizzy user who may see it");
    assert_eq!(names(12), ["Manager"], "Security's: only who may see it (Sam has no Fizzy user)");

    // Fizzy refuses someone without access to the board: said plainly, and the owner stays (the
    // new one is added before the old one is removed).
    workspace.set_owner(&fizzy, &maya(), 13, OwnerTarget::Me).await.unwrap();
    fizzy.reply("/897/users.json", json!([user("fz-maya", "Maya")]));
    let error = workspace.set_owner(&fizzy, &boss(), 13, OwnerTarget::Person(7)).await.unwrap_err();
    assert!(error.message().contains("no access to the incident board"), "{error}");
    assert_eq!(owners_of(&fizzy, 13), ["fz-maya"], "not left without an owner");
    assert!(workspace.take_alerts(Some(9)).unwrap().is_empty(), "nobody pinged: nothing changed");

    // An owner Fizzy won't remove (deactivated: its 404) doesn't stop the change; it's reported.
    fizzy.reply("/897/users.json", users());
    let mut card = fizzy.live_card(13);
    card["assignees"] = json!([user("fz-gone", "Gone"), user("fz-maya", "Maya")]);
    fizzy.live(card);
    let error = workspace.set_owner(&fizzy, &boss(), 13, OwnerTarget::Person(7)).await.unwrap_err();
    assert!(error.message().contains("won’t remove Gone"), "{error}");
    assert_eq!(owners_of(&fizzy, 13), ["fz-gone", "fz-karim"], "Karim added, Maya removed");
    let sent = workspace.take_alerts(Some(9)).unwrap();
    assert_eq!(to(&sent), [To::Person(5), To::Person(7)], "the change that happened is pinged");

    // A closed card: no owner changes.
    let mut closed = fizzy.live_card(15);
    closed["closed"] = json!(true);
    fizzy.live(closed);
    assert_eq!(workspace.set_owner(&fizzy, &maya(), 15, OwnerTarget::Me).await.unwrap_err().code(), "closed_card");

    // Duty managers only: taking a ticket too.
    let mut strict = settings();
    strict.confirm_policy = Policy::DutyManagersOnly;
    workspace.settings_store().save(strict).unwrap();
    assert!(matches!(workspace.set_owner(&fizzy, &maya(), 13, OwnerTarget::Me).await, Err(ActionError::Forbidden(_))));
}

fn fizzy_tags(fizzy: &FakeFizzy, number: u64) -> Vec<String> {
    fizzy.live_card(number)["tags"].as_array().unwrap().iter().map(|tag| tag.as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn the_sheet_shows_the_owner_control_to_whom_it_applies() {
    let (fizzy, workspace) = sheet_workspace(settings()).await;
    workspace.set_owner(&fizzy, &maya(), 13, OwnerTarget::Me).await.unwrap();

    let sheet = workspace.card_sheet(&fizzy, &boss(), 13).await.unwrap();
    assert!(sheet.owner.can_take && sheet.owner.can_remove);
    let choices: Vec<(String, bool)> = sheet.owner.candidates.iter().map(|c| (c.label.clone(), c.selected)).collect();
    assert_eq!(choices, [("Karim".into(), false), ("Manager".into(), false), ("Maya".into(), true)]);
    let html = askama::Template::render(&sheet).unwrap();
    for part in [r#"data-ws-owner-set="me""#, r#"data-ws-owner-set="none""#, r#"data-ws-change="owner""#, "Assign to…", "Maya (owner)"] {
        assert!(html.contains(part), "{part}: {html}");
    }
    assert!(html.contains(r#"data-ws-sheet="13""#) && html.contains("data-ws-status"), "the sheet's contract stays");

    // Maya owns it: she may let go of it, not take it again; no picker.
    let sheet = workspace.card_sheet(&fizzy, &maya(), 13).await.unwrap();
    assert!(!sheet.owner.can_take && sheet.owner.can_remove && sheet.owner.candidates.is_empty());
    // Karim may take it, not remove Maya.
    let sheet = workspace.card_sheet(&fizzy, &karim(), 13).await.unwrap();
    assert!(sheet.owner.can_take && !sheet.owner.can_remove && sheet.owner.candidates.is_empty());
    // Sam has no Fizzy user: a hint instead.
    let sheet = workspace.card_sheet(&fizzy, &sam(), 13).await.unwrap();
    assert!(!sheet.owner.can_take && sheet.owner.hint.as_deref().unwrap().contains("Fizzy user"));
    let html = askama::Template::render(&sheet).unwrap();
    assert!(html.contains("You can’t own tickets yet") && html.contains("Maya"), "{html}");

    // Under "duty managers only", a member sees the owner, no control.
    let mut strict = settings();
    strict.confirm_policy = Policy::DutyManagersOnly;
    workspace.settings_store().save(strict).unwrap();
    let sheet = workspace.card_sheet(&fizzy, &karim(), 13).await.unwrap();
    assert!(sheet.owner.is_empty());
    let html = askama::Template::render(&sheet).unwrap();
    assert!(html.contains("Maya") && !html.contains("data-ws-owner"), "{html}");
}

#[tokio::test]
async fn the_picker_offers_only_people_with_access_to_the_board() {
    let (fizzy, workspace, clock) = pinging(settings()).await;
    // Karim has a Fizzy user but no access to the board (not an all-access board).
    let access = |id: &str, name: &str, has: bool| {
        let mut user = user(id, name);
        user["has_access"] = json!(has);
        user
    };
    fizzy.reply(
        "/897/boards/b1/accesses.json",
        json!({"board_id": "b1", "all_access": false, "users": [
            access("fz-karim", "Karim", false), access("fz-manager", "Manager", true), access("fz-maya", "Maya", true)
        ]}),
    );
    poll_at(&fizzy, &workspace, &clock, 0).await;
    let names: Vec<String> = workspace.owner_candidates(&["engineering".into()]).into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["Manager", "Maya"]);
    assert_eq!(workspace.people_without_fizzy_user(), Some(vec!["Karim".to_string(), "Sam".to_string()]));
    assert_eq!(workspace.set_owner(&fizzy, &karim(), 13, OwnerTarget::Me).await.unwrap_err().code(), "no_fizzy_user");
}
