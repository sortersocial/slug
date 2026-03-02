//! Shared logic for scoped ranking (connected components under a parent).
//! Used by both the HTML garden view and the API rank endpoint.

use std::collections::HashMap;

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

/// Build connected-component rankings for direct children of parent_scope.
/// Matches the HTML garden view: multiple components, isolates, no-vote items.
pub fn build_children_rankings(reduced: &ReducerState, parent_scope: &str) -> ChildrenRankings {
    let group = &reduced.ranking_group;
    let mut items_in_scope: Vec<String> = reduced
        .item_children
        .get(parent_scope)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
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
            let comp_set: std::collections::HashSet<usize> = comp_global.iter().copied().collect();
            let ranked = ranked_items_subset(group, &comp_global, 10000, 1e-8);
            let pairs = group
                .voted_pairs
                .iter()
                .filter(|(i, j)| comp_set.contains(i) && comp_set.contains(j))
                .count();
            ScopedComponent { pairs, ranked }
        })
        .collect();

    // Unranked in this scope: isolates (in graph but no in-scope edges) + never voted
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
