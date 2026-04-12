use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sha2::{Digest, Sha256};
use slug_types::*;
use std::collections::HashMap;

use crate::{
    canonical_path::canonicalize_item,
    path_types::CanonicalItemUrl,
    ranking::connected_components_from_voted_pairs,
};

pub fn api_error(status: StatusCode, error: impl Into<String>, hint: Option<String>) -> axum::response::Response {
    (status, Json(ApiError { ok: false, error: error.into(), hint })).into_response()
}

pub fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

/// Serialize a canonical item for JSON: absolute URLs stay as-is; bare paths get a `/` prefix.
pub fn item_path_for_api(item: &str) -> String {
    if item.starts_with("http://") || item.starts_with("https://") {
        item.to_string()
    } else {
        format!("/{}", item)
    }
}

/// Same as [`item_path_for_api`], but for private rooms ontology items are prefixed with
/// `/r/{short}/{slug}` so the URL matches the web app (`/r/…/~/…` routes).
pub fn item_path_for_api_in_room(item: &str, room_wire: &str) -> String {
    let room = room_wire.trim();
    if room.is_empty() || room == "public" {
        return item_path_for_api(item);
    }
    let Some((short, slug)) = room.split_once('/') else {
        return item_path_for_api(item);
    };
    if short.is_empty() || slug.is_empty() {
        return item_path_for_api(item);
    }
    let Some(c) = CanonicalItemUrl::parse(item) else {
        return item_path_for_api(item);
    };
    let root = CanonicalItemUrl::ontology_root();
    let item_norm = c.as_str().trim_end_matches('/');
    let root_norm = root.as_str().trim_end_matches('/');
    if let Some(tail) = c.tilde_tail() {
        return if tail.is_empty() {
            format!("https://slug.social/r/{short}/{slug}/~")
        } else {
            format!("https://slug.social/r/{short}/{slug}/~/{}", tail)
        };
    }
    if item_norm == root_norm {
        return format!("https://slug.social/r/{short}/{slug}/~");
    }
    item_path_for_api(item)
}

/// Absolute thread URL for forum JSON (`/t/…` vs `/r/…/t/…`).
pub fn forum_thread_web_url(room_wire: &str, thread_tag: &str) -> String {
    let room = room_wire.trim();
    let tag = thread_tag.trim().trim_start_matches('#');
    if room.is_empty() || room == "public" {
        format!("https://slug.social/t/{tag}")
    } else if let Some((short, slug)) = room.split_once('/') {
        if short.is_empty() || slug.is_empty() {
            format!("https://slug.social/t/{tag}")
        } else {
            format!("https://slug.social/r/{short}/{slug}/t/{tag}")
        }
    } else {
        format!("https://slug.social/t/{tag}")
    }
}

/// Resolve an item path as a first-class canonical path.
pub fn resolve_item(item: &str) -> Result<String, String> {
    let canonical = canonicalize_item(item);
    if canonical.is_empty() {
        return Err(format!("empty item path: `{}`", item));
    }
    Ok(canonical)
}

pub fn parse_parent_specs(parent: Option<&String>) -> Vec<String> {
    let s = match parent {
        Some(p) => p.trim(),
        None => return vec![],
    };
    if s.is_empty() {
        return vec![];
    }
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Apply offset+limit pagination to the flattened component rankings.
/// Items are flattened in component order (largest component first), then unranked last.
/// Returns (components, unranked_items) after the window.
pub fn paginate_rankings(
    components: Vec<RankComponent>,
    unranked_items: Vec<String>,
    offset: usize,
    limit: Option<usize>,
) -> (Vec<RankComponent>, Vec<String>) {
    let mut remaining_skip = offset;
    let mut remaining_take = limit.unwrap_or(usize::MAX);
    let mut out_components: Vec<RankComponent> = Vec::new();

    for comp in components {
        if remaining_take == 0 {
            break;
        }
        let n = comp.ranking.len();
        if remaining_skip >= n {
            remaining_skip -= n;
            continue;
        }
        let start = remaining_skip;
        remaining_skip = 0;
        let end = (start + remaining_take).min(n);
        let taken = end - start;
        remaining_take -= taken;
        out_components.push(RankComponent {
            pairs: comp.pairs,
            ranking: comp.ranking[start..end].to_vec(),
        });
    }

    let out_unranked: Vec<String> = if remaining_take > 0 {
        unranked_items
            .into_iter()
            .skip(remaining_skip)
            .take(remaining_take)
            .collect()
    } else {
        vec![]
    };

    (out_components, out_unranked)
}

pub fn pick_random_distinct(items: &[String]) -> Option<(String, String)> {
    use rand::seq::SliceRandom;
    if items.len() < 2 {
        return None;
    }
    let mut rng = rand::thread_rng();
    let left = items.choose(&mut rng)?.clone();
    for _ in 0..8 {
        let right = items.choose(&mut rng)?.clone();
        if right != left {
            return Some((left, right));
        }
    }
    let mut right = items[0].clone();
    if right == left {
        right = items[1].clone();
    }
    Some((left, right))
}

pub fn is_pair_voted(group: &crate::reducer::GroupState, a: &str, b: &str) -> bool {
    let a_key = CanonicalItemUrl(a.to_string());
    let b_key = CanonicalItemUrl(b.to_string());
    let Some(&a_idx) = group.item_to_idx.get(&a_key) else { return false; };
    let Some(&b_idx) = group.item_to_idx.get(&b_key) else { return false; };
    let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
    group.voted_pairs.contains(&(i, j))
}

/// Compute graph connectivity stats for a set of items within the ranking group.
pub fn compute_connectivity_stats(group: &crate::reducer::GroupState, pool: &[String]) -> ConnectivityStats {
    let n = pool.len();

    // Map pool items to global indices (items not yet in the group get no index)
    let global_idxs: Vec<Option<usize>> = pool
        .iter()
        .map(|it| {
            let key = CanonicalItemUrl(it.clone());
            group.item_to_idx.get(&key).copied()
        })
        .collect();
    let present: Vec<usize> = global_idxs.iter().filter_map(|x| *x).collect();

    // Build local index mapping for items that exist in the ranking group
    let global_to_local: HashMap<usize, usize> = present
        .iter()
        .enumerate()
        .map(|(local, &global)| (global, local))
        .collect();

    let (comps, isolates) = connected_components_from_voted_pairs(
        present.len(),
        group.voted_pairs.iter().filter_map(|(i, j)| {
            let li = global_to_local.get(i).copied()?;
            let lj = global_to_local.get(j).copied()?;
            Some((li, lj))
        }),
    );

    // Items not in the ranking group at all are also isolates
    let items_not_in_group = global_idxs.iter().filter(|x| x.is_none()).count();

    let num_components = comps.len() + isolates.len() + items_not_in_group;

    let pairs_voted = group
        .voted_pairs
        .iter()
        .filter(|(i, j)| global_to_local.contains_key(i) && global_to_local.contains_key(j))
        .count();

    ConnectivityStats {
        items: n,
        components: num_components,
        comparisons_until_connected: if num_components > 0 { num_components - 1 } else { 0 },
        pairs_voted,
        pairs_possible: n * n.saturating_sub(1) / 2,
    }
}

pub fn vote_touches_path(a: &str, b: &str, parent_canon: &str) -> bool {
    let under = |item: &str| item == parent_canon || item.starts_with(&format!("{}/", parent_canon));
    under(a) || under(b)
}

#[cfg(test)]
mod wire_url_tests {
    use super::{forum_thread_web_url, item_path_for_api_in_room};

    #[test]
    fn public_room_unchanged() {
        let u = "https://slug.social/~/a/b";
        assert_eq!(item_path_for_api_in_room(u, "public"), u);
    }

    #[test]
    fn private_room_prefixes_ontology() {
        assert_eq!(
            item_path_for_api_in_room("https://slug.social/~/topic/x", "9ab12cd/my-room"),
            "https://slug.social/r/9ab12cd/my-room/~/topic/x"
        );
    }

    #[test]
    fn private_room_ontology_root() {
        assert_eq!(
            item_path_for_api_in_room("https://slug.social/~", "9ab12cd/my-room"),
            "https://slug.social/r/9ab12cd/my-room/~"
        );
        assert_eq!(
            item_path_for_api_in_room("https://slug.social/~/", "9ab12cd/my-room"),
            "https://slug.social/r/9ab12cd/my-room/~"
        );
    }

    #[test]
    fn external_url_untouched_in_private_room() {
        let u = "https://example.com/z";
        assert_eq!(item_path_for_api_in_room(u, "9ab12cd/my-room"), u);
    }

    #[test]
    fn forum_web_public_vs_room() {
        assert_eq!(
            forum_thread_web_url("public", "debate"),
            "https://slug.social/t/debate"
        );
        assert_eq!(
            forum_thread_web_url("9ab12cd/my-room", "#debate"),
            "https://slug.social/r/9ab12cd/my-room/t/debate"
        );
    }
}
