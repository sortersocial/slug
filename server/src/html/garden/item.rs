use axum::http::Uri;
use maud::{html, Markup};

use crate::{
    canonical_path::canonicalize_item,
    html::forum::ThreadNav,
    path_types::ItemId,
};

/// Sentinel for "every descendant" ranking depth (`?depth=all`).
pub(super) const GARDEN_DEPTH_ALL: usize = usize::MAX;

pub(super) fn item_display_path(item: &str) -> String {
    ItemId::parse(item)
        .map(|c| c.ontology_leaf().display_path())
        .unwrap_or_else(|| canonicalize_item(item))
}

/// Garden href for an item path string in this nav scope.
pub(super) fn item_href(item: &str, nav: &ThreadNav) -> String {
    nav.garden_item_url(item)
}

pub(super) fn item_code_label(item: &str) -> String {
    item_display_path(item)
}

pub(super) fn login_href_with_next(next: &str) -> String {
    let next = if next.trim().starts_with('/') && !next.trim().starts_with("//") {
        next.trim()
    } else {
        "/"
    };
    format!("/login?next={}", urlencoding::encode(next))
}

/// Parse a garden `depth` query/CLI value. Default / invalid → 1.
/// Accepts `all`, `∞`, `inf`, `infinity`, `*` for unbounded descendant depth.
pub(super) fn parse_garden_depth(raw: &str) -> usize {
    let s = raw.trim();
    if s.is_empty() {
        return 1;
    }
    // Percent-decoding for ∞ (%E2%88%9E) if a client encodes the glyph.
    let decoded = percent_decode_basic(s);
    let key = decoded.as_str();
    if matches!(
        key,
        "all" | "∞" | "inf" | "infinity" | "*" | "unlimited"
    ) {
        return GARDEN_DEPTH_ALL;
    }
    key.parse::<usize>().unwrap_or(1).max(1)
}

fn percent_decode_basic(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub(super) fn garden_depth_query_value(depth: usize) -> String {
    if depth == GARDEN_DEPTH_ALL {
        "all".to_string()
    } else {
        depth.to_string()
    }
}

pub(super) fn child_depth_from_uri(uri: &Uri) -> usize {
    uri.query()
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(k, v)| {
            if k == "depth" {
                Some(parse_garden_depth(v))
            } else {
                None
            }
        })
        .unwrap_or(1)
}

/// GET navigation depth control. Options: 1, 2, 3, ∞ (`all`).
///
/// Uses a GET form + `this.form.submit()` (not `new URL(...)`). Inline handlers
/// resolve `URL` against `document.URL` (a string), which throws
/// "URL is not a constructor" and leaves the page unchanged.
pub(super) fn garden_depth_select_markup(current: usize) -> Markup {
    let current_val = garden_depth_query_value(current);
    let mut options: Vec<(String, String)> = vec![
        ("1".into(), "1".into()),
        ("2".into(), "2".into()),
        ("3".into(), "3".into()),
        ("all".into(), "∞".into()),
    ];
    if !options.iter().any(|(v, _)| v == &current_val) {
        let insert_at = options.len().saturating_sub(1);
        options.insert(insert_at, (current_val.clone(), current_val.clone()));
    }
    html! {
        form method="get" class="garden-depth-control" {
            label {
                span class="muted" { "depth" }
                " "
                select id="garden-depth-select" name="depth" aria-label="Garden ranking depth"
                    onchange="this.form.submit()" {
                    @for (val, label) in &options {
                        @if *val == current_val {
                            option value=(val) selected { (label) }
                        } @else {
                            option value=(val) { (label) }
                        }
                    }
                }
            }
        }
    }
}

// re-export for vote module tests that used canonicalize_tag via pick_autothread - not needed here

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Uri;

    #[test]
    fn parse_garden_depth_defaults_and_all() {
        assert_eq!(parse_garden_depth(""), 1);
        assert_eq!(parse_garden_depth("1"), 1);
        assert_eq!(parse_garden_depth("3"), 3);
        assert_eq!(parse_garden_depth("0"), 1);
        assert_eq!(parse_garden_depth("all"), GARDEN_DEPTH_ALL);
        assert_eq!(parse_garden_depth("∞"), GARDEN_DEPTH_ALL);
        assert_eq!(parse_garden_depth("inf"), GARDEN_DEPTH_ALL);
        assert_eq!(parse_garden_depth("%E2%88%9E"), GARDEN_DEPTH_ALL);
        assert_eq!(parse_garden_depth("nope"), 1);
    }

    #[test]
    fn child_depth_from_uri_reads_query() {
        let d1: Uri = "/~/topic".parse().unwrap();
        assert_eq!(child_depth_from_uri(&d1), 1);
        let d2: Uri = "/~/topic?depth=2".parse().unwrap();
        assert_eq!(child_depth_from_uri(&d2), 2);
        let dall: Uri = "/~?depth=all".parse().unwrap();
        assert_eq!(child_depth_from_uri(&dall), GARDEN_DEPTH_ALL);
    }

    #[test]
    fn item_display_path_emits_leaf_form() {
        assert_eq!(item_display_path("~/x/luke"), "~/luke");
        assert_eq!(item_display_path("~/luke"), "~/luke");
        assert_eq!(item_display_path("https://slug.social/~/x/luke"), "~/luke");
    }

    #[test]
    fn garden_depth_select_marks_current() {
        let html = garden_depth_select_markup(2).into_string();
        assert!(html.contains("id=\"garden-depth-select\""));
        assert!(html.contains("method=\"get\""));
        assert!(html.contains("name=\"depth\""));
        assert!(html.contains("onchange=\"this.form.submit()\""));
        assert!(html.contains("value=\"2\" selected"));
        assert!(html.contains("value=\"all\""));
        assert!(html.contains(">∞<"));
        let all = garden_depth_select_markup(GARDEN_DEPTH_ALL).into_string();
        assert!(all.contains("value=\"all\" selected"));
    }
}