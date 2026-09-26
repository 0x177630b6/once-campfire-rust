//! The slice of `Nokogiri::HTML(html)` (libxml2's legacy HTML parser, in recovery mode) that
//! `Opengraph::Document` reads: every `<meta>` element's attributes.
//!
//! The body string comes from `Opengraph::Fetch`, tagged UTF-8, so libxml2 decodes it as UTF-8
//! whatever the page declares, taking a byte that isn't valid UTF-8 as Latin-1. Then it tokenizes
//! the way its pre-HTML5 parser does: `<script>`/`<style>` hold raw text, comments and
//! `<!…>`/`<?…>` markup are skipped, tag and attribute names are lowercased, the first of a
//! repeated attribute wins, and attribute values decode HTML 4 entities only when terminated by
//! `;` and numeric references with or without one (an invalid one cuts the value short, as the
//! NUL it produces ends libxml2's C string).

use super::entities::ENTITIES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub attributes: Vec<(String, String)>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attributes.iter().any(|(n, _)| n == name)
    }
}

/// libxml2 reading a UTF-8 buffer: valid sequences decode, any other byte is taken as Latin-1.
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                out.push_str(std::str::from_utf8(valid).expect("validated"));
                out.push(after[0] as char);
                rest = &after[1..];
            }
        }
    }
    out
}

/// The `<meta>` elements of the document, in document order.
pub fn meta_elements(html: &str) -> Vec<Element> {
    // A NUL ends libxml2's input
    let html = &html[..html.find('\0').unwrap_or(html.len())];
    let mut scanner = Scanner { chars: html.chars().collect(), pos: 0 };
    let mut metas = Vec::new();
    while let Some(c) = scanner.peek(0) {
        if c != '<' {
            scanner.pos += 1;
            continue;
        }
        match scanner.peek(1) {
            Some('/') => scanner.end_tag(),
            Some('!') => scanner.markup_declaration(),
            Some('?') => scanner.skip_past('>'),
            Some(c) if c.is_ascii_alphabetic() => {
                let (name, element, self_closing) = scanner.start_tag();
                if name == "meta" {
                    metas.push(element);
                } else if (name == "script" || name == "style") && !self_closing {
                    scanner.raw_text(&name);
                }
            }
            _ => scanner.pos += 1,
        }
    }
    metas
}

struct Scanner {
    chars: Vec<char>,
    pos: usize,
}

fn is_blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Scanner {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn starts_with_ignore_case(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(i, t)| self.peek(i).is_some_and(|c| c.eq_ignore_ascii_case(&t)))
    }

    fn skip_blanks(&mut self) {
        while self.peek(0).is_some_and(is_blank) {
            self.pos += 1;
        }
    }

    /// Moves past the next `c` (or to the end).
    fn skip_past(&mut self, c: char) {
        while let Some(next) = self.peek(0) {
            self.pos += 1;
            if next == c {
                break;
            }
        }
    }

    /// `htmlParseHTMLName`: `[A-Za-z_:.][A-Za-z0-9:_.-]*`, lowercased.
    fn html_name(&mut self) -> Option<String> {
        let first = self.peek(0)?;
        if !(first.is_ascii_alphabetic() || matches!(first, '_' | ':' | '.')) {
            return None;
        }
        let mut name = String::new();
        while let Some(c) = self.peek(0).filter(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.')) {
            name.push(c.to_ascii_lowercase());
            self.pos += 1;
        }
        Some(name)
    }

    fn end_tag(&mut self) {
        self.pos += 2;
        if self.html_name().is_some() {
            self.skip_past('>');
        }
    }

    /// `<!--…-->` (with `<!-->` and `<!--->` closing at once, and `--!>` accepted), `<!DOCTYPE…>`,
    /// and any other `<!…>` skipped as a bogus comment.
    fn markup_declaration(&mut self) {
        if self.peek(2) == Some('-') && self.peek(3) == Some('-') {
            self.pos += 4;
            if self.peek(0) == Some('>') {
                self.pos += 1;
                return;
            }
            if self.peek(0) == Some('-') && self.peek(1) == Some('>') {
                self.pos += 2;
                return;
            }
            while self.pos < self.chars.len() {
                if self.starts_with_ignore_case("-->") {
                    self.pos += 3;
                    return;
                }
                if self.starts_with_ignore_case("--!>") {
                    self.pos += 4;
                    return;
                }
                self.pos += 1;
            }
        } else {
            self.skip_past('>');
        }
    }

    /// `htmlParseStartTag`: returns the name, the element, and whether it ended with `/>`.
    fn start_tag(&mut self) -> (String, Element, bool) {
        self.pos += 1;
        let name = self.html_name().unwrap_or_default();
        let mut attributes: Vec<(String, String)> = Vec::new();
        self.skip_blanks();
        loop {
            match self.peek(0) {
                None => break,
                Some('>') => break,
                Some('/') if self.peek(1) == Some('>') => break,
                _ => {}
            }
            match self.html_name() {
                Some(attribute) => {
                    self.skip_blanks();
                    let value = if self.peek(0) == Some('=') {
                        self.pos += 1;
                        self.skip_blanks();
                        self.attribute_value()
                    } else {
                        String::new()
                    };
                    if !attributes.iter().any(|(n, _)| *n == attribute) {
                        attributes.push((attribute, value));
                    }
                }
                None => {
                    // Dump the bogus attribute string up to the next blank or the end of the tag
                    while let Some(c) = self.peek(0) {
                        if is_blank(c) || c == '>' || (c == '/' && self.peek(1) == Some('>')) {
                            break;
                        }
                        self.pos += 1;
                    }
                }
            }
            self.skip_blanks();
        }
        let self_closing = self.peek(0) == Some('/');
        if self_closing {
            self.pos += 2;
        } else if self.peek(0) == Some('>') {
            self.pos += 1;
        }
        (name, Element { attributes }, self_closing)
    }

    /// `htmlParseAttValue`
    fn attribute_value(&mut self) -> String {
        match self.peek(0) {
            Some(quote @ ('"' | '\'')) => {
                self.pos += 1;
                let value = self.attribute_text(Some(quote));
                if self.peek(0) == Some(quote) {
                    self.pos += 1;
                }
                value
            }
            _ => self.attribute_text(None),
        }
    }

    /// `htmlParseHTMLAttribute`: up to the quote, or (unquoted) a blank or `>`.
    fn attribute_text(&mut self, stop: Option<char>) -> String {
        let mut out = String::new();
        let mut truncated = false;
        while let Some(c) = self.peek(0) {
            if Some(c) == stop || (stop.is_none() && (c == '>' || is_blank(c))) {
                break;
            }
            if c != '&' {
                if !truncated {
                    out.push(c);
                }
                self.pos += 1;
                continue;
            }
            let decoded = if self.peek(1) == Some('#') { self.char_ref().map(|c| c.to_string()) } else { Some(self.entity_ref()) };
            match decoded {
                Some(text) if !truncated => out.push_str(&text),
                Some(_) => {}
                None => truncated = true,
            }
        }
        out
    }

    /// `htmlParseCharRef`: `None` for a value that isn't a valid XML character.
    fn char_ref(&mut self) -> Option<char> {
        let hex = matches!(self.peek(2), Some('x' | 'X'));
        self.pos += if hex { 3 } else { 2 };
        let radix = if hex { 16 } else { 10 };
        let mut value: u32 = 0;
        while let Some(c) = self.peek(0) {
            if c == ';' {
                self.pos += 1;
                break;
            }
            let Some(digit) = c.to_digit(radix) else { break };
            if value < 0x110000 {
                value = value * radix + digit;
            }
            self.pos += 1;
        }
        let is_char = matches!(value, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF);
        if is_char { char::from_u32(value) } else { None }
    }

    /// `htmlParseEntityRef`: a known name followed by `;` decodes; anything else stays as written.
    fn entity_ref(&mut self) -> String {
        self.pos += 1;
        let start = self.pos;
        if self.peek(0).is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | ':')) {
            while self.peek(0).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-')) {
                self.pos += 1;
            }
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        if !name.is_empty() && self.peek(0) == Some(';') {
            if let Ok(index) = ENTITIES.binary_search_by(|(n, _)| (*n).cmp(name.as_str())) {
                self.pos += 1;
                return char::from_u32(ENTITIES[index].1).map(String::from).unwrap_or_default();
            }
        }
        format!("&{name}")
    }

    /// `htmlParseScript`: everything up to `</name` (any case) is text.
    fn raw_text(&mut self, name: &str) {
        let end = format!("</{name}");
        while self.pos < self.chars.len() && !self.starts_with_ignore_case(&end) {
            self.pos += 1;
        }
    }
}

/// `Nokogiri::HTML4::Document#meta_encoding`: the first `meta[@charset]`, else the charset in
/// the first `http-equiv="Content-Type"` meta with a `content`.
pub fn meta_encoding(metas: &[Element]) -> Option<String> {
    if let Some(meta) = metas.iter().find(|m| m.has_attr("charset")) {
        return meta.attr("charset").map(str::to_string);
    }
    let meta = metas
        .iter()
        .find(|m| m.has_attr("content") && m.attr("http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("content-type")))?;
    charset_in(meta.attr("content")?)
}

/// `content[/charset\s*=\s*([\w-]+)/i, 1]`
fn charset_in(content: &str) -> Option<String> {
    static CHARSET: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i-u)charset[ \t\n\x0B\x0C\r]*=[ \t\n\x0B\x0C\r]*([A-Za-z0-9_-]+)").unwrap());
    CHARSET.captures(content).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(html: &str) -> Option<String> {
        meta_elements(&format!("<meta charset=utf-8>{html}"))
            .into_iter()
            .filter(|m| m.attr("property") == Some("og:title"))
            .last()
            .and_then(|m| m.attr("content").filter(|c| !c.is_empty()).map(str::to_string))
    }

    fn title_of(content: &str) -> Option<String> {
        title(&format!("<meta property=\"og:title\" content=\"a{content}b\">"))
    }

    /// Probed against the reference's Nokogiri 1.19.4.
    #[test]
    fn decodes_references_like_libxml2() {
        for (reference, expected) in [
            ("&apos;", "a'b"),
            ("&eacute", "a&eacuteb"),
            ("&eacute;x", "aéxb"),
            ("&#233", "aéb"),
            ("&#233x", "aéxb"),
            ("&#xE9", "a\u{0E9B}"),
            ("&#xe9;", "aéb"),
            ("&AMP;", "a&AMP;b"),
            ("&Eacute;", "aÉb"),
            ("&unknown;", "a&unknown;b"),
            ("& x", "a& xb"),
            ("&#65;&#x41;", "aAAb"),
            ("&#128;", "a\u{80}b"),
            ("&#150;", "a\u{96}b"),
            ("&#xD800;", "a"),
            ("&#1114112;", "a"),
            ("&lt", "a&ltb"),
            ("&amp;amp;", "a&amp;b"),
            ("&hellip;", "a…b"),
            ("&nbsp", "a&nbspb"),
            ("&#;", "a"),
            ("&#x;", "a"),
        ] {
            assert_eq!(title_of(reference).as_deref(), Some(expected), "{reference}");
        }
    }

    #[test]
    fn tokenizes_like_libxml2() {
        let cases: &[(&str, Option<&str>)] = &[
            ("<script><meta property=\"og:title\" content=\"in script\"></script><meta property=\"og:title\" content=\"after\">", Some("after")),
            ("<style><meta property=\"og:title\" content=\"in style\"></style>", None),
            ("<textarea><meta property=\"og:title\" content=\"in textarea\"></textarea>", Some("in textarea")),
            ("<title><meta property=\"og:title\" content=\"in title\"></title>", Some("in title")),
            ("<noscript><meta property=\"og:title\" content=\"in noscript\"></noscript>", Some("in noscript")),
            ("<template><meta property=\"og:title\" content=\"in template\"></template>", Some("in template")),
            ("<svg><meta property=\"og:title\" content=\"in svg\"></svg>", Some("in svg")),
            ("<!-- <meta property=\"og:title\" content=\"comment\"> --><p>", None),
            ("<meta property=og:title content=unquoted>", Some("unquoted")),
            ("<meta property=\"og:title\" content=\"line1\r\nline2\">", Some("line1\r\nline2")),
            ("<meta property='og:title' content='single'>", Some("single")),
            ("<meta property = \"og:title\" content = \"spaced\">", Some("spaced")),
            ("<META PROPERTY=\"og:title\" CONTENT=\"upper\">", Some("upper")),
            ("<meta property=\"og:title\"content=\"nospace\">", Some("nospace")),
            ("<meta/property=\"og:title\"/content=\"slashes\">", None),
            ("<meta property=\"og:title\" content=\"<b>tag</b>\">", Some("<b>tag</b>")),
            ("<meta property=\"og:title\" content=\"a\">b\">", Some("a")),
            ("<meta property=\"og:title\" content=\"unterminated>", Some("unterminated>")),
            ("<meta property=\"og:title\" content=unq\"uoted>", Some("unq\"uoted")),
            ("<meta property=\"og:title\" content=a&amp;b>", Some("a&b")),
            ("<!--> <meta property=\"og:title\" content=\"after empty comment\"> -->", Some("after empty comment")),
            ("<!---> <meta property=\"og:title\" content=\"after dash comment\"> -->", Some("after dash comment")),
            ("<!DOCTYPE html><meta property=\"og:title\" content=\"doctype\">", Some("doctype")),
            ("<?xml version=\"1.0\"?><meta property=\"og:title\" content=\"pi\">", Some("pi")),
            ("<![CDATA[ <meta property=\"og:title\" content=\"cdata\"> ]]>", None),
            ("<p <meta property=\"og:title\" content=\"broken\">", None),
            ("< meta property=\"og:title\" content=\"space\">", None),
            ("<meta property=\"og:title\" content=\"tab\there\">", Some("tab\there")),
            ("<meta property=\"og:title\" content=\"\x00nul\">", None),
        ];
        for (html, expected) in cases {
            assert_eq!(title(html).as_deref(), *expected, "{html}");
        }
    }

    #[test]
    fn decodes_bytes_like_libxml2_reading_utf8() {
        assert_eq!(decode(b"caf\xc3\xa9 \xff x"), "café ÿ x");
        assert_eq!(decode(b"\xff caf\xc3\xa9 x"), "ÿ café x");
        assert_eq!(decode(b"\x93q\x94"), "\u{93}q\u{94}");
        assert_eq!(decode(b"\x82\xa0"), "\u{82}\u{a0}");
    }

    #[test]
    fn finds_the_meta_encoding_like_nokogiri() {
        let encoding = |html: &str| meta_encoding(&meta_elements(html));
        assert_eq!(encoding("<meta charset=\"iso-8859-1\">"), Some("iso-8859-1".into()));
        assert_eq!(encoding("<meta charset=\"\">"), Some("".into()));
        assert_eq!(encoding("<meta http-equiv=\"content-type\" content=\"text/html; charset=iso-8859-1\">"), Some("iso-8859-1".into()));
        assert_eq!(encoding("<meta http-equiv=\"Content-Type\" content=\"text/html\"><meta http-equiv=\"Content-Type\" content=\"charset=utf-8\">"), None);
        assert_eq!(encoding("<meta http-equiv=\"refresh\" content=\"charset=utf-8\">"), None);
        assert_eq!(encoding("<meta property=\"og:title\" content=\"x\">"), None);
    }
}
