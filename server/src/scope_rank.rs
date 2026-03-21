//! Shared logic for scoped ranking (connected components under a parent).
//! Used by both the HTML garden view and the API rank endpoint.

use std::collections::{HashMap, HashSet};

use crate::events::canonicalize_item;
use crate::ranking::{connected_components_from_voted_pairs, ranked_items_subset, RankedItem};
use crate::reducer::ReducerState;

#[derive(Debug, Clone)]
pub struct ScopedComponent {
    pub pairs: usize,
    pub ranked: Vec<RankedItem>,
}

#[derive(Debug, Clone)]
pub struct ChildrenRankings {
    pub component_rankings: Vec<ScopedComponent>,
    /// Items in scope with no rank (no votes connecting them to others in this scope).
    pub unranked_items: Vec<String>,
}

/// Resolve one scope spec (literal path) to direct children of that parent. No wildcards.
fn resolve_one_scope(reduced: &ReducerState, spec: &str) -> HashSet<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return HashSet::new();
    }
    let parent = canonicalize_item(spec);
    reduced
        .item_children
        .get(&parent)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Resolve multiple scope specs (literal paths) to a single merged, deduplicated list of item paths.
/// Used for rank/pair with multiple parents (explicit merge, e.g. rank ~/models ~/ai-models).
pub fn resolve_scope(reduced: &ReducerState, specs: &[String]) -> Vec<String> {
    let mut set = HashSet::new();
    for spec in specs {
        set.extend(resolve_one_scope(reduced, spec));
    }
    let mut out: Vec<String> = set.into_iter().collect();
    out.sort();
    out
}

/// Resolve scope specs recursively up to `depth` levels deep.
/// depth=1 is equivalent to resolve_scope (direct children only).
/// depth=2 includes grandchildren, etc.
pub fn resolve_scope_recursive(reduced: &ReducerState, specs: &[String], depth: usize) -> Vec<String> {
    if depth == 0 {
        return vec![];
    }
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = specs.iter().map(|s| canonicalize_item(s)).collect();

    for _level in 0..depth {
        let mut next_frontier: Vec<String> = Vec::new();
        for parent in &frontier {
            if let Some(children) = reduced.item_children.get(parent) {
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

    let mut out: Vec<String> = visited.into_iter().collect();
    out.sort();
    out
}

/// Build connected-component rankings for an explicit set of item paths.
/// Use this when scope comes from multiple parents (resolve_scope).
pub fn build_rankings_for_item_set(reduced: &ReducerState, items_in_scope: &[String]) -> ChildrenRankings {
    let group = &reduced.ranking_group;
    let mut items_in_scope: Vec<String> = items_in_scope.to_vec();
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

    let component_rankings: Vec<ScopedComponent> = comps_local
        .iter()
        .map(|comp_local| {
            let comp_global: Vec<usize> = comp_local
                .iter()
                .filter_map(|li| local_to_global.get(*li).copied())
                .collect();
            let comp_set: HashSet<usize> = comp_global.iter().copied().collect();
            let ranked = ranked_items_subset(group, &comp_global, 10000, 1e-8);
            let pairs = group
                .voted_pairs
                .iter()
                .filter(|(i, j)| comp_set.contains(i) && comp_set.contains(j))
                .count();
            ScopedComponent { pairs, ranked }
        })
        .collect();

    let mut unranked_items: Vec<String> = isolate_local_idxs
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
pub fn build_children_rankings(reduced: &ReducerState, parent_scope: &str) -> ChildrenRankings {
    let items: Vec<String> = reduced
        .item_children
        .get(parent_scope)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    build_rankings_for_item_set(reduced, &items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn reduced_with_children(edges: &[(&str, &[&str])]) -> ReducerState {
        let mut item_children: HashMap<String, HashSet<String>> = HashMap::new();
        for (parent, children) in edges {
            let parent = (*parent).to_string();
            let set: HashSet<String> = children.iter().map(|s| (*s).to_string()).collect();
            item_children.insert(parent, set);
        }
        ReducerState {
            ranking_group: crate::reducer::GroupState::new(),
            items: HashSet::new(),
            item_bodies: HashMap::new(),
            item_children,
            ..ReducerState::default()
        }
    }

    #[test]
    fn resolve_one_scope_literal() {
        let reduced = reduced_with_children(&[
            ("", &["a", "b"]),
            ("models", &["models/x", "models/y"]),
        ]);
        let out = resolve_one_scope(&reduced, "models");
        assert_eq!(out.len(), 2);
        assert!(out.contains("models/x"));
        assert!(out.contains("models/y"));
    }

    #[test]
    fn resolve_scope_multiple_parents_merges() {
        let reduced = reduced_with_children(&[
            ("a", &["a/1", "a/2"]),
            ("b", &["b/1", "b/2"]),
        ]);
        let out = resolve_scope(&reduced, &["a".into(), "b".into()]);
        assert_eq!(out.len(), 4);
        assert!(out.contains(&"a/1".to_string()));
        assert!(out.contains(&"a/2".to_string()));
        assert!(out.contains(&"b/1".to_string()));
        assert!(out.contains(&"b/2".to_string()));
    }
}
