use std::collections::HashMap;

use crate::reducer::GroupState;

#[derive(Debug, Clone)]
pub struct RankedItem {
    pub item: String,
    pub score: f64,
}

/// Compute connected components over the voted-pairs graph (treated as undirected).
///
/// Returns:
/// - `components`: each component is a sorted list of node indices, excluding isolates.
/// - `isolates`: sorted list of node indices with degree 0 (no voted pairs).
pub fn connected_components_from_voted_pairs(
    n: usize,
    voted_pairs: impl Iterator<Item = (usize, usize)>,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, b) in voted_pairs {
        if a >= n || b >= n || a == b {
            continue;
        }
        adj[a].push(b);
        adj[b].push(a);
    }

    let mut isolates: Vec<usize> = (0..n).filter(|&i| adj[i].is_empty()).collect();
    isolates.sort();

    let mut seen = vec![false; n];
    for &i in &isolates {
        seen[i] = true;
    }

    let mut comps: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        if seen[i] {
            continue;
        }
        let mut stack = vec![i];
        seen[i] = true;
        let mut comp: Vec<usize> = Vec::new();
        while let Some(x) = stack.pop() {
            comp.push(x);
            for &y in &adj[x] {
                if !seen[y] {
                    seen[y] = true;
                    stack.push(y);
                }
            }
        }
        comp.sort();
        comps.push(comp);
    }

    (comps, isolates)
}

/// Compute rank centrality scores for a GroupState.
/// This matches the approach in the earlier standalone prototype but avoids dependencies by doing
/// an O(E) multiply per iteration.
pub fn compute_group_ranking(group: &mut GroupState, max_iters: usize, tol: f64) {
    if !group.dirty && !group.cached_scores.is_empty() {
        return;
    }

    let n = group.idx_to_item.len();
    if n == 0 {
        group.cached_scores = vec![];
        group.dirty = false;
        return;
    }
    if n == 1 {
        group.cached_scores = vec![1.0];
        group.dirty = false;
        return;
    }

    let scores = compute_scores_from_edges(
        n,
        group.edges.iter().map(|(&k, &w)| (k, w)),
        max_iters,
        tol,
    );
    group.cached_scores = scores;
    group.dirty = false;
}

pub fn ranked_items(group: &mut GroupState, max_iters: usize, tol: f64) -> Vec<RankedItem> {
    compute_group_ranking(group, max_iters, tol);
    let mut items: Vec<RankedItem> = group
        .idx_to_item
        .iter()
        .enumerate()
        .map(|(i, item)| RankedItem {
            item: item.clone(),
            score: *group.cached_scores.get(i).unwrap_or(&0.0),
        })
        .collect();

    items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    items
}

fn compute_scores_from_edges(n: usize, edges: impl Iterator<Item = ((usize, usize), f64)>, max_iters: usize, tol: f64) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![1.0];
    }

    let mut out_edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut out_deg: Vec<f64> = vec![0.0; n];

    for ((src, dst), w) in edges {
        if src >= n || dst >= n {
            continue;
        }
        if w <= 0.0 {
            continue;
        }
        out_edges[src].push((dst, w));
        out_deg[src] += w;
    }

    let mut max_out = 0.0f64;
    for &d in &out_deg {
        if d > max_out {
            max_out = d;
        }
    }
    if max_out <= 1e-12 {
        return vec![1.0 / n as f64; n];
    }

    let mut scores = vec![1.0 / n as f64; n];
    let mut next = vec![0.0f64; n];

    for _ in 0..max_iters {
        next.fill(0.0);
        for i in 0..n {
            let stay_prob = (max_out - out_deg[i]) / max_out;
            next[i] += scores[i] * stay_prob;

            if out_edges[i].is_empty() {
                continue;
            }
            for &(dst, w) in &out_edges[i] {
                next[dst] += scores[i] * (w / max_out);
            }
        }

        let diff: f64 = scores
            .iter()
            .zip(next.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();

        scores.clone_from_slice(&next);
        if diff < tol {
            break;
        }
    }

    let sum: f64 = scores.iter().sum();
    if sum.is_finite() && sum > 0.0 {
        for s in &mut scores {
            *s /= sum;
        }
    }
    scores
}

/// Rank-centrality within a subset of items (an induced subgraph), using the group's aggregated edges.
///
/// `idxs` are indices into `group.idx_to_item`. The returned items use the original item names.
pub fn ranked_items_subset(group: &GroupState, idxs: &[usize], max_iters: usize, tol: f64) -> Vec<RankedItem> {
    if idxs.is_empty() {
        return vec![];
    }

    // Map original idx -> compact idx [0..m)
    let mut map: HashMap<usize, usize> = HashMap::with_capacity(idxs.len());
    for (j, &i) in idxs.iter().enumerate() {
        map.insert(i, j);
    }

    let edges_iter = group.edges.iter().filter_map(|(&(src, dst), &w)| {
        let s = *map.get(&src)?;
        let d = *map.get(&dst)?;
        Some(((s, d), w))
    });

    let scores = compute_scores_from_edges(idxs.len(), edges_iter, max_iters, tol);

    let mut items: Vec<RankedItem> = idxs
        .iter()
        .enumerate()
        .map(|(j, &orig)| RankedItem {
            item: group.idx_to_item.get(orig).cloned().unwrap_or_default(),
            score: *scores.get(j).unwrap_or(&0.0),
        })
        .collect();

    items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    items
}

pub fn group_summary_scores(
    group: &mut GroupState,
    max_iters: usize,
    tol: f64,
) -> HashMap<String, f64> {
    ranked_items(group, max_iters, tol)
        .into_iter()
        .map(|r| (r.item, r.score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::VoteData;

    fn mk_group() -> GroupState {
        GroupState::new("t".to_string(), "x".to_string())
    }

    fn vote(ts: i64, a: &str, b: &str, l: i32, r: i32) -> VoteData {
        VoteData {
            ts,
            tag: "t".to_string(),
            aspect: "x".to_string(),
            a: a.to_string(),
            b: b.to_string(),
            ratio_left: l,
            ratio_right: r,
            body: "because".to_string(),
            actor: "00000000-0000-0000-0000-000000000000:test:local/test".to_string(),
        }
    }

    #[test]
    fn group_ranking_cache_dirty_flow() {
        let mut g = mk_group();
        assert!(g.dirty);

        g.apply_vote(vote(1, "a", "b", 3, 1));
        assert!(g.dirty);
        assert!(g.cached_scores.is_empty());

        compute_group_ranking(&mut g, 10000, 1e-8);
        assert!(!g.dirty);
        assert_eq!(g.cached_scores.len(), g.idx_to_item.len());

        // Recomputing when not dirty should be a no-op.
        let before = g.cached_scores.clone();
        compute_group_ranking(&mut g, 10000, 1e-8);
        assert_eq!(before, g.cached_scores);
    }

    #[test]
    fn connected_components_split_disconnected_pairs() {
        let mut g = mk_group();
        // Two disconnected edges: (a,b) and (c,d)
        g.apply_vote(vote(1, "a", "b", 3, 1));
        g.apply_vote(vote(2, "c", "d", 3, 1));

        let n = g.idx_to_item.len();
        let (mut comps, isolates) =
            connected_components_from_voted_pairs(n, g.voted_pairs.iter().copied());
        assert!(isolates.is_empty());
        // Order-independent: sort components by their item names for stable assert.
        comps.sort_by_key(|c| {
            c.iter()
                .map(|&i| g.idx_to_item[i].clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(comps.len(), 2);
        let comp0 = comps[0]
            .iter()
            .map(|&i| g.idx_to_item[i].as_str())
            .collect::<Vec<_>>();
        let comp1 = comps[1]
            .iter()
            .map(|&i| g.idx_to_item[i].as_str())
            .collect::<Vec<_>>();
        assert_eq!(comp0, vec!["a", "b"]);
        assert_eq!(comp1, vec!["c", "d"]);
    }

    #[test]
    fn subset_ranking_ranks_within_component_only() {
        let mut g = mk_group();
        g.apply_vote(vote(1, "a", "b", 3, 1)); // a > b
        g.apply_vote(vote(2, "c", "d", 1, 4)); // d > c

        let (comps, _) =
            connected_components_from_voted_pairs(g.idx_to_item.len(), g.voted_pairs.iter().copied());
        assert_eq!(comps.len(), 2);

        // Rank each component and ensure winner is first within that component.
        for comp in comps {
            let ranked = ranked_items_subset(&g, &comp, 10000, 1e-8);
            assert_eq!(ranked.len(), 2);
            let names = ranked.iter().map(|r| r.item.as_str()).collect::<Vec<_>>();
            if names.contains(&"a") {
                assert_eq!(names[0], "a");
            } else {
                assert_eq!(names[0], "d");
            }
        }
    }
}


