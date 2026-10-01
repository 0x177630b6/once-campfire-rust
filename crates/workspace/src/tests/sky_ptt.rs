//! Sky push-to-talk (batch 1a): what the screen note may say about a card, per viewer, and the
//! usage store the workspace owns.

use std::collections::{BTreeSet, HashMap};

use super::*;
use crate::sky::{CardFacts, ContextNote, Screen};
use crate::visibility::{Mode, Untagged, Visibility};

fn person(id: i64, name: &str) -> Viewer {
    Viewer { id, name: name.into(), email: None, administrator: false }
}

/// Engineering (room 3), Security (room 4, restricted), visibility by department room.
fn settings() -> Settings {
    Settings {
        departments: vec![
            Department { name: "Engineering".into(), tag: "engineering".into(), rooms: vec![3], restricted: false },
            Department { name: "Security".into(), tag: "security".into(), rooms: vec![4], restricted: true },
        ],
        visibility: Visibility { mode: Mode::ByDepartmentRoom, untagged: Untagged::Everyone },
        ..Settings::default()
    }
}

/// Maya in room 3; Sam in rooms 3 and 4. One poll first, so the workspace knows the board.
async fn workspace() -> Workspace {
    let workspace = Workspace::new(config()).with_settings(scratch_store(settings()));
    let memberships = [(5, vec![3]), (8, vec![3, 4])];
    workspace.set_directory(Directory {
        memberships: memberships.into_iter().map(|(id, rooms)| (id, rooms.into_iter().collect::<BTreeSet<i64>>())).collect(),
        rooms: HashMap::from([(3, "front-desk".to_string()), (4, "security".to_string())]),
        loaded: true,
        ..Directory::default()
    });
    workspace.poll(&fizzy(), now()).await.unwrap();
    for value in [
        card(57, "Lift out of order \"building 7\"", &["engineering", "sev-critical"], Some("In progress")),
        card(58, "Theft in the lobby", &["security", "sev-high"], None),
    ] {
        workspace.remember(serde_json::from_value(value).unwrap());
    }
    workspace
}

#[tokio::test]
async fn the_note_only_shows_cards_the_viewer_may_see_and_never_a_restricted_ones_content() {
    let workspace = workspace().await;
    let (maya, sam) = (person(5, "Maya"), person(8, "Sam"));

    let lift = workspace.sky_card(&maya, 57).unwrap();
    assert_eq!(
        lift,
        CardFacts {
            number: 57,
            restricted: false,
            title: "Lift out of order \"building 7\"".into(),
            state: "In progress".into(),
            severity: Some("critical"),
        }
    );
    assert_eq!(workspace.sky_card(&maya, 58), None, "Maya isn't in the security room");
    assert_eq!(workspace.sky_card(&maya, 99), None, "not in the picture");

    let theft = workspace.sky_card(&sam, 58).unwrap();
    assert!(theft.restricted && theft.title.is_empty() && theft.severity.is_none(), "{theft:?}");
    let zone = crate::shifts::time_zone("Europe/Paris").unwrap();
    let note = ContextNote::build(Screen::Room, Some("security"), Some(&theft), now(), &zone);
    assert!(note.restricted);
    assert!(!note.note.contains("Theft") && !note.chip.contains("Theft"), "{note:?}");
    assert_eq!(note.chip, "Ticket #58 · restricted");
}

#[tokio::test]
async fn the_workspace_owns_the_usage_store() {
    let workspace = workspace().await;
    assert_eq!(workspace.sky().config().mode, crate::sky::Mode::Off);
    assert!(workspace.take_storage_errors().is_empty());
    let zone = crate::shifts::time_zone("Europe/Paris").unwrap();
    workspace.sky().allow_press(5, now(), &zone).unwrap();
    assert!(workspace.sky().save().unwrap());
    let path = workspace.config().storage_file(crate::sky::USAGE_FILE);
    assert!(std::fs::read_to_string(path).unwrap().contains("\"presses\": 1"));
}
