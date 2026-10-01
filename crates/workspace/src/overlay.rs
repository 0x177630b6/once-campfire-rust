//! What the workspace adds to every page of a signed-in user, through the hook at the end of
//! the application layout: its scripts, the phone tab bar (Home · Chats · Report ·
//! Boards · Hermes; a small rail on wide screens), and, on the page of a room linked to a department, the
//! room's cards panel ([`crate::pages::RoomPanel`]). Report opens the live voice report of the room
//! on screen (or of the last room visited), only when that feature is on. For the people Sky
//! push-to-talk is on for (`SKY_PTT`), also Sky's floating button ([`SkyButton`]). Its stylesheet,
//! [`STYLESHEET`], is linked from the layout's head instead (`campfire_views::hermes::head_tags`).

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
    /// `false` keeps only the scripts (pages with their own bottom bar).
    pub show_bar: bool,
    pub home_url: String,
    pub board_url: String,
    pub hermes_url: String,
    pub chats_url: String,
    pub report_url: Option<String>,
    pub cards_url: String,
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
    /// Sky's push-to-talk button, for the people `SKY_PTT` lets use it; `None` renders nothing.
    pub sky: Option<SkyButton>,
}

/// Sky push-to-talk (docs/hermes-gemini-live.md, "Sky push-to-talk"): a `data-turbo-permanent`
/// element (`#sky-ptt`, kept across Turbo visits so a reply goes on playing) after the tab bar, and
/// the page's own hint in a non-permanent `<template data-sky-page>`, which the script reads on
/// every visit (Turbo keeps the old permanent element and ignores the new page's copy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyButton {
    pub token_url: String,
    pub context_url: String,
    pub usage_url: String,
    /// The microphone's AudioWorklet (`voice/pcm-worklet.js`, digested).
    pub worklet_url: String,
    /// `hermes/sky_ptt_logic.js` and `hermes/sky_ptt.js` (digested), loaded with the tab bar.
    pub logic_url: String,
    pub script_url: String,
    /// `home`, `chats`, `room`, `board`, `card`, `sky` or `other` (`campfire_workspace::sky::Screen`).
    pub screen: &'static str,
    pub room_id: Option<i64>,
    /// A card's own page (`/workspace/cards/:n`).
    pub card: Option<u64>,
    /// Not on this page (the live voice report has its own microphone).
    pub hidden: bool,
    /// `SKY_WARM_SECONDS`: idle seconds before the page closes the session.
    pub warm_seconds: u64,
    /// Show the press timings in the reply (administrators: the spike's measurements).
    pub debug: bool,
}

impl SkyButton {
    /// The screen of a request path, for the page's hint: `(screen, room, card)`.
    pub fn screen_of(path: &str) -> (&'static str, Option<i64>, Option<u64>) {
        let path = path.split(['?', '#']).next().unwrap_or("");
        if let Some(number) = path.strip_prefix("/workspace/cards/").and_then(|rest| rest.parse::<u64>().ok()) {
            return ("card", None, Some(number));
        }
        if let Some(room) = room_page(path) {
            return ("room", Some(room), None);
        }
        match locate(path) {
            (Tab::Home, _) => ("home", None, None),
            (Tab::Board, _) => ("board", None, None),
            (Tab::Hermes, _) => ("sky", None, None),
            (Tab::Chats, room) => ("chats", room, None),
            (Tab::Other, _) => ("other", None, None),
        }
    }
}

/// The workspace's stylesheet (crates/assets/overrides), linked from the layout's head on the pages
/// that get the tab bar.
pub const STYLESHEET: &str = "hermes/workspace.css";

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
            script_url: "/assets/hermes/workspace-1.js".into(),
            logic_url: "/assets/hermes/workspace_logic-1.js".into(),
            home_icon: "/assets/hermes/home-1.svg".into(),
            board_icon: "/assets/hermes/board-1.svg".into(),
            hermes_icon: "/assets/bot-1.svg".into(),
            chats_icon: "/assets/messages-outlined-1.svg".into(),
            report_icon: "/assets/headset-1.svg".into(),
            panel: String::new(),
            can_create: true,
            sky: None,
        }
    }

    fn sky_button(screen: &'static str, room_id: Option<i64>, hidden: bool) -> SkyButton {
        SkyButton {
            token_url: "/sky/token".into(),
            context_url: "/sky/context".into(),
            usage_url: "/sky/usage".into(),
            worklet_url: "/assets/voice/pcm-worklet-1.js".into(),
            logic_url: "/assets/hermes/sky_ptt_logic-1.js".into(),
            script_url: "/assets/hermes/sky_ptt-1.js".into(),
            screen,
            room_id,
            card: None,
            hidden,
            warm_seconds: 120,
            debug: false,
        }
    }

    #[test]
    fn renders_skys_button_once_after_the_nav() {
        let html = TabBar { sky: Some(sky_button("room", Some(12), false)), ..bar(Tab::Chats, None) }.render().unwrap();
        assert_eq!(html.matches(r#"id="sky-ptt""#).count(), 1, "{html}");
        let element = html.find(r#"<div id="sky-ptt""#).unwrap();
        assert!(html.find("</nav>").unwrap() < element, "after the tab bar");
        assert!(html[element..].starts_with(
            r#"<div id="sky-ptt" class="sky-ptt" data-turbo-permanent data-sky-logic-url="/assets/hermes/sky_ptt_logic-1.js" data-sky-token-url="/sky/token" data-sky-context-url="/sky/context" data-sky-usage-url="/sky/usage" data-sky-worklet-url="/assets/voice/pcm-worklet-1.js" data-sky-warm-seconds="120" data-sky-debug="false">"#
        ), "{html}");
        assert!(html.contains(r#"data-sky-ptt-button"#) && html.contains(r#"aria-label="Hold to talk to Sky""#));
        assert!(
            html.contains(r#"<template data-sky-page data-screen="room" data-room="12" data-card="" data-hidden="false"></template>"#),
            "{html}"
        );
        let logic = html.find("sky_ptt_logic-1.js").unwrap();
        assert!(logic < html.find("sky_ptt-1.js").unwrap(), "the logic module loads first");
        assert!(html.find("workspace-1.js").unwrap() < logic);

        // The voice page: no tab bar, the element still there (Turbo keeps it), hidden.
        let voice = TabBar { show_bar: false, sky: Some(sky_button("other", None, true)), ..bar(Tab::Chats, None) }.render().unwrap();
        assert!(!voice.contains("<nav") && voice.contains(r#"<div id="sky-ptt""#));
        assert!(voice.contains(r#"data-hidden="true""#) && voice.contains(r#"data-sky-debug="false" hidden>"#), "{voice}");

        let off = bar(Tab::Chats, None).render().unwrap();
        assert!(!off.contains("sky-ptt") && !off.contains("data-sky"), "nothing when off: {off}");
    }

    #[test]
    fn the_screen_of_a_path() {
        assert_eq!(SkyButton::screen_of("/rooms/12"), ("room", Some(12), None));
        assert_eq!(SkyButton::screen_of("/rooms/12/@34"), ("room", Some(12), None));
        assert_eq!(SkyButton::screen_of("/rooms/12/voice"), ("chats", Some(12), None));
        assert_eq!(SkyButton::screen_of("/workspace"), ("home", None, None));
        assert_eq!(SkyButton::screen_of("/workspace/board?dept=x"), ("board", None, None));
        assert_eq!(SkyButton::screen_of("/workspace/cards/57"), ("card", None, Some(57)));
        assert_eq!(SkyButton::screen_of("/workspace/cards/new"), ("board", None, None));
        assert_eq!(SkyButton::screen_of("/workspace/hermes"), ("sky", None, None));
        assert_eq!(SkyButton::screen_of("/users/1"), ("other", None, None));
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
        assert!(!html.contains("<link") && !html.contains(".css"), "the stylesheet is linked from the head: {html}");
        assert!(html.contains(r#"<script type="module" src="/assets/hermes/workspace-1.js"></script>"#));
        assert!(html.contains(r#"data-ws-cards-url="/workspace/cards.json""#));
        assert!(html.contains(r#"href="/workspace" aria-current="page""#));
        assert!(html.contains(r#"href="/rooms/3/voice""#));
        assert!(html.contains(r#"href="/workspace/board""#) && html.contains("Boards"));
        let board = bar(Tab::Board, None).render().unwrap();
        assert!(board.contains(r#"href="/workspace/board" aria-current="page""#));
        let hermes = bar(Tab::Hermes, None).render().unwrap();
        assert!(hermes.contains(r#"href="/workspace/hermes" aria-current="page""#) && hermes.contains(">Sky<"));
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
