//! Card chips: a link to a Fizzy card (`…/<account>/cards/<n>`) in a message becomes a small
//! Fizzy-style card showing its number, title, severity and column, at render time. The stored
//! message is never changed. Only incident-board cards become chips ([`Snapshot::card`]). A card
//! the workspace doesn't know yet (or that is on another board) keeps its plain link, marked
//! with `data-ws-card` so the page can swap in the chip once Fizzy has been asked
//! (`hermes/workspace.js`, `GET /workspace/cards.json`), which is also how chips in cached message
//! fragments stay current.
//!
//! Which links count: those under `FIZZY_URL`, `FIZZY_PUBLIC_URL`, or the base Fizzy itself writes
//! into card URLs (its `BASE_URL`, learned from the cards it returns), for the configured account.

use std::sync::LazyLock;

use regex::Regex;

use crate::cache::Snapshot;
use crate::config::WorkspaceConfig;
use crate::fizzy::Card;
use crate::html::{decode_entities, escape};

/// Recognizes card URLs.
#[derive(Debug, Clone)]
pub struct Matcher {
    /// URL prefixes, e.g. `http://fizzy`, `https://192.168.0.114:8444`.
    bases: Vec<String>,
    /// The account slug; `None` (before the first poll, without `FIZZY_ACCOUNT`) takes any.
    account: Option<String>,
}

impl Matcher {
    pub fn new(config: &WorkspaceConfig, snapshot: &Snapshot) -> Self {
        let mut bases = vec![config.fizzy_url.clone()];
        bases.extend(config.public_url.clone());
        bases.extend(snapshot.origins.iter().cloned());
        bases.dedup();
        Self { bases, account: snapshot.account.clone().or_else(|| config.account.clone()) }
    }

    /// The card number `href` points to, if it's a card of the account under a known base.
    pub fn card_number(&self, href: &str) -> Option<u64> {
        let href = href.trim();
        let rest = self.bases.iter().find_map(|base| {
            let head = href.get(..base.len())?;
            head.eq_ignore_ascii_case(base).then(|| &href[base.len()..]).filter(|rest| rest.starts_with('/'))
        })?;
        let mut parts = rest[1..].splitn(3, '/');
        let (account, cards, tail) = (parts.next()?, parts.next()?, parts.next()?);
        if cards != "cards" || account.is_empty() || !account.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if self.account.as_deref().is_some_and(|wanted| wanted != account) {
            return None;
        }
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        let after = tail[digits..].chars().next();
        if digits == 0 || digits > 12 || after.is_some_and(|c| !matches!(c, '/' | '?' | '#' | '.')) {
            return None;
        }
        tail[..digits].parse().ok()
    }
}

/// The card's page for browsers: `FIZZY_PUBLIC_URL` (else `FIZZY_URL`) `/<account>/cards/<n>`.
pub fn card_link(config: &WorkspaceConfig, snapshot: &Snapshot, number: u64) -> Option<String> {
    let account = snapshot.account.as_deref().or(config.account.as_deref())?;
    Some(format!("{}/{account}/cards/{number}", config.link_base()))
}

/// `html` with its card links turned into chips (or marked for the page to fill in), or `None`
/// when it has none.
pub fn decorate(html: &str, config: &WorkspaceConfig, snapshot: &Snapshot) -> Option<String> {
    static ANCHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<a\b([^>]*)>(.*?)</a\s*>").unwrap());
    static HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*"([^"]*)""#).unwrap());
    if !html.contains("/cards/") {
        return None;
    }
    let matcher = Matcher::new(config, snapshot);
    let mut changed = false;
    let decorated = ANCHOR.replace_all(html, |caps: &regex::Captures| {
        let attributes = &caps[1];
        let number = HREF.captures(attributes).and_then(|href| matcher.card_number(&decode_entities(&href[1])));
        let Some(number) = number else { return caps[0].to_string() };
        if attributes.contains("data-ws-card") {
            return caps[0].to_string();
        }
        changed = true;
        match snapshot.card(number) {
            Some(card) => {
                let original = HREF.captures(attributes).map(|href| decode_entities(&href[1])).unwrap_or_default();
                let link = card_link(config, snapshot, number).unwrap_or(original);
                chip(card, &link)
            }
            None => format!(r#"<a data-ws-card="{number}"{attributes}>{}</a>"#, &caps[2]),
        }
    });
    changed.then(|| decorated.into_owned())
}

/// The chip: the number in the column's colour, then the title, severity and column.
pub fn chip(card: &Card, link: &str) -> String {
    let state = card.state();
    let severity =
        card.severity().map(|severity| format!(r#"<span class="ws-sev ws-sev--{0}">{0}</span>"#, severity.as_str())).unwrap_or_default();
    let board = card.board.as_ref().map(|board| format!(" · {}", board.name)).unwrap_or_default();
    format!(
        concat!(
            r#"<a class="ws-chip ws-cc--{tone}" href="{link}" target="_blank" rel="noopener" data-ws-card="{number}" title="No. {number}{board} · {state}">"#,
            r#"<b class="ws-chip__no">#{number}</b>"#,
            r#"<span class="ws-chip__body"><span class="ws-chip__title">{title}</span>{severity}<span class="ws-chip__state">{state}</span></span>"#,
            r#"</a>"#
        ),
        tone = state.tone(),
        link = escape(link),
        number = card.number,
        board = escape(&board),
        state = escape(state.label()),
        title = escape(if card.title.trim().is_empty() { "Untitled" } else { card.title.trim() }),
        severity = severity,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fizzy::{BoardRef, Column};

    fn config() -> WorkspaceConfig {
        WorkspaceConfig::from_lookup(|name| match name {
            "FIZZY_URL" => Some("http://fizzy".into()),
            "FIZZY_TOKEN" => Some("t".into()),
            "FIZZY_PUBLIC_URL" => Some("https://192.168.0.114:8444".into()),
            _ => None,
        })
        .unwrap()
        .unwrap()
    }

    fn snapshot() -> Snapshot {
        let mut snapshot = Snapshot { account: Some("897".into()), origins: vec!["http://localhost:8484".into()], ..Snapshot::default() };
        snapshot.cards.insert(
            12,
            Card {
                number: 12,
                title: "Lift B <out> of service".into(),
                tags: vec!["incident".into(), "sev-high".into()],
                board: Some(BoardRef { id: "b".into(), name: "Incident Log".into() }),
                column: Some(Column { id: "c".into(), name: "In progress".into(), color: Some("Lime".into()) }),
                ..Card::default()
            },
        );
        snapshot
    }

    #[test]
    fn matches_card_urls_under_known_bases() {
        let matcher = Matcher::new(&config(), &snapshot());
        assert_eq!(matcher.card_number("http://fizzy/897/cards/12"), Some(12));
        assert_eq!(matcher.card_number("HTTPS://192.168.0.114:8444/897/cards/7#comment_1"), Some(7));
        assert_eq!(matcher.card_number("http://localhost:8484/897/cards/13/"), Some(13));
        assert_eq!(matcher.card_number("http://fizzy/897/cards/12.json"), Some(12));
        for other in [
            "http://fizzy/898/cards/12",
            "http://fizzy/897/cards/abc",
            "http://fizzy/897/cards/12x",
            "http://fizzy/897/boards/12",
            "http://fizzy.evil/897/cards/12",
            "https://example.com/897/cards/12",
            "http://fizzy",
        ] {
            assert_eq!(matcher.card_number(other), None, "{other}");
        }
        let any_account = Matcher::new(&config(), &Snapshot::default());
        assert_eq!(any_account.card_number("http://fizzy/1/cards/3"), Some(3));
    }

    #[test]
    fn known_cards_become_chips_and_unknown_ones_are_marked() {
        let html = concat!(
            r#"<div class="lexxy-content">Filed: <a target="_blank" href="http://localhost:8484/897/cards/12">http://localhost:8484/897/cards/12</a>"#,
            r#" and <a href="http://fizzy/897/cards/99">#99</a>, see <a href="https://example.com">x</a></div>"#
        );
        let out = decorate(html, &config(), &snapshot()).unwrap();
        assert!(out.contains(r#"<a class="ws-chip ws-cc--lime" href="https://192.168.0.114:8444/897/cards/12" target="_blank" rel="noopener" data-ws-card="12""#), "{out}");
        assert!(out.contains(r#"<span class="ws-chip__title">Lift B &lt;out&gt; of service</span><span class="ws-sev ws-sev--high">high</span><span class="ws-chip__state">In progress</span>"#), "{out}");
        assert!(out.contains(r#"<a data-ws-card="99" href="http://fizzy/897/cards/99">#99</a>"#), "{out}");
        assert!(out.contains(r#"<a href="https://example.com">x</a>"#));
        assert!(!out.contains("localhost:8484/897/cards/12</a>"));
    }

    #[test]
    fn leaves_other_messages_alone() {
        assert_eq!(decorate("<p>no links</p>", &config(), &snapshot()), None);
        assert_eq!(decorate(r#"<a href="https://example.com/cards/1">x</a>"#, &config(), &snapshot()), None);
        let chip = decorate(r#"<a href="http://fizzy/897/cards/12">x</a>"#, &config(), &snapshot()).unwrap();
        assert_eq!(decorate(&chip, &config(), &snapshot()), None, "idempotent");
    }
}
