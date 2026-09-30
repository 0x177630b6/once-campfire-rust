//! What the workspace adds to every page of a signed-in user, through the one hook at the end of
//! the application layout: its stylesheet and script, and the phone tab bar (Home · Report ·
//! Chats; a small rail on wide screens). Report opens the live voice report of the room on screen
//! (or of the last room visited), only when that feature is on.

use askama::Template;

/// Which tab the page belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Chats,
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
    pub chats_url: String,
    pub report_url: Option<String>,
    pub cards_url: String,
    pub stylesheet_url: String,
    pub script_url: String,
    pub home_icon: String,
    pub chats_icon: String,
    pub report_icon: String,
}

/// `/workspace`
pub const HOME_PATH: &str = "/workspace";
/// `GET ?numbers=12,13`: the chips of those cards, as `{"cards": {"12": "<a class=\"ws-chip\"…"}}`.
pub const CARDS_PATH: &str = "/workspace/cards.json";

/// The Home page's nav: back to the chats, and the page's name in Campfire's title pill.
#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "workspace/_nav.html")]
pub struct HomeNav {
    pub chats_url: String,
    pub back_icon: String,
}

/// Where a request path is: `(tab, room id)`. Room pages are `/rooms/:id` and below.
pub fn locate(path: &str) -> (Tab, Option<i64>) {
    let path = path.split(['?', '#']).next().unwrap_or("");
    if path == HOME_PATH || path.starts_with("/workspace/") {
        return (Tab::Home, None);
    }
    let room = path.strip_prefix("/rooms/").and_then(|rest| rest.split('/').next()).and_then(|id| id.parse::<i64>().ok());
    match room {
        Some(id) => (Tab::Chats, Some(id)),
        None => (Tab::Other, None),
    }
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
            chats_url: "/".into(),
            report_url: report_url.map(str::to_string),
            cards_url: CARDS_PATH.into(),
            stylesheet_url: "/assets/hermes/workspace-1.css".into(),
            script_url: "/assets/hermes/workspace-1.js".into(),
            home_icon: "/assets/hermes/home-1.svg".into(),
            chats_icon: "/assets/messages-outlined-1.svg".into(),
            report_icon: "/assets/headset-1.svg".into(),
        }
    }

    #[test]
    fn locates_pages() {
        assert_eq!(locate("/workspace"), (Tab::Home, None));
        assert_eq!(locate("/rooms/12?x=1"), (Tab::Chats, Some(12)));
        assert_eq!(locate("/rooms/12/@34"), (Tab::Chats, Some(12)));
        assert_eq!(locate("/rooms/opens"), (Tab::Other, None));
        assert_eq!(locate("/users/1"), (Tab::Other, None));
    }

    #[test]
    fn renders_the_bar() {
        let html = bar(Tab::Home, Some("/rooms/3/voice")).render().unwrap();
        assert!(html.contains(r#"<link rel="stylesheet" href="/assets/hermes/workspace-1.css">"#));
        assert!(html.contains(r#"<script type="module" src="/assets/hermes/workspace-1.js"></script>"#));
        assert!(html.contains(r#"data-ws-cards-url="/workspace/cards.json""#));
        assert!(html.contains(r#"href="/workspace" aria-current="page""#));
        assert!(html.contains(r#"href="/rooms/3/voice""#));

        let without_report = bar(Tab::Chats, None).render().unwrap();
        assert!(!without_report.contains("Report") && without_report.contains(r#"href="/" aria-current="page""#));

        let hidden = TabBar { show_bar: false, ..bar(Tab::Chats, None) }.render().unwrap();
        assert!(!hidden.contains("<nav") && hidden.contains("workspace-1.js"));
    }
}
