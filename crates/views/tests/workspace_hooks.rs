//! Hermes fork: the Duty Manager Workspace's rendering seams (`campfire_views::hermes`). The
//! hooks are process-wide, so this is a test binary of its own, with a single test: nothing else
//! renders while they're installed.

mod messages_support;

use std::sync::Arc;

use askama::Template;
use campfire_views::ViewContext;
use campfire_views::hermes::{WorkspaceHooks, install_workspace_hooks};
use campfire_views::messages::MessageView;
use campfire_views::rooms::{self, ShowView};
use messages_support::golden;

struct Marking;

impl WorkspaceHooks for Marking {
    fn stylesheets(&self, _ctx: &ViewContext) -> Vec<String> {
        vec!["/assets/ws-test.css".into()]
    }

    fn layout_overlay(&self, ctx: &ViewContext) -> String {
        format!("<nav class=\"ws-test\" data-user=\"{}\"></nav>", ctx.current_user_id().unwrap_or(0))
    }

    fn message_html(&self, message: &MessageView, html: &str) -> Option<String> {
        (message.id % 2 == 0).then(|| format!("{html}<i class=\"ws-test-message\"></i>"))
    }
}

fn room_page() -> String {
    let g = golden("rooms_show_closed");
    let show: ShowView = g.input();
    g.render(|ctx| rooms::Show { ctx, show: &show }.render().unwrap())
}

#[test]
fn the_hooks_render_nothing_until_installed_and_nothing_once_removed() {
    let upstream = room_page();
    assert!(!upstream.contains("ws-test"));

    install_workspace_hooks(Some(Arc::new(Marking)));
    let decorated = room_page();
    install_workspace_hooks(None);

    // The stylesheet goes once, in the head.
    let link = "\n    <link rel=\"stylesheet\" href=\"/assets/ws-test.css\" data-turbo-track=\"reload\" />";
    assert_eq!(decorated.matches(link).count(), 1);
    assert!(decorated.find(link).unwrap() < decorated.find("</head>").unwrap());
    let decorated = decorated.replace(link, "");
    // The overlay goes once, right after the lightbox, before the app logo.
    assert_eq!(decorated.matches("<nav class=\"ws-test\"").count(), 1);
    let overlay = decorated.find("<nav class=\"ws-test\"").unwrap();
    assert!(overlay > decorated.find("id=\"lightbox\"").unwrap_or(0) && overlay < decorated.find("id=\"app-logo\"").unwrap());
    // Text messages go through the message hook; it may leave some alone.
    assert!(decorated.contains("<i class=\"ws-test-message\"></i>"));
    let overlay_end = overlay + decorated[overlay..].find("</nav>").unwrap() + "</nav>".len();
    let without = format!("{}{}", &decorated[..overlay], &decorated[overlay_end..]).replace("<i class=\"ws-test-message\"></i>", "");
    assert_eq!(without, upstream, "nothing else changes");

    assert_eq!(room_page(), upstream, "removed: back to upstream's bytes");
}
