//! Thread timeline as XML for CLI `forum show` and browser copy-thread.

use crate::timeago::timeago_compact;

/// Blank line block between consecutive thread rows in CLI / copy output.
pub const ITEM_SEPARATOR: &str = "\n\n\n";

fn escape_xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Opening tag for one forum post (body omitted).
pub fn post_open_tag(
    index: usize,
    timeago: &str,
    principal: &str,
    delegate: Option<&str>,
) -> String {
    let delegate_attr = delegate.unwrap_or("");
    format!(
        "<post index=\"{}\" timeago=\"{}\" principal=\"{}\" delegate=\"{}\">",
        index,
        escape_xml_attr(timeago),
        escape_xml_attr(principal),
        escape_xml_attr(delegate_attr),
    )
}

/// One forum post element: opening tag, trimmed body, closing tag.
pub fn format_post(
    index: usize,
    timeago: &str,
    principal: &str,
    delegate: Option<&str>,
    body: &str,
) -> String {
    format!(
        "{}\n{}\n</post>",
        post_open_tag(index, timeago, principal, delegate),
        body.trim(),
    )
}

/// One room system line in thread XML.
pub fn format_system(timeago: &str, text: &str) -> String {
    format!(
        "<system timeago=\"{}\">{}</system>",
        escape_xml_attr(timeago),
        text.trim(),
    )
}

/// Format a post row using wall-clock `now_ms` for the `timeago` attribute.
pub fn format_post_at(
    now_ms: i64,
    index: usize,
    ts: i64,
    principal: &str,
    delegate: Option<&str>,
    body: &str,
) -> String {
    let timeago = timeago_compact(now_ms, ts);
    format_post(index, &timeago, principal, delegate, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_includes_principal_and_delegate() {
        let xml = format_post(
            2,
            "1h2m3s",
            "alice",
            Some("00000000-0000-0000-0000-000000000000:test:local/dev"),
            "hello\n",
        );
        assert!(xml.starts_with(
            "<post index=\"2\" timeago=\"1h2m3s\" principal=\"alice\" delegate=\"00000000-0000-0000-0000-000000000000:test:local/dev\">"
        ));
        assert!(xml.ends_with("\nhello\n</post>"));
    }

    #[test]
    fn post_empty_delegate_when_human() {
        let xml = format_post(0, "5s", "bob", None, "hi");
        assert!(
            xml.starts_with("<post index=\"0\" timeago=\"5s\" principal=\"bob\" delegate=\"\">")
        );
    }

    #[test]
    fn escapes_attribute_values() {
        let xml = post_open_tag(0, "1s", "a&b", Some("d\"q"));
        assert_eq!(
            xml,
            "<post index=\"0\" timeago=\"1s\" principal=\"a&amp;b\" delegate=\"d&quot;q\">"
        );
    }
}
