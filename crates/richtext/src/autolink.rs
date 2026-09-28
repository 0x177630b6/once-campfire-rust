//! rails_autolink 1.1.8's `auto_link(text, html: { target: "_blank" }, sanitize_options: ...)`, as
//! `MessagesHelper#message_presentation` calls it. It works on the serialized HTML with regular
//! expressions, so the exact serialization from the earlier steps matters.
//!
//! One deliberate difference closes a stored XSS in rails_autolink: the sanitized HTML is
//! serialized with `<` and `>` escaped in attribute values. Nokogiri leaves them raw, so a URL after
//! a `>` in a `title` looked like text to `auto_linked?`, and the `<a href="...">` inserted there
//! closed the attribute and turned the rest of its value into markup. With them escaped, every `<`
//! and `>` in the text is a tag's, so auto_link only ever inserts links between tags.

use regex::Regex;
use std::sync::LazyLock;

use crate::dom::ParseError;
use crate::ruby::{html_escape, is_blank, url_encode};
use crate::sanitizer::{SafeList, sanitize, sanitize_with_escaped_attribute_brackets};

/// `AUTO_LINK_RE`. Ruby's `\s` and `\w` are ASCII-only.
static AUTO_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:(?i:((?:ed2k|ftp|http|https|irc|mailto|news|gopher|nntp|telnet|webcal|xmpp|callto|feed|svn|urn|aim|rsync|tag|ssh|sftp|rtsp|afs|file):))//|(?i:www)\.[a-zA-Z0-9_])[^ \t\r\n\x0B\x0C<\u{A0}"]+"#,
    )
    .unwrap()
});

/// `AUTO_EMAIL_RE` without its lookbehind, which is checked separately.
static AUTO_EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-zA-Z0-9_.!#$%+-]\.?[a-zA-Z0-9_.!#$%&'*/=?^`{|}~+-]*@[a-zA-Z0-9_-]+(?:\.[a-zA-Z0-9_-]+)+").unwrap()
});

fn is_email_local_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_.!#$%&'*/=?^`{|}~+-".contains(c)
}

/// `AUTO_LINK_CRE[0]`: `/<[^>]+$/`, with `$` matching before any newline or at the end.
static OPEN_TAG_AT_LINE_END_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)<[^>]+$").unwrap());
/// `AUTO_LINK_CRE[1]`: `/^[^>]*>/`
static CLOSES_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^[^>]*>").unwrap());
/// `AUTO_LINK_CRE[2]`: `/<a\b.*?>/i`, where `.` stops at newlines
static OPEN_ANCHOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<a\b.*?>").unwrap());
/// `AUTO_LINK_CRE[3]`: `/<\/a>/i`
static CLOSE_ANCHOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</a>").unwrap());

/// Brackets whose closing half may end a URL when the URL opened it.
fn opening_bracket(closing: char) -> Option<char> {
    match closing {
        ']' => Some('['),
        ')' => Some('('),
        '}' => Some('{'),
        _ => None,
    }
}

/// `auto_link(html, html: { target: "_blank" }, sanitize_options: { tags:, attributes: })`
pub fn auto_link(text: &str, sanitize_options: &SafeList) -> Result<String, ParseError> {
    if is_blank(text) {
        return Ok(String::new());
    }
    let text = sanitize_with_escaped_attribute_brackets(text, sanitize_options)?;
    let text = auto_link_urls(&text)?;
    auto_link_email_addresses(&text)
}

/// `auto_linked?(left, right)`: inside a tag, or inside an unclosed `<a>`.
fn auto_linked(left: &str, right: &str) -> bool {
    if OPEN_TAG_AT_LINE_END_RE.is_match(left) && CLOSES_TAG_RE.is_match(right) {
        return true;
    }
    // `left.rindex(/<a\b.*?>/i)`: the last position at which the pattern matches
    let mut last: Option<regex::Match> = None;
    for (i, _) in left.char_indices().rev() {
        if let Some(m) = OPEN_ANCHOR_RE.find_at(left, i).filter(|m| m.start() == i) {
            last = Some(m);
            break;
        }
    }
    match last {
        Some(m) => !CLOSE_ANCHOR_RE.is_match(&left[m.end()..]),
        None => false,
    }
}

/// Ruby's `\p{Word}`, which is Unicode's word class (as the regex crate's `\w` is).
fn is_word_char(c: char) -> bool {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A\w\z").unwrap());
    WORD.is_match(c.encode_utf8(&mut [0; 4]))
}

fn auto_link_urls(text: &str) -> Result<String, ParseError> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in AUTO_LINK_RE.captures_iter(text) {
        let whole = caps.get(0).unwrap();
        out.push_str(&text[last..whole.start()]);
        last = whole.end();
        let scheme = caps.get(1);
        let mut href = whole.as_str().to_string();
        if auto_linked(&text[..whole.start()], &text[whole.end()..]) {
            out.push_str(&href);
            continue;
        }
        let mut punctuation: Vec<char> = Vec::new();
        while let Some(c) = href.chars().last().filter(|&c| !(is_word_char(c) || matches!(c, '/' | '-' | '=' | ';'))) {
            href.pop();
            punctuation.push(c);
            if let Some(opening) = opening_bracket(c)
                && href.matches(opening).count() > href.matches(c).count() {
                    href.push(punctuation.pop().unwrap());
                    break;
                }
        }
        let mut trailing_gt = "";
        if let Some(stripped) = href.strip_suffix("&gt;") {
            href = stripped.to_string();
            trailing_gt = "&gt;";
        }
        let link_text = href.clone();
        if scheme.is_none() {
            href = format!("http://{href}");
        }
        let link_text = sanitize(&link_text, &SafeList::defaults())?;
        let href = sanitize(&href, &SafeList::defaults())?;
        // content_tag(:a, link_text, attrs, false): nothing escaped but double quotes in attributes
        out.push_str(&format!("<a target=\"_blank\" href=\"{}\">{}</a>", href.replace('"', "&quot;"), link_text));
        // SafeBuffer#+ escapes the (unsafe) punctuation string
        let trailing: String = punctuation.iter().rev().collect();
        out.push_str(&html_escape(&trailing));
        out.push_str(trailing_gt);
    }
    out.push_str(&text[last..]);
    Ok(out)
}

fn auto_link_email_addresses(text: &str) -> Result<String, ParseError> {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut position = 0;
    while position < text.len() {
        let preceded_by_local_char = text[..position].chars().last().is_some_and(is_email_local_char);
        let found = if preceded_by_local_char { None } else { AUTO_EMAIL_RE.find(&text[position..]) };
        let Some(m) = found else {
            position += text[position..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let (start, end) = (position + m.start(), position + m.end());
        let email = &text[start..end];
        out.push_str(&text[copied..start]);
        if auto_linked(&text[..start], &text[end..]) {
            out.push_str(email);
        } else {
            let sanitized = sanitize(email, &SafeList::defaults())?;
            // display_text is only sanitized (and so marked safe) when sanitizing changed the address
            let display = if sanitized == email { html_escape(email) } else { sanitize(email, &SafeList::defaults())? };
            let href = format!("mailto:{}", url_encode(&sanitized).replace("%40", "@"));
            out.push_str(&format!("<a target=\"_blank\" href=\"{}\">{}</a>", html_escape(&href), display));
        }
        copied = end;
        position = end.max(position + 1);
    }
    out.push_str(&text[copied..]);
    Ok(out)
}
