//! What the workspace adds to every page of a signed-in user, through the one hook at the end of
//! the application layout: its stylesheet and script, the phone tab bar (Home · Chats · Report ·
//! Boards · Hermes; a small rail on wide screens), and, on the page of a room linked to a department, the
//! room's cards panel ([`crate::pages::RoomPanel`]). Report opens the live voice report of the room
//! on screen (or of the last room visited), only when that feature is on.

use askama::Template;

/// Which tab the page belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Board,
    Chats,
    /// The Hermes tab (phase 2).
    Hermes,
    Other,
}

/// Asset URLs are digested paths the app resolves.
#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/_tab_bar.html")]
pub struct TabBar {
    pub active: Tab,
    /// `false` keeps only the stylesheet and script (pages with their own bottom bar).
    pub show_bar: bool,
    pub home_url: String,
    pub board_url: String,
    pub hermes_url: String,
    pub chats_url: String,
    pub report_url: Option<String>,
    pub cards_url: String,
    pub stylesheet_url: String,
    pub script_url: String,
    /// The script's pure logic (`hermes/workspace_logic.js`), loaded before it.
    pub logic_url: String,
    pub home_icon: String,
    pub board_icon: String,
    pub hermes_icon: String,
    pub chats_icon: String,
    pub report_icon: String,
    /// The room's cards panel (rendered), or empty.
    pub panel: String,
    /// The policy lets the viewer create cards: the script adds "Create a card" to the message
    /// menus only then (the server checks again).
    pub can_create: bool,
}

/// `/workspace`
pub const HOME_PATH: &str = "/workspace";
/// `GET ?numbers=12,13`: the chips of those cards, as `{"cards": {"12": "<a class=\"ws-chip\"…"}}`.
pub const CARDS_PATH: &str = "/workspace/cards.json";

/// The workspace pages' nav: back to the chats, and the page's name in Campfire's title pill.
#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/_nav.html")]
pub struct HomeNav {
    pub title: String,
    pub chats_url: String,
    pub back_icon: String,
}

/// Where a request path is: `(tab, room id)`. Room pages are `/rooms/:id` and below.
pub fn locate(path: &str) -> (Tab, Option<i64>) {
    let path = path.split(['?', '#']).next().unwrap_or("");
    if path == crate::pages::BOARD_PATH || path.starts_with("/workspace/cards/") {
        return (Tab::Board, None);
    }
    if path == crate::hermes::HERMES_PATH || path.starts_with("/workspace/hermes/") {
        return (Tab::Hermes, None);
    }
    if path == HOME_PATH || path.starts_with("/workspace/") {
        return (Tab::Home, None);
    }
    let room = path.strip_prefix("/rooms/").and_then(|rest| rest.split('/').next()).and_then(|id| id.parse::<i64>().ok());
    match room {
        Some(id) => (Tab::Chats, Some(id)),
        None => (Tab::Other, None),
    }
}

/// The room whose conversation is on screen: `/rooms/:id`, or a message in it (`/rooms/:id/@:m`).
/// Not the room's other pages (settings, voice report…).
pub fn room_page(path: &str) -> Option<i64> {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let rest = path.strip_prefix("/rooms/")?;
    let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let id = id.parse().ok()?;
    (tail.is_empty() || (tail.starts_with('@') && tail[1..].bytes().all(|b| b.is_ascii_digit()))).then_some(id)
}

/// The live voice report's page for a room (`controllers::voice`).
pub fn voice_path(room_id: i64) -> String {
    format!("/rooms/{room_id}/voice")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(active: Tab, report_url: Option<&str>) -> TabBar {
        TabBar {
            active,
            show_bar: true,
            home_url: HOME_PATH.into(),
            board_url: "/workspace/board".into(),
            hermes_url: "/workspace/hermes".into(),
            chats_url: "/".into(),
            report_url: report_url.map(str::to_string),
            cards_url: CARDS_PATH.into(),
            stylesheet_url: "/assets/hermes/workspace-1.css".into(),
            script_url: "/assets/hermes/workspace-1.js".into(),
            logic_url: "/assets/hermes/workspace_logic-1.js".into(),
            home_icon: "/assets/hermes/home-1.svg".into(),
            board_icon: "/assets/hermes/board-1.svg".into(),
            hermes_icon: "/assets/bot-1.svg".into(),
            chats_icon: "/assets/messages-outlined-1.svg".into(),
            report_icon: "/assets/headset-1.svg".into(),
            panel: String::new(),
            can_create: true,
        }
    }

    #[test]
    fn locates_pages() {
        assert_eq!(locate("/workspace"), (Tab::Home, None));
        assert_eq!(locate("/rooms/12?x=1"), (Tab::Chats, Some(12)));
        assert_eq!(locate("/rooms/12/@34"), (Tab::Chats, Some(12)));
        assert_eq!(locate("/rooms/opens"), (Tab::Other, None));
        assert_eq!(locate("/users/1"), (Tab::Other, None));
        assert_eq!(locate("/workspace/board?dept=x"), (Tab::Board, None));
        assert_eq!(locate("/workspace/cards/12"), (Tab::Board, None));
        assert_eq!(locate("/workspace/settings"), (Tab::Home, None));
        assert_eq!(locate("/workspace/hermes"), (Tab::Hermes, None));
        assert_eq!(locate("/workspace/hermes?filter=tags"), (Tab::Hermes, None));
    }

    #[test]
    fn room_pages() {
        assert_eq!(room_page("/rooms/12"), Some(12));
        assert_eq!(room_page("/rooms/12/@34?x"), Some(12));
        for other in ["/rooms/12/voice", "/rooms/12/edit", "/rooms/opens", "/rooms/12/@x", "/workspace"] {
            assert_eq!(room_page(other), None, "{other}");
        }
    }

    #[test]
    fn renders_the_bar() {
        let html = bar(Tab::Home, Some("/rooms/3/voice")).render().unwrap();
        assert!(html.contains(r#"<link rel="stylesheet" href="/assets/hermes/workspace-1.css">"#));
        assert!(html.contains(r#"<script type="module" src="/assets/hermes/workspace-1.js"></script>"#));
        assert!(html.contains(r#"data-ws-cards-url="/workspace/cards.json""#));
        assert!(html.contains(r#"href="/workspace" aria-current="page""#));
        assert!(html.contains(r#"href="/rooms/3/voice""#));
        assert!(html.contains(r#"href="/workspace/board""#) && html.contains("Boards"));
        let board = bar(Tab::Board, None).render().unwrap();
        assert!(board.contains(r#"href="/workspace/board" aria-current="page""#));
        let hermes = bar(Tab::Hermes, None).render().unwrap();
        assert!(hermes.contains(r#"href="/workspace/hermes" aria-current="page""#) && hermes.contains(">Hermes<"));
        let logic = html.find("workspace_logic-1.js").unwrap();
        assert!(logic < html.find("workspace-1.js").unwrap(), "the logic module loads first");
        let with_panel = TabBar { panel: "<div class=\"ws-panel-root\"></div>".into(), ..bar(Tab::Chats, None) }.render().unwrap();
        assert!(with_panel.contains(r#"<div class="ws-panel-root"></div>"#));

        let without_report = bar(Tab::Chats, None).render().unwrap();
        assert!(!without_report.contains("Report") && without_report.contains(r#"href="/" aria-current="page""#));

        let hidden = TabBar { show_bar: false, ..bar(Tab::Chats, None) }.render().unwrap();
        assert!(!hidden.contains("<nav") && hidden.contains("workspace-1.js"));
        assert!(hidden.contains(r#"<template data-ws-viewer data-ws-can-create="true"></template>"#), "{hidden}");
        let cannot = TabBar { can_create: false, ..bar(Tab::Chats, None) }.render().unwrap();
        assert!(cannot.contains(r#"data-ws-can-create="false""#));
    }
}
