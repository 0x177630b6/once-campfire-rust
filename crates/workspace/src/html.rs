//! Small HTML helpers: escaping, and the text of a message body for matching.

use std::sync::LazyLock;

use regex::Regex;

/// `ERB::Util.html_escape`: `& < > " '`.
pub fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The text a reader sees in `html`: tags dropped, block ends and `<br>` as line breaks, entities
/// decoded, runs of spaces folded, blank lines dropped.
pub fn to_text(html: &str) -> String {
    static BREAK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>|</(p|div|li|h[1-6]|blockquote|pre|tr|ul|ol)\s*>|<(li|p|div|h[1-6])\b[^>]*>").unwrap());
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->|<[^>]*>").unwrap());
    let broken = BREAK.replace_all(html, "\n");
    let text = decode_entities(&TAG.replace_all(&broken, ""));
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The named entities Action Text and the sanitizer emit, plus numeric ones.
pub fn decode_entities(text: &str) -> String {
    static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[a-zA-Z]{2,8});").unwrap());
    ENTITY
        .replace_all(text, |caps: &regex::Captures| {
            let name = &caps[1];
            let decoded = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ if name.starts_with("#x") || name.starts_with("#X") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            decoded.map(String::from).unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

/// At most `limit` characters, cut with an ellipsis.
pub fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let kept: String = value.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_lines_and_decodes_entities() {
        let html = "<div class=\"lexxy-content\"><p>Draft:&nbsp;<b>AC leak</b> &amp; more</p><ul><li>One</li><li>Two</li></ul>Reply <b>confirm</b><br>to file</div>";
        assert_eq!(to_text(html), "Draft: AC leak & more\nOne\nTwo\nReply confirm\nto file");
    }

    #[test]
    fn escapes_and_truncates() {
        assert_eq!(escape(r#"<a href="x">'&'</a>"#), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(decode_entities("&#233;&#xE9;&bogus;"), "éé&bogus;");
    }
}
