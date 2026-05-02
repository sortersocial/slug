use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sha2::{Digest, Sha256};
use slug_types::paths::GardenItemUrl;
use slug_types::ItemId;
use slug_types::*;
use std::collections::HashMap;

use crate::{
    canonical_path::canonicalize_item,
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

/// Resolve DSL/user input to a stored canonical item id.
pub fn resolve_item(item: &str) -> Result<ItemId, String> {
    let canonical = canonicalize_item(item);
    if canonical.is_empty() {
        return Err(format!("empty item path: `{}`", item));
    }
    ItemId::parse(&canonical).ok_or_else(|| format!("invalid item path: `{}`", item))
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
pub fn paginate_rankings(
    components: Vec<RankComponent>,
    unranked_items: Vec<GardenItemUrl>,
    offset: usize,
    limit: Option<usize>,
) -> (Vec<RankComponent>, Vec<GardenItemUrl>) {
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

    let out_unranked: Vec<GardenItemUrl> = if remaining_take > 0 {
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

pub fn pick_random_distinct_canonical(items: &[ItemId]) -> Option<(ItemId, ItemId)> {
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
    let a_key = ItemId::parse(a).unwrap_or_else(|| ItemId::opaque(a.to_string()));
    let b_key = ItemId::parse(b).unwrap_or_else(|| ItemId::opaque(b.to_string()));
    let Some(&a_idx) = group.item_to_idx.get(&a_key) else { return false; };
    let Some(&b_idx) = group.item_to_idx.get(&b_key) else { return false; };
    let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
    group.voted_pairs.contains(&(i, j))
}

pub fn compute_connectivity_stats(group: &crate::reducer::GroupState, pool: &[ItemId]) -> ConnectivityStats {
    let n = pool.len();

    let global_idxs: Vec<Option<usize>> = pool
        .iter()
        .map(|it| group.item_to_idx.get(it).copied())
        .collect();
    let present: Vec<usize> = global_idxs.iter().filter_map(|x| *x).collect();

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
