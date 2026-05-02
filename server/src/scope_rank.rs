//! Shared logic for scoped ranking (connected components under a parent).
//! Used by both the HTML garden view and the API rank endpoint.

use std::collections::{HashMap, HashSet};

use crate::path_types::CanonicalItemUrl;
use crate::ranking::{connected_components_from_voted_pairs, ranked_items_subset, RankedItem};
use crate::reducer::ContentState;

#[derive(Debug, Clone)]
pub struct ScopedComponent {
    pub pairs: usize,
    pub ranked: Vec<RankedItem>,
}

#[derive(Debug, Clone)]
pub struct ChildrenRankings {
    pub component_rankings: Vec<ScopedComponent>,
    /// Items in scope with no rank (no votes connecting them to others in this scope).
    pub unranked_items: Vec<CanonicalItemUrl>,
}

/// Resolve one scope spec (literal path) to direct children of that parent. No wildcards.
fn resolve_one_scope(content: &ContentState, spec: &str) -> HashSet<CanonicalItemUrl> {
    let Some(parent) = CanonicalItemUrl::parse(spec.trim()) else {
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
pub fn resolve_scope(content: &ContentState, specs: &[String]) -> Vec<CanonicalItemUrl> {
    let mut set = HashSet::new();
    for spec in specs {
        set.extend(resolve_one_scope(content, spec));
    }
    let mut out: Vec<CanonicalItemUrl> = set.into_iter().collect();
    out.sort();
    out
}

/// Resolve scope specs recursively up to `depth` levels deep.
/// depth=1 is equivalent to resolve_scope (direct children only).
/// depth=2 includes grandchildren, etc.
pub fn resolve_scope_recursive(content: &ContentState, specs: &[String], depth: usize) -> Vec<CanonicalItemUrl> {
    if depth == 0 {
        return vec![];
    }
    let mut visited: HashSet<CanonicalItemUrl> = HashSet::new();
    let mut frontier: Vec<CanonicalItemUrl> = specs
        .iter()
        .filter_map(|s| CanonicalItemUrl::parse(s))
        .collect();

    for _level in 0..depth {
        let mut next_frontier: Vec<CanonicalItemUrl> = Vec::new();
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

    let mut out: Vec<CanonicalItemUrl> = visited.into_iter().collect();
    out.sort();
    out
}

/// Build connected-component rankings for an explicit set of item paths.
/// Use this when scope comes from multiple parents (resolve_scope).
pub fn build_rankings_for_item_set(content: &ContentState, items_in_scope: &[CanonicalItemUrl]) -> ChildrenRankings {
    let group = &content.ranking_group;
    let mut items_in_scope: Vec<CanonicalItemUrl> = items_in_scope.to_vec();
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

    let mut unranked_items: Vec<CanonicalItemUrl> = isolate_local_idxs
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
pub fn build_children_rankings(content: &ContentState, parent: &CanonicalItemUrl) -> ChildrenRankings {
    let parent = parent.clone().normalized_storage();
    let items: Vec<CanonicalItemUrl> = content
        .item_children
        .get(&parent)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    build_rankings_for_item_set(content, &items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn content_with_children(edges: &[(&str, &[&str])]) -> ContentState {
        let mut item_children: HashMap<CanonicalItemUrl, HashSet<CanonicalItemUrl>> = HashMap::new();
        for (parent, children) in edges {
            let parent = CanonicalItemUrl((*parent).to_string());
            let set: HashSet<CanonicalItemUrl> = children.iter().map(|s| CanonicalItemUrl((*s).to_string())).collect();
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
        }
    }

    #[test]
    fn resolve_one_scope_literal() {
        let content = content_with_children(&[
            ("https://slug.social/models", &["https://slug.social/models/x", "https://slug.social/models/y"]),
        ]);
        let out = resolve_one_scope(&content, "models");
        assert_eq!(out.len(), 2);
        assert!(out.contains(&CanonicalItemUrl("https://slug.social/models/x".to_string())));
        assert!(out.contains(&CanonicalItemUrl("https://slug.social/models/y".to_string())));
    }

    #[test]
    fn resolve_scope_multiple_parents_merges() {
        let content = content_with_children(&[
            ("https://slug.social/a", &["https://slug.social/a/1", "https://slug.social/a/2"]),
            ("https://slug.social/b", &["https://slug.social/b/1", "https://slug.social/b/2"]),
        ]);
        let out = resolve_scope(&content, &["a".into(), "b".into()]);
        assert_eq!(out.len(), 4);
        assert!(out.contains(&CanonicalItemUrl("https://slug.social/a/1".to_string())));
        assert!(out.contains(&CanonicalItemUrl("https://slug.social/a/2".to_string())));
        assert!(out.contains(&CanonicalItemUrl("https://slug.social/b/1".to_string())));
        assert!(out.contains(&CanonicalItemUrl("https://slug.social/b/2".to_string())));
    }
}
