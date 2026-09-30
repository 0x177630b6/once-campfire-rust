//! Phase 2 batch 2: visibility by department room (2.7), alerts and reminders (2.5), the
//! end-of-shift handover (2.6), against the fake Fizzy.

use std::collections::{BTreeSet, HashMap};

use super::*;
use crate::alerts::To;
use crate::visibility::{Mode, Untagged, Visibility};

const LIST: &str = "/897/cards.json?board_ids%5B%5D=b1";
const LIST_PAGE2: &str = "/897/cards.json?board_ids%5B%5D=b1&page=2";

/// Rooms of the viewer, no messages.
struct Rooms(Vec<i64>);

impl ChatSource for Rooms {
    fn recent_messages(&self, _since: Timestamp) -> BoxFuture<'_, Result<Vec<ChatMessage>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn room_ids(&self) -> BoxFuture<'_, Result<Vec<i64>, String>> {
        let rooms = self.0.clone();
        Box::pin(async move { Ok(rooms) })
    }
}

fn sam() -> Viewer {
    Viewer { id: 8, name: "Sam".into(), email: None, administrator: false }
}

fn maya_by_email() -> Viewer {
    Viewer { id: 5, name: "Maya".into(), email: Some("maya@hotel.test".into()), administrator: false }
}

/// Engineering (room 3) and Security (room 4, restricted); visibility by department room.
fn restricted() -> Settings {
    Settings {
        departments: vec![
            Department { name: "Engineering".into(), tag: "engineering".into(), rooms: vec![3], restricted: false },
            Department { name: "Security".into(), tag: "security".into(), rooms: vec![4], restricted: true },
        ],
        visibility: Visibility { mode: Mode::ByDepartmentRoom, untagged: Untagged::Everyone },
        ..Settings::default()
    }
}

/// Karim and Maya are in front-desk (3), Sam in front-desk and security (4), the Manager (an
/// administrator) in neither; Hermes's bot (9) in both.
fn directory() -> Directory {
    let people = [(1, "Manager", true), (5, "Maya", false), (7, "Karim", false), (8, "Sam", false)];
    let memberships = [(1, vec![]), (5, vec![3]), (7, vec![3]), (8, vec![3, 4]), (9, vec![3, 4])];
    Directory {
        people: people.iter().map(|(id, name, admin)| (*id, (name.to_string(), *admin))).collect(),
        memberships: memberships.into_iter().map(|(id, rooms)| (id, rooms.into_iter().collect::<BTreeSet<i64>>())).collect(),
        rooms: HashMap::from([(3, "front-desk".to_string()), (4, "security".to_string())]),
        loaded: true,
    }
}

fn card_at(number: u64, title: &str, tags: &[&str], created_at: &str) -> Value {
    let mut card = card(number, title, tags, None);
    card["created_at"] = json!(created_at);
    card["last_active_at"] = json!(created_at);
    card
}

/// 13 engineering, 12 security, 14 both, 15 no department; all live too.
fn tagged_fizzy() -> FakeFizzy {
    let fizzy = fizzy();
    let cards = [
        card(13, "Guest slip in lobby", &["engineering", "sev-critical"], None),
        card(12, "Lift B out of service", &["security", "sev-high"], Some("In progress")),
        card(14, "Door forced, service entrance", &["engineering", "security", "sev-medium"], None),
        card(15, "Puddle by the bar", &["sev-low"], None),
    ];
    fizzy.reply_paged(LIST, json!([cards[0]]));
    fizzy.reply(LIST_PAGE2, json!(cards[1..]));
    for card in cards {
        fizzy.live(card);
    }
    fizzy
}

async fn restricted_workspace(settings: Settings) -> (FakeFizzy, Workspace) {
    let fizzy = tagged_fizzy();
    let workspace = Workspace::new(config()).with_settings(scratch_store(settings)).with_clock(now);
    workspace.set_bots(vec![hermes_bot()]);
    workspace.set_directory(directory());
    workspace.poll(&fizzy, now()).await.unwrap();
    fizzy.requests.lock().unwrap().clear();
    (fizzy, workspace)
}

fn keys(chips: BTreeMap<u64, String>) -> Vec<u64> {
    chips.into_keys().collect()
}

fn home_numbers(home: &HomeView) -> Vec<u64> {
    let mut numbers: Vec<u64> = home.open.iter().flat_map(|group| group.cards.iter().map(|card| card.number)).collect();
    numbers.sort();
    numbers
}

fn board_numbers(board: &crate::pages::BoardView) -> Vec<u64> {
    let mut numbers: Vec<u64> = board.columns.iter().flat_map(|column| column.cards.iter().map(|card| card.number)).collect();
    numbers.sort();
    numbers
}

#[tokio::test]
async fn restricted_departments_are_hidden_on_every_surface() {
    let (fizzy, workspace) = restricted_workspace(restricted()).await;

    // Chips, per viewer: Karim (front-desk only) doesn't get Security's.
    assert_eq!(keys(workspace.chips_for(&karim(), &[12, 13, 14, 15])), [13, 15]);
    assert_eq!(keys(workspace.chips_for(&sam(), &[12, 13, 14, 15])), [12, 13, 14, 15]);
    assert_eq!(keys(workspace.chips_for(&manager(), &[12, 13, 14, 15])), [12, 13, 14, 15], "administrators see everything");
    assert_eq!(workspace.chip_for(&karim(), 12), None);

    // The message HTML is shared by every viewer: links only marked, no card data in it.
    let body = r#"<p><a href="http://fizzy/897/cards/12">http://fizzy/897/cards/12</a></p>"#;
    let decorated = workspace.decorate_message(1, 7, body).unwrap();
    assert!(decorated.contains(r#"data-ws-card="12""#) && !decorated.contains("Lift B") && !decorated.contains("ws-chip"), "{decorated}");

    // Home: open incidents, the 12 h handover list, mentions.
    let home = workspace.home(&karim(), &Rooms(vec![3]), now()).await.unwrap();
    assert_eq!(home_numbers(&home), [13, 15]);
    assert_eq!(home.open_count, 2);
    assert!(home.handover.iter().all(|card| ![12, 14].contains(&card.number)));
    let html = askama::Template::render(&home).unwrap();
    assert!(!html.contains("Lift B") && !html.contains("Door forced"), "{html}");
    assert!(home.handover_url.is_none() && home.no_department.is_empty(), "duty managers only");
    let maya = workspace.home(&maya_by_email(), &Rooms(vec![3]), now()).await.unwrap();
    assert!(maya.mentions.is_empty(), "the mention is on a Security card");
    let maya_in_security = workspace.home(&maya_by_email(), &Rooms(vec![3, 4]), now()).await.unwrap();
    assert_eq!(maya_in_security.mentions.len(), 1);
    let manager_home = workspace.home(&manager(), &Rooms(vec![]), now()).await.unwrap();
    assert_eq!(home_numbers(&manager_home), [12, 13, 14, 15]);
    assert_eq!(manager_home.no_department.iter().map(|card| card.number).collect::<Vec<_>>(), [15], "decision D3");
    assert_eq!(manager_home.handover_url.as_deref(), Some("/workspace/handover"));

    // The board and a room's panel.
    workspace.set_rooms_of(7, [3]);
    assert_eq!(board_numbers(&workspace.board(&karim(), &BoardFilter::default())), [13, 15]);
    assert_eq!(board_numbers(&workspace.board(&sam(), &BoardFilter::default())), [12, 13, 14, 15]);
    let panel = |viewer: &Viewer| workspace.room_panel(viewer, 3).unwrap().cards.iter().map(|card| card.number).collect::<Vec<_>>();
    assert_eq!(panel(&karim()), [13], "14 is Security's too");
    assert_eq!(panel(&sam()), [13, 14]);

    // The card sheet and changes: as good as missing, nothing written.
    assert_eq!(workspace.card_sheet(&fizzy, &karim(), 12).await.unwrap_err(), ActionError::NotFound);
    assert!(workspace.card_sheet(&fizzy, &sam(), 12).await.is_ok());
    let refused = workspace.change_card(&fizzy, &karim(), 12, Change::Comment("seen".into())).await.unwrap_err();
    assert_eq!(refused, ActionError::NotFound);
    assert!(fizzy.writes().is_empty(), "nothing written for a hidden card");
    workspace.change_card(&fizzy, &sam(), 12, Change::Comment("seen".into())).await.unwrap();
    workspace.change_card(&fizzy, &karim(), 13, Change::Comment("on it".into())).await.unwrap();

    // Membership comes from the directory, refreshed by the requests: Karim joins #security.
    workspace.set_rooms_of(7, [3, 4]);
    assert_eq!(keys(workspace.chips_for(&karim(), &[12, 14])), [12, 14]);
}

#[tokio::test]
async fn untagged_cards_and_the_everyone_mode() {
    let mut settings = restricted();
    settings.visibility.untagged = Untagged::DutyManagers;
    let (_, workspace) = restricted_workspace(settings).await;
    assert_eq!(keys(workspace.chips_for(&karim(), &[13, 15])), [13], "no department: duty managers only");
    assert_eq!(keys(workspace.chips_for(&manager(), &[13, 15])), [13, 15]);

    let mut everyone = restricted();
    everyone.visibility.mode = Mode::Everyone;
    workspace.settings_store().save(everyone).unwrap();
    assert_eq!(keys(workspace.chips_for(&karim(), &[12, 13, 14, 15])), [12, 13, 14, 15], "the default: nothing hidden");
    let body = r#"<p><a href="http://fizzy/897/cards/12">x</a></p>"#;
    assert!(workspace.decorate_message(1, 7, body).unwrap().contains("Lift B"), "chips rendered in the HTML, as before");
}

#[tokio::test]
async fn the_hermes_tab_and_proposals_follow_visibility() {
    let mut settings = restricted();
    settings.autonomy.comment = Dial::AskFirst;
    let (fizzy, workspace) = restricted_workspace(settings).await;
    let bot = hermes_bot();
    let on_security_card = json!({"action": "comment", "card": 12, "body": "Camera footage requested"});
    let security = json!({"action": "create", "title": "Badge cloned", "department": "security"});
    let engineering = json!({"action": "create", "title": "Boiler noise", "department": "engineering"});
    let mut ids = Vec::new();
    for body in [&on_security_card, &security, &engineering] {
        ids.push(pending_id(workspace.propose(&fizzy, &bot, body, for_maya(), None).await.unwrap()));
    }
    let pending =
        |viewer: &Viewer, rooms: &[i64]| workspace.pending_items(viewer, rooms).into_iter().map(|item| item.id).collect::<Vec<_>>();
    assert_eq!(pending(&karim(), &[3]), [ids[2].clone()], "front-desk: only the Engineering one");
    assert_eq!(pending(&sam(), &[3, 4]).len(), 3);
    assert_eq!(pending(&manager(), &[]).len(), 3);
    let proposal = workspace.proposals().get(&ids[1]).unwrap();
    assert!(!workspace.proposal_visible(&karim(), &proposal, &[3]) && workspace.proposal_visible(&sam(), &proposal, &[3, 4]));
    // Its draft isn't posted in front-desk, whose members aren't all in Security; Engineering's is.
    assert!(!workspace.proposal_visible_to_room(&proposal, 3));
    assert!(workspace.proposal_visible_to_room(&workspace.proposals().get(&ids[2]).unwrap(), 3));
    assert!(workspace.proposal_visible_to_room(&proposal, 4), "Security's own room");

    // The log: the lines about a hidden card or a hidden proposal aren't shown.
    let page = workspace.hermes_page(&karim(), "all", &Rooms(vec![3])).await.unwrap();
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains("Boiler noise") && !html.contains("Badge cloned") && !html.contains("Lift B"), "{html}");
    let page = workspace.hermes_page(&sam(), "all", &Rooms(vec![3, 4])).await.unwrap();
    let html = askama::Template::render(&page).unwrap();
    assert!(html.contains("Badge cloned") && html.contains("Lift B"), "{html}");
}

// --- Alerts ------------------------------------------------------------------------------------------

async fn alerting(settings: Settings) -> (FakeFizzy, Workspace, Clock) {
    let fizzy = tagged_fizzy();
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

async fn poll_at(fizzy: &FakeFizzy, workspace: &Workspace, clock: &Clock, minutes: i64) -> Vec<crate::alerts::Delivery> {
    *clock.lock().unwrap() = at(minutes);
    workspace.poll(fizzy, at(minutes)).await.unwrap();
    workspace.take_alerts(Some(9)).unwrap()
}

fn to(deliveries: &[crate::alerts::Delivery]) -> Vec<To> {
    deliveries.iter().map(|delivery| delivery.to).collect()
}

fn html_for(deliveries: &[crate::alerts::Delivery], target: To) -> String {
    deliveries.iter().find(|delivery| delivery.to == target).map(|delivery| delivery.html.clone()).unwrap_or_default()
}

/// The board's lists with `extra` cards on page 2.
fn with_cards(fizzy: &FakeFizzy, extra: &[Value]) {
    let mut page = vec![
        card(12, "Lift B out of service", &["security", "sev-high"], Some("In progress")),
        card(14, "Door forced, service entrance", &["engineering", "security", "sev-medium"], None),
        card(15, "Puddle by the bar", &["sev-low"], None),
    ];
    page.extend(extra.iter().cloned());
    fizzy.reply(LIST_PAGE2, json!(page));
}

#[tokio::test]
async fn a_critical_card_alerts_the_duty_managers_once_and_never_at_the_first_poll() {
    let (fizzy, workspace, clock) = alerting(restricted()).await;
    assert!(poll_at(&fizzy, &workspace, &clock, 0).await.is_empty(), "the first poll only records (13 is critical, 12 high)");
    assert!(workspace.notified().contains("sev:13:critical") && workspace.notified().contains("sev:12:high"));

    // Filed after the start: the duty managers (the administrator) and Engineering's room.
    with_cards(&fizzy, &[card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z")]);
    let sent = poll_at(&fizzy, &workspace, &clock, 2).await;
    assert_eq!(to(&sent), [To::Person(1), To::Room(3)]);
    let dm = html_for(&sent, To::Person(1));
    assert!(dm.contains("<strong>Critical</strong>: Smoke in kitchen · #16 · nobody assigned"), "{dm}");
    assert!(dm.contains(r#"<a href="https://fizzy.example/897/cards/16">"#), "its link, which renders as a chip: {dm}");
    assert!(html_for(&sent, To::Room(3)).contains("Critical incident</strong> (Engineering)"));
    assert!(poll_at(&fizzy, &workspace, &clock, 3).await.is_empty(), "once");

    // Medium doesn't alert (critical and high by default); raised to high, it does.
    with_cards(
        &fizzy,
        &[
            card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z"),
            card_at(17, "Badge reader down", &["security", "sev-medium"], "2026-09-30T09:03:00Z"),
        ],
    );
    assert!(poll_at(&fizzy, &workspace, &clock, 4).await.is_empty());
    with_cards(
        &fizzy,
        &[
            card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z"),
            card_at(17, "Badge reader down", &["security", "sev-high"], "2026-09-30T09:03:00Z"),
        ],
    );
    let raised = poll_at(&fizzy, &workspace, &clock, 5).await;
    assert_eq!(to(&raised), [To::Person(1), To::Room(4)], "a Security card: Security's room only, not front-desk");
    assert!(html_for(&raised, To::Person(1)).contains("<strong>High</strong> (was medium): Badge reader down"));

    // A restart: nothing sent again, the first poll records again.
    let restarted = Workspace::new(workspace.config().clone()).with_settings(scratch_store(restricted())).with_clock(now);
    restarted.set_bots(vec![hermes_bot()]);
    restarted.set_directory(directory());
    restarted.poll(&fizzy, at(6)).await.unwrap();
    assert!(restarted.take_alerts(Some(9)).unwrap().is_empty());
    // 17 back to medium, then high again: already sent, even across the restart.
    with_cards(&fizzy, &[card_at(17, "Badge reader down", &["security", "sev-medium"], "2026-09-30T09:03:00Z")]);
    restarted.poll(&fizzy, at(7)).await.unwrap();
    with_cards(&fizzy, &[card_at(17, "Badge reader down", &["security", "sev-high"], "2026-09-30T09:03:00Z")]);
    restarted.poll(&fizzy, at(8)).await.unwrap();
    assert!(restarted.take_alerts(Some(9)).unwrap().is_empty(), "never twice");

    // No Hermes bot to send them: dropped, and counted.
    with_cards(&fizzy, &[card_at(18, "Flood, basement", &["engineering", "sev-critical"], "2026-09-30T09:08:00Z")]);
    restarted.poll(&fizzy, at(9)).await.unwrap();
    assert_eq!(restarted.take_alerts(None), Err(1));
}

#[tokio::test]
async fn alerts_follow_the_settings_and_visibility() {
    let mut settings = restricted();
    settings.duty_managers = Some(vec![7, 1]);
    settings.notifications.department_rooms = false;
    settings.notifications.severities = vec!["critical".into()];
    let (fizzy, workspace, clock) = alerting(settings).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    with_cards(
        &fizzy,
        &[
            card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z"),
            card_at(17, "Badge reader down", &["security", "sev-high"], "2026-09-30T09:01:00Z"),
        ],
    );
    let sent = poll_at(&fizzy, &workspace, &clock, 2).await;
    assert_eq!(to(&sent), [To::Person(1), To::Person(7)], "listed duty managers; no room notice; high doesn't alert");
    assert!(html_for(&sent, To::Person(7)).contains("Smoke in kitchen"));

    // Off: nothing at all.
    let mut off = workspace.settings().as_ref().clone();
    off.notifications.enabled = false;
    workspace.settings_store().save(off).unwrap();
    with_cards(&fizzy, &[card_at(18, "Flood, basement", &["engineering", "sev-critical"], "2026-09-30T09:03:00Z")]);
    assert!(poll_at(&fizzy, &workspace, &clock, 4).await.is_empty());
}

#[tokio::test]
async fn a_waiting_proposal_reminds_its_person_then_the_duty_managers() {
    // Duty managers see every card, so each of them is alerted; a proposal's person is reminded
    // only of what they may see.
    let mut settings = restricted();
    settings.autonomy.comment = Dial::AskFirst;
    let (fizzy, workspace, clock) = alerting(settings).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    let bot = hermes_bot();
    let hidden = json!({"action": "comment", "card": 12, "body": "Camera footage requested"});
    let shown = json!({"action": "comment", "card": 13, "body": "Wet floor sign placed"});
    workspace.propose(&fizzy, &bot, &hidden, for_maya(), None).await.unwrap();
    workspace.propose(&fizzy, &bot, &shown, for_maya(), None).await.unwrap();

    assert!(poll_at(&fizzy, &workspace, &clock, 9).await.is_empty(), "not 10 minutes yet");
    let reminded = poll_at(&fizzy, &workspace, &clock, 10).await;
    assert_eq!(to(&reminded), [To::Person(5)], "Maya, for the card she may see only");
    let html = html_for(&reminded, To::Person(5));
    assert!(html.contains("waiting for your confirmation") && html.contains("#13") && !html.contains("#12"), "{html}");
    assert!(poll_at(&fizzy, &workspace, &clock, 15).await.is_empty(), "once");
    let managers = poll_at(&fizzy, &workspace, &clock, 20).await;
    assert_eq!(to(&managers), [To::Person(1)]);
    let html = html_for(&managers, To::Person(1));
    assert!(html.contains("2 alerts") && html.contains("#12") && html.contains("#13") && html.contains("for Maya in front-desk"), "{html}");
}

#[tokio::test]
async fn reminders_for_cards_still_new_and_the_handover() {
    let mut settings = restricted();
    settings.handover.room_id = Some(3);
    settings.handover.time_zone = "UTC".into();
    settings.handover.shift_ends = vec!["09:30".into(), "21:30".into()];
    let (fizzy, workspace, clock) = alerting(settings).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    with_cards(&fizzy, &[card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z")]);
    assert_eq!(to(&poll_at(&fizzy, &workspace, &clock, 2).await), [To::Person(1), To::Room(3)]);
    assert!(poll_at(&fizzy, &workspace, &clock, 15).await.is_empty(), "14 minutes in New");
    let reminder = poll_at(&fizzy, &workspace, &clock, 16).await;
    assert_eq!(to(&reminder), [To::Person(1)]);
    assert!(html_for(&reminder, To::Person(1)).contains("Still in New after 15 min</strong>: Critical · Smoke in kitchen · #16"));
    assert!(poll_at(&fizzy, &workspace, &clock, 25).await.is_empty(), "once");

    // 09:30 UTC, a shift end: the handover is due.
    let due = poll_at(&fizzy, &workspace, &clock, 31).await;
    assert_eq!(to(&due), [To::Person(1)]);
    assert!(html_for(&due, To::Person(1)).contains("Handover due</strong> (shift ending 09:30)"));
    assert!(poll_at(&fizzy, &workspace, &clock, 40).await.is_empty(), "once per shift end");
}

// --- Handover ----------------------------------------------------------------------------------------

#[tokio::test]
async fn the_handover_is_built_from_the_board_for_the_room_s_members() {
    let mut settings = restricted();
    settings.handover.room_id = Some(3);
    settings.handover.time_zone = "UTC".into();
    let (fizzy, workspace) = restricted_workspace(settings).await;
    let mut closed = card_at(20, "Fire door propped", &["engineering", "sev-medium"], "2026-09-30T08:00:00Z");
    closed["closed"] = json!(true);
    fizzy.reply("/897/cards.json?board_ids%5B%5D=b1&indexed_by=closed", json!([closed]));
    workspace.poll(&fizzy, now()).await.unwrap();

    assert_eq!(workspace.handover_page(&karim()).unwrap_err().status(), 403, "duty managers only");
    let page = workspace.handover_page(&manager()).unwrap();
    // 09:00 UTC: the 23:00-07:00 shift is the nearest.
    assert_eq!(page.shift_end, "Wed 30 Sep 07:00");
    let text = &page.text;
    assert!(text.starts_with("Handover: shift ending Wed 30 Sep 07:00\nSince 23:00: "), "{text}");
    assert!(text.contains("Critical\n- #13 Guest slip in lobby · New · nobody assigned · https://fizzy.example/897/cards/13"), "{text}");
    assert!(text.contains("Low\n- #15 Puddle by the bar"), "{text}");
    assert!(text.contains("Closed this shift (1)\n- #20 Fire door propped · medium · Closed"), "{text}");
    assert!(text.contains("Waiting for a confirmation (0)\n- none"), "{text}");
    // Front-desk's members (Karim, Maya) may not see Security's cards: counted, not named.
    assert!(!text.contains("Lift B") && !text.contains("Door forced"), "{text}");
    assert_eq!(page.left_out, 2);
    assert!(!page.can_post && page.blocked.as_deref().unwrap().contains("aren’t a member of front-desk"));
    assert_eq!(workspace.handover_message(&manager(), "x").unwrap_err().code(), "not_a_member");

    // A member of the room can post it; the next one starts from there.
    workspace.set_rooms_of(1, [3]);
    let page = workspace.handover_page(&manager()).unwrap();
    assert!(page.can_post && page.blocked.is_none());
    assert_eq!(page.room_name.as_deref(), Some("front-desk"));
    let html = askama::Template::render(&page).unwrap();
    assert!(
        html.contains("data-ws-handover-form") && html.contains("Post to front-desk") && html.contains("2 items aren’t listed"),
        "{html}"
    );
    let (room, body) = workspace.handover_message(&manager(), "All quiet.\nhttps://fizzy.example/897/cards/13").unwrap();
    assert_eq!(room, 3);
    assert!(body.contains(r#"<a href="https://fizzy.example/897/cards/13">"#));
    assert_eq!(workspace.handover_message(&manager(), "  ").unwrap_err().code(), "blank_text");
    workspace.handover_posted(&manager());
    let page = workspace.handover_page(&manager()).unwrap();
    assert!(page.text.contains("Since 09:00: 0 new, 0 closed"), "{}", page.text);
    assert_eq!(page.last_posted_by.as_deref(), Some("Manager"));

    // No room set: shown, but it can't be posted.
    let mut unset = workspace.settings().as_ref().clone();
    unset.handover.room_id = None;
    workspace.settings_store().save(unset).unwrap();
    let page = workspace.handover_page(&manager()).unwrap();
    assert!(!page.can_post && page.text.contains("Lift B"), "no room: the duty manager's own view");
    assert_eq!(workspace.handover_message(&manager(), "x").unwrap_err().code(), "no_room");
}

// --- Review fixes ------------------------------------------------------------------------------------

/// The cards of the Raised alerts in `sent` to the administrator (the duty manager).
fn raised_cards(sent: &[crate::alerts::Delivery]) -> Vec<String> {
    let html = html_for(sent, To::Person(1));
    html.split("<p>")
        .filter(|line| line.starts_with("<strong>Critical</strong>") || line.starts_with("<strong>High</strong>"))
        .filter_map(|line| line.split(" · #").nth(1)?.split(' ').next().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn cards_changed_through_campfire_alert_at_the_next_poll() {
    // Everything Campfire writes or reads is remembered in the live picture at once: the alerts
    // compare with what the last poll saw, not with that.
    let mut settings = restricted();
    settings.autonomy.create = Dial::Alone;
    let (fizzy, workspace, clock) = alerting(settings).await;
    assert!(poll_at(&fizzy, &workspace, &clock, 0).await.is_empty());
    let mut listed: Vec<Value> = Vec::new();

    // 1. Created from the workspace, critical.
    let new =
        NewCard::parse(&json!({"title": "Smoke in kitchen", "severity": "critical", "department": "engineering"}), &workspace.settings())
            .unwrap();
    let created = workspace.create_card(&fizzy, &manager(), new).await.unwrap().card.number;
    listed.push(card_at(created, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z"));
    with_cards(&fizzy, &listed);
    let sent = poll_at(&fizzy, &workspace, &clock, 2).await;
    assert_eq!(raised_cards(&sent), [created.to_string()], "{sent:?}");
    assert!(poll_at(&fizzy, &workspace, &clock, 3).await.is_empty(), "once");

    // 2. Severity raised from the sheet: 15, low → high.
    workspace.change_card(&fizzy, &manager(), 15, Change::Severity(Some(Severity::High))).await.unwrap();
    let mut page = vec![
        card(12, "Lift B out of service", &["security", "sev-high"], Some("In progress")),
        card(14, "Door forced, service entrance", &["engineering", "security", "sev-medium"], None),
        card(15, "Puddle by the bar", &["sev-high"], None),
    ];
    page.extend(listed.iter().cloned());
    fizzy.reply(LIST_PAGE2, json!(page));
    let sent = poll_at(&fizzy, &workspace, &clock, 4).await;
    assert_eq!(raised_cards(&sent), ["15"]);
    assert!(html_for(&sent, To::Person(1)).contains("<strong>High</strong> (was low): Puddle by the bar"));
    assert!(poll_at(&fizzy, &workspace, &clock, 5).await.is_empty(), "once");

    // 3. A proposal Campfire runs alone.
    let alone = json!({"action": "create", "title": "Flood, basement", "severity": "critical", "department": "engineering"});
    let proposal = match workspace.propose(&fizzy, &hermes_bot(), &alone, for_maya(), None).await.unwrap() {
        Proposed::Done { proposal } => proposal,
        other => panic!("{other:?}"),
    };
    let flood = proposal.result_card.unwrap();
    page.push(card_at(flood, "Flood, basement", &["engineering", "sev-critical"], "2026-09-30T09:05:30Z"));
    fizzy.reply(LIST_PAGE2, json!(page));
    let sent = poll_at(&fizzy, &workspace, &clock, 6).await;
    assert_eq!(raised_cards(&sent), [flood.to_string()]);
    assert!(poll_at(&fizzy, &workspace, &clock, 7).await.is_empty(), "once");

    // 4. A proposal someone confirms.
    let mut asking = workspace.settings().as_ref().clone();
    asking.autonomy.create = Dial::AskFirst;
    workspace.settings_store().save(asking).unwrap();
    let ask = json!({"action": "create", "title": "Gas smell, laundry", "severity": "high", "department": "engineering"});
    let id = pending_id(workspace.propose(&fizzy, &hermes_bot(), &ask, for_maya(), None).await.unwrap());
    let confirmed = workspace.decide(&fizzy, &manager(), &id, crate::drafts::Decision::Confirm).await.unwrap();
    let gas = confirmed.result_card.unwrap();
    page.push(card_at(gas, "Gas smell, laundry", &["engineering", "sev-high"], "2026-09-30T09:07:30Z"));
    fizzy.reply(LIST_PAGE2, json!(page));
    let sent = poll_at(&fizzy, &workspace, &clock, 8).await;
    assert_eq!(raised_cards(&sent), [gas.to_string()]);
    assert!(poll_at(&fizzy, &workspace, &clock, 9).await.is_empty(), "once");

    // 5. Raised in Fizzy, and the sheet opened before the next poll: 14, medium → critical.
    let mut raised = fizzy.live_card(14);
    raised["tags"] = json!(["engineering", "security", "sev-critical"]);
    fizzy.live(raised.clone());
    workspace.card_sheet(&fizzy, &manager(), 14).await.unwrap();
    page[1] = raised;
    fizzy.reply(LIST_PAGE2, json!(page));
    let sent = poll_at(&fizzy, &workspace, &clock, 10).await;
    assert_eq!(raised_cards(&sent), ["14"]);
    assert!(poll_at(&fizzy, &workspace, &clock, 11).await.is_empty(), "once");
}

#[tokio::test]
async fn a_proposed_card_s_departments_are_those_its_request_resolves_to() {
    // `tags` naming a department count as that department, as when the card is created.
    let mut settings = restricted();
    settings.notifications.draft_reminder_min = 10;
    let (fizzy, workspace, clock) = alerting(settings).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    let by_tags = json!({"action": "create", "title": "Badge cloned", "tags": ["#Security", "incident"]});
    let id = pending_id(workspace.propose(&fizzy, &hermes_bot(), &by_tags, for_maya(), None).await.unwrap());
    let proposal = workspace.proposals().get(&id).unwrap();
    assert_eq!(proposal.departments.as_deref(), Some(["security".to_string()].as_slice()), "resolved and stored");
    assert!(!workspace.proposal_visible(&karim(), &proposal, &[3]), "Security's, like the card it would create");
    assert!(workspace.proposal_visible(&sam(), &proposal, &[3, 4]));
    assert!(!workspace.proposal_visible_to_room(&proposal, 3));
    assert_eq!(crate::alerts::proposal_tags(&proposal, &workspace.snapshot()), ["security"]);
    // Maya (front-desk only) isn't reminded of it.
    assert!(poll_at(&fizzy, &workspace, &clock, 10).await.is_empty());
}

#[tokio::test]
async fn unknown_room_members_see_nothing_restricted() {
    // Before the app has read the people and rooms (or for a room nobody is known to be in), a
    // restricted card's details go nowhere: an empty member list isn't "everyone may see it".
    let mut settings = restricted();
    settings.handover.room_id = Some(3);
    settings.handover.time_zone = "UTC".into();
    let fizzy = tagged_fizzy();
    let workspace = Workspace::new(config()).with_settings(scratch_store(settings)).with_clock(now);
    workspace.set_bots(vec![hermes_bot()]);
    workspace.poll(&fizzy, now()).await.unwrap();
    assert!(!workspace.directory().loaded);
    let engineering = json!({"action": "create", "title": "Boiler noise", "department": "engineering"});
    let id = pending_id(workspace.propose(&fizzy, &hermes_bot(), &engineering, for_maya(), None).await.unwrap());
    let proposal = workspace.proposals().get(&id).unwrap();
    assert!(!workspace.proposal_visible_to_room(&proposal, 3), "the draft waits in the Hermes tab");

    let page = workspace.handover_page(&manager()).unwrap();
    assert!(!page.text.contains("Lift B") && !page.text.contains("Door forced"), "{}", page.text);
    assert!(page.text.contains("Guest slip in lobby"), "not restricted: listed");
    assert!(page.text.contains("Waiting for a confirmation (0)"), "{}", page.text);
    assert_eq!(page.left_out, 3, "12, 14 and the proposal, counted");

    // Loaded, but nobody is known to be in the room.
    workspace.set_directory(directory());
    assert!(workspace.proposal_visible_to_room(&proposal, 3));
    assert!(!workspace.proposal_visible_to_room(&proposal, 99));
    let mut elsewhere = workspace.settings().as_ref().clone();
    elsewhere.handover.room_id = Some(99);
    workspace.settings_store().save(elsewhere).unwrap();
    let page = workspace.handover_page(&manager()).unwrap();
    assert!(!page.text.contains("Lift B") && page.left_out == 3, "{}", page.text);

    // Not restricted: a room's members don't matter to the cards.
    let mut open = workspace.settings().as_ref().clone();
    open.visibility.mode = Mode::Everyone;
    workspace.settings_store().save(open).unwrap();
    assert!(workspace.proposal_visible_to_room(&proposal, 99));
    assert!(workspace.handover_page(&manager()).unwrap().text.contains("Lift B"));
}

#[tokio::test]
async fn turning_alerts_back_on_or_changing_delays_sends_nothing_stale() {
    let (fizzy, workspace, clock) = alerting(restricted()).await;
    let set = |change: &dyn Fn(&mut Settings)| {
        let mut settings = workspace.settings().as_ref().clone();
        change(&mut settings);
        workspace.settings_store().save(settings).unwrap();
    };
    poll_at(&fizzy, &workspace, &clock, 0).await;

    // Off: what would have been sent is recorded all the same, and not sent once back on.
    set(&|settings| settings.notifications.enabled = false);
    with_cards(&fizzy, &[card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z")]);
    assert!(poll_at(&fizzy, &workspace, &clock, 2).await.is_empty());
    assert!(poll_at(&fizzy, &workspace, &clock, 20).await.is_empty(), "16 has been in New 19 minutes");
    set(&|settings| settings.notifications.enabled = true);
    assert!(poll_at(&fizzy, &workspace, &clock, 21).await.is_empty(), "neither the alert nor the reminder, late");

    // A shorter delay: reminders long overdue under it aren't sent all at once.
    set(&|settings| settings.notifications.new_reminder_min = 240);
    with_cards(
        &fizzy,
        &[
            card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z"),
            card_at(17, "Flood, basement", &["engineering", "sev-critical"], "2026-09-30T09:22:00Z"),
        ],
    );
    assert_eq!(raised_cards(&poll_at(&fizzy, &workspace, &clock, 23).await), ["17"]);
    let mut asking = workspace.settings().as_ref().clone();
    asking.autonomy.comment = Dial::AskFirst;
    asking.notifications.draft_reminder_min = 0;
    workspace.settings_store().save(asking).unwrap();
    let comment = json!({"action": "comment", "card": 13, "body": "Wet floor sign placed"});
    workspace.propose(&fizzy, &hermes_bot(), &comment, for_maya(), None).await.unwrap();
    let html = html_for(&poll_at(&fizzy, &workspace, &clock, 90).await, To::Person(1));
    assert!(!html.contains("#16") && !html.contains("#17") && !html.contains("waiting"), "not due under these delays: {html}");
    set(&|settings| {
        settings.notifications.new_reminder_min = 15;
        settings.notifications.draft_reminder_min = 10;
    });
    assert!(poll_at(&fizzy, &workspace, &clock, 91).await.is_empty(), "due over an hour ago: not sent now");

    // What becomes due from now on is.
    with_cards(
        &fizzy,
        &[
            card_at(16, "Smoke in kitchen", &["engineering", "sev-critical"], "2026-09-30T09:01:00Z"),
            card_at(17, "Flood, basement", &["engineering", "sev-critical"], "2026-09-30T09:22:00Z"),
            card_at(18, "Gas smell, laundry", &["engineering", "sev-high"], "2026-09-30T10:32:00Z"),
        ],
    );
    assert_eq!(raised_cards(&poll_at(&fizzy, &workspace, &clock, 92).await), ["18"]);
    let reminder = poll_at(&fizzy, &workspace, &clock, 107).await;
    let html = html_for(&reminder, To::Person(1));
    assert!(html.contains("Still in New after 15 min") && html.contains("#18") && !html.contains("#16"), "{html}");
}

#[tokio::test]
async fn new_serious_incidents_come_first_in_a_long_message() {
    let (fizzy, workspace, clock) = alerting(restricted()).await;
    poll_at(&fizzy, &workspace, &clock, 0).await;
    let mut cards: Vec<Value> =
        (20..30).map(|number| card_at(number, &format!("Leak {number}"), &["engineering", "sev-high"], "2026-09-30T09:00:30Z")).collect();
    with_cards(&fizzy, &cards);
    assert_eq!(raised_cards(&poll_at(&fizzy, &workspace, &clock, 1).await).len(), 10);
    // At 09:16: ten reminders (still in New), a high card and a critical one: 12 lines, 10 shown.
    cards.push(card_at(30, "Gas smell, laundry", &["engineering", "sev-high"], "2026-09-30T09:15:00Z"));
    cards.push(card_at(31, "Fire, kitchen", &["engineering", "sev-critical"], "2026-09-30T09:15:00Z"));
    with_cards(&fizzy, &cards);
    let html = html_for(&poll_at(&fizzy, &workspace, &clock, 16).await, To::Person(1));
    let lines: Vec<&str> = html.split("<p>").skip(1).collect();
    assert!(lines[0].starts_with("<strong>12 alerts</strong>"), "{html}");
    assert!(lines[1].starts_with("<strong>Critical</strong>: Fire, kitchen"), "{html}");
    assert!(lines[2].starts_with("<strong>High</strong>: Gas smell, laundry"), "{html}");
    assert!(lines[3..11].iter().all(|line| line.starts_with("<strong>Still in New")), "{html}");
    assert!(lines[11].starts_with("… and 2 more"), "{html}");
}
