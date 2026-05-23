use axum::http::Uri;

use crate::{
    canonical_path::canonicalize_item,
    html::forum::ThreadNav,
    path_types::ItemId,
};

pub(super) fn item_display_path(item: &str) -> String {
    ItemId::parse(item)
        .map(|c| c.display_path())
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

pub(super) fn child_depth_from_uri(uri: &Uri) -> usize {
    uri.query()
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(k, v)| {
            if k == "depth" {
                v.parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(1)
        .clamp(1, 5)
}

// re-export for vote module tests that used canonicalize_tag via pick_autothread - not needed here
