//! Shared logic for scoped ranking (connected components under a parent).
//! Used by both the HTML garden view and the API rank endpoint.

use std::collections::{HashMap, HashSet};

use crate::path_types::ItemId;
use crate::ranking::{connected_components_from_voted_pairs, rank_partition, RankedItem};
use crate::reducer::{ContentState, GroupState};

#[derive(Debug, Clone)]
pub struct ScopedComponent {
    pub pairs: usize,
    pub ranked: Vec<RankedItem>,
}

#[derive(Debug, Clone)]
pub struct ChildrenRankings {
    pub component_rankings: Vec<ScopedComponent>,
    /// Items in scope with no rank (no votes connecting them to others in this scope).
    pub unranked_items: Vec<ItemId>,
}

/// Resolve one scope spec (literal path) to direct children of that parent. No wildcards.
fn resolve_one_scope(content: &ContentState, spec: &str) -> HashSet<ItemId> {
    let Some(parent) = ItemId::parse(spec.trim()) else {
        return HashSet::new();
    };
    content
        .item_children
        .get(&parent)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Resolve multiple scope specs (literal paths) to a single merged, deduplicated list of item paths.
/// Used for rank/pair with multiple parents (explicit merge, e.g. rank ~/models ~/ai-models).
pub fn resolve_scope(content: &ContentState, specs: &[String]) -> Vec<ItemId> {
    let mut set = HashSet::new();
    for spec in specs {
        set.extend(resolve_one_scope(content, spec));
    }
    let mut out: Vec<ItemId> = set.into_iter().collect();
    out.sort();
    out
}

/// Resolve scope specs recursively up to `depth` levels deep.
/// depth=1 is equivalent to resolve_scope (direct children only).
/// depth=2 includes grandchildren, etc.
/// Large depths (including `usize::MAX` for "all") stop early when the frontier empties.
pub fn resolve_scope_recursive(
    content: &ContentState,
    specs: &[String],
    depth: usize,
) -> Vec<ItemId> {
    if depth == 0 {
        return vec![];
    }
    let mut visited: HashSet<ItemId> = HashSet::new();
    let mut frontier: Vec<ItemId> = specs.iter().filter_map(|s| ItemId::parse(s)).collect();

    for _level in 0..depth {
        let mut next_frontier: Vec<ItemId> = Vec::new();
        for parent in &frontier {
            if let Some(children) = content.item_children.get(parent) {
                for child in children {
                    if visited.insert(child.clone()) {
                        next_frontier.push(child.clone());
                    }
                }
            }
        }
        if next_frontier.is_empty() {
            break;
        }
        frontier = next_frontier;
    }

    let mut out: Vec<ItemId> = visited.into_iter().collect();
    out.sort();
    out
}

/// Build connected-component rankings for an explicit set of item paths.
/// Use this when scope comes from multiple parents (resolve_scope).
pub fn build_rankings_for_item_set(
    content: &ContentState,
    items_in_scope: &[ItemId],
) -> ChildrenRankings {
    let group = &content.ranking_group;
    let mut items_in_scope: Vec<ItemId> = items_in_scope.to_vec();
    items_in_scope.sort();

    let scoped_idxs: Vec<usize> = items_in_scope
        .iter()
        .filter_map(|it| group.item_to_idx.get(it).copied())
        .collect();
    let local_to_global: Vec<usize> = scoped_idxs.clone();
    let global_to_local: HashMap<usize, usize> = scoped_idxs
        .iter()
        .enumerate()
        .map(|(local, global)| (*global, local))
        .collect();
    let (mut comps_local, isolate_local_idxs) = connected_components_from_voted_pairs(
        scoped_idxs.len(),
        group.voted_pairs.iter().filter_map(|(i, j)| {
            let li = global_to_local.get(i).copied()?;
            let lj = global_to_local.get(j).copied()?;
            Some((li, lj))
        }),
    );

    comps_local.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    // Rank every component in one pass over the edge map, and count each
    // component's pairs in one pass over `voted_pairs`. Doing either per
    // component costs O(components x edges), which dominates page render time
    // once a scope fragments into many small clusters.
    let comps_global: Vec<Vec<usize>> = comps_local
        .iter()
        .map(|comp_local| {
            comp_local
                .iter()
                .filter_map(|li| local_to_global.get(*li).copied())
                .collect()
        })
        .collect();

    let mut comp_of_local: Vec<Option<usize>> = vec![None; scoped_idxs.len()];
    for (ci, comp_local) in comps_local.iter().enumerate() {
        for &li in comp_local {
            comp_of_local[li] = Some(ci);
        }
    }
    let mut pair_counts: Vec<usize> = vec![0; comps_local.len()];
    for (i, j) in &group.voted_pairs {
        let (Some(&li), Some(&lj)) = (global_to_local.get(i), global_to_local.get(j)) else {
            continue;
        };
        if let (Some(ci), Some(cj)) = (comp_of_local[li], comp_of_local[lj]) {
            if ci == cj {
                pair_counts[ci] += 1;
            }
        }
    }

    let component_rankings: Vec<ScopedComponent> = rank_partition(group, &comps_global, 10000, 1e-8)
        .into_iter()
        .zip(pair_counts)
        .map(|(ranked, pairs)| ScopedComponent { pairs, ranked })
        .collect();

    let mut unranked_items: Vec<ItemId> = isolate_local_idxs
        .into_iter()
        .filter_map(|li| local_to_global.get(li).copied())
        .filter_map(|idx| group.idx_to_item.get(idx).cloned())
        .chain(
            items_in_scope
                .iter()
                .filter(|it| !group.item_to_idx.contains_key(*it))
                .cloned(),
        )
        .collect();
    unranked_items.sort();

    ChildrenRankings {
        component_rankings,
        unranked_items,
    }
}

/// Build connected-component rankings for direct children of parent_scope.
/// Matches the HTML garden view: multiple components, isolates, no-vote items.
pub fn build_children_rankings(content: &ContentState, parent: &ItemId) -> ChildrenRankings {
    let parent = parent.clone().normalized_storage();
    let items: Vec<ItemId> = content
        .item_children
        .get(&parent)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    build_rankings_for_item_set(content, &items)
}

/// Host-only `https://…` roots for the external garden index (`/-/`).
///
/// Includes every `https://host` ancestor of any [`ItemId::Web`] item that appears in
/// `content.items`, as a parent key in `item_children`, or as a child in `item_children`
/// (so implied “ghost” parents created only via [`ReducerState::add_child_edge`] still show up).
pub fn external_root_host_items(content: &ContentState) -> Vec<ItemId> {
    let mut hosts: HashSet<ItemId> = HashSet::new();

    let mut consider = |id: ItemId| {
        let id = id.normalized_storage();
        if !matches!(&id, ItemId::Web(_)) {
            return;
        }
        let mut cur = id;
        while let Some(p) = cur.parent() {
            cur = p.normalized_storage();
        }
        if matches!(cur, ItemId::Web(_)) {
            hosts.insert(cur);
        }
    };

    for it in &content.items {
        consider(it.clone());
    }
    for parent in content.item_children.keys() {
        consider(parent.clone());
    }
    for set in content.item_children.values() {
        for ch in set {
            consider(ch.clone());
        }
    }

    let mut out: Vec<ItemId> = hosts.into_iter().collect();
    out.sort();
    out
}

pub fn is_pair_voted_in_group(group: &GroupState, a: &ItemId, b: &ItemId) -> bool {
    let Some(&a_idx) = group.item_to_idx.get(a) else {
        return false;
    };
    let Some(&b_idx) = group.item_to_idx.get(b) else {
        return false;
    };
    let (i, j) = if a_idx < b_idx {
        (a_idx, b_idx)
    } else {
        (b_idx, a_idx)
    };
    group.voted_pairs.contains(&(i, j))
}

fn canonical_pair(a: &ItemId, b: &ItemId) -> (ItemId, ItemId) {
    let ac = a.clone().normalized_storage();
    let bc = b.clone().normalized_storage();
    if ac.as_str() <= bc.as_str() {
        (ac, bc)
    } else {
        (bc, ac)
    }
}

pub fn suggest_next_pair_in_pool(
    group: &GroupState,
    pool: &[ItemId],
    current_pair: Option<(&ItemId, &ItemId)>,
) -> Option<(ItemId, ItemId)> {
    let current = current_pair.map(|(a, b)| canonical_pair(a, b));
    let mut pool = pool.to_vec();
    pool.sort();
    pool.dedup();
    if pool.len() < 2 {
        return None;
    }

    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let pair = canonical_pair(&pool[i], &pool[j]);
            if current.as_ref() == Some(&pair) {
                continue;
            }
            if !is_pair_voted_in_group(group, &pool[i], &pool[j]) {
                return Some((pool[i].clone(), pool[j].clone()));
            }
        }
    }

    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let pair = canonical_pair(&pool[i], &pool[j]);
            if current.as_ref() != Some(&pair) {
                return Some((pool[i].clone(), pool[j].clone()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn content_with_children(edges: &[(&str, &[&str])]) -> ContentState {
        let mut item_children: HashMap<ItemId, HashSet<ItemId>> = HashMap::new();
        for (parent, children) in edges {
            let parent = ItemId::parse(parent).unwrap();
            let set: HashSet<ItemId> = children.iter().map(|s| ItemId::parse(s).unwrap()).collect();
            item_children.insert(parent, set);
        }
        ContentState {
            ranking_group: crate::reducer::GroupState::new(),
            items: HashSet::new(),
            item_bodies: HashMap::new(),
            item_children,
            item_votes: HashMap::new(),
            item_snippets: HashMap::new(),
            item_threads: HashMap::new(),
            rank_history: HashMap::new(),
            rank_position_cache: None,
        }
    }

    #[test]
    fn resolve_one_scope_literal() {
        let content = content_with_children(&[(
            "https://slug.social/models",
            &[
                "https://slug.social/models/x",
                "https://slug.social/models/y",
            ],
        )]);
        let out = resolve_one_scope(&content, "models");
        assert_eq!(out.len(), 2);
        assert!(out.contains(&ItemId::parse("https://slug.social/models/x").unwrap()));
        assert!(out.contains(&ItemId::parse("https://slug.social/models/y").unwrap()));
    }

    #[test]
    fn resolve_scope_multiple_parents_merges() {
        let content = content_with_children(&[
            (
                "https://slug.social/a",
                &["https://slug.social/a/1", "https://slug.social/a/2"],
            ),
            (
                "https://slug.social/b",
                &["https://slug.social/b/1", "https://slug.social/b/2"],
            ),
        ]);
        let out = resolve_scope(&content, &["a".into(), "b".into()]);
        assert_eq!(out.len(), 4);
        assert!(out.contains(&ItemId::parse("https://slug.social/a/1").unwrap()));
        assert!(out.contains(&ItemId::parse("https://slug.social/a/2").unwrap()));
        assert!(out.contains(&ItemId::parse("https://slug.social/b/1").unwrap()));
        assert!(out.contains(&ItemId::parse("https://slug.social/b/2").unwrap()));
    }

    #[test]
    fn suggest_next_pair_skips_current_and_voted_pairs() {
        let mut group = crate::reducer::GroupState::new();
        let a = ItemId::parse("~/a").unwrap().normalized_storage();
        let b = ItemId::parse("~/b").unwrap().normalized_storage();
        let c = ItemId::parse("~/c").unwrap().normalized_storage();
        group.apply_vote(crate::reducer::VoteData {
            ts: 1,
            a: a.clone(),
            b: b.clone(),
            ratio_left: 2,
            ratio_right: 1,
            body: "a beats b".to_string(),
            principal: "tester".to_string(),
            delegate: None,
            thread_tag: "vote".to_string(),
        });
        let next =
            suggest_next_pair_in_pool(&group, &[a.clone(), b.clone(), c.clone()], Some((&a, &b)))
                .expect("next pair");
        assert!(next.0 == c || next.1 == c);
        assert_ne!(canonical_pair(&next.0, &next.1), canonical_pair(&a, &b));
    }

    #[test]
    fn external_root_hosts_include_ghost_chain_hosts() {
        use crate::reducer::ContentState;
        let gh = ItemId::parse("https://github.com").unwrap();
        let org = ItemId::parse("https://github.com/org").unwrap();
        let repo = ItemId::parse("https://github.com/org/rep").unwrap();
        let mut item_children: HashMap<ItemId, HashSet<ItemId>> = HashMap::new();
        item_children.entry(gh.clone()).or_default().insert(org.clone());
        item_children.entry(org.clone()).or_default().insert(repo.clone());
        let mut items = HashSet::new();
        items.insert(repo.clone());
        let content = ContentState {
            ranking_group: crate::reducer::GroupState::new(),
            items,
            item_bodies: HashMap::new(),
            item_children,
            item_votes: HashMap::new(),
            item_snippets: HashMap::new(),
            item_threads: HashMap::new(),
            rank_history: HashMap::new(),
            rank_position_cache: None,
        };
        let roots = external_root_host_items(&content);
        assert_eq!(roots, vec![gh]);
    }
}
