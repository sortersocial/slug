use std::collections::{HashMap, HashSet};

use crate::path_types::ItemId;
use crate::reducer::GroupState;
use crate::stationary::{self, RankChain, SolveOptions, Solution};

#[derive(Debug, Clone)]
pub struct RankedItem {
    pub item: ItemId,
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

/// Build the Rank Centrality Markov chain from raw directed vote weights.
///
/// Rank Centrality (Negahban, Oh, Shah 2012, §3.1):
///   P_ij = (1/d_max) * a_ij           for i ≠ j compared
///   P_ii = 1 - (1/d_max) * Σ_k a_ik
/// where d_i is the *degree* (number of distinct neighbors compared) and
/// d_max = max_i d_i. Using the unweighted degree — not the sum of
/// pairwise-normalized weights — is what guarantees aperiodicity: it
/// forces P_ii > 0 for every non-maximum-degree node, and for max-degree
/// nodes whenever any neighbor weight is below 1 (i.e. not a unanimous
/// loss). Without this, regular comparison graphs (e.g. a pure star at
/// ratio 2:1) produce a bipartite chain that oscillates instead of
/// converging — see issue #146. (Direct solvers are immune either way:
/// π depends only on the off-diagonals, which d_max scales uniformly.)
pub fn chain_from_edges(n: usize, edges: impl Iterator<Item = ((usize, usize), f64)>) -> RankChain {
    // Collect raw edges into a map for pairwise normalization.
    let mut raw: HashMap<(usize, usize), f64> = HashMap::new();
    for ((src, dst), w) in edges {
        if src >= n || dst >= n || w <= 0.0 {
            continue;
        }
        *raw.entry((src, dst)).or_insert(0.0) += w;
    }

    // Pairwise normalization: a_ij = A_ij / (A_ij + A_ji).
    // This ensures repeated votes on the same pair don't inflate influence
    // beyond what the ratio implies.
    //
    // Sorted, not read straight off the `HashMap`: the summation order of every
    // downstream reduction has to be a function of the graph alone, or scores
    // wobble in their low bits between runs and near-ties can flip.
    let mut keys: Vec<(usize, usize)> = raw.keys().copied().collect();
    keys.sort_unstable();

    let mut normalized: Vec<((usize, usize), f64)> = Vec::with_capacity(keys.len());
    let mut neighbors: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for (i, j) in keys {
        let w_ij = *raw.get(&(i, j)).unwrap_or(&0.0);
        let w_ji = *raw.get(&(j, i)).unwrap_or(&0.0);
        let total = w_ij + w_ji;
        if total <= 0.0 {
            continue;
        }
        normalized.push(((i, j), w_ij / total));
        neighbors[i].insert(j);
        neighbors[j].insert(i);
    }

    let d_max = neighbors.iter().map(|s| s.len()).max().unwrap_or(0);
    RankChain::from_normalized(n, normalized, d_max)
}

/// Stationary distribution for the chain induced by `edges`, with convergence
/// diagnostics attached. See [`stationary::Solution`].
///
/// This is the API that replaces "return whatever the iteration reached":
/// callers that care can inspect `converged`, `residual` and `method`, and
/// non-convergence is logged rather than swallowed.
pub fn solve_scores_from_edges(
    n: usize,
    edges: impl Iterator<Item = ((usize, usize), f64)>,
    max_iters: usize,
    tol: f64,
) -> Solution {
    if n == 0 {
        return stationary::trivial(vec![]);
    }
    if n == 1 {
        return stationary::trivial(vec![1.0]);
    }

    let chain = chain_from_edges(n, edges);
    let solution = stationary::solve(
        &chain,
        SolveOptions {
            tol,
            max_iters,
            ..SolveOptions::default()
        },
    );

    if !solution.converged {
        tracing::warn!(
            n,
            method = solution.method.label(),
            iterations = solution.iterations,
            residual = solution.residual,
            "rank centrality did not reach tolerance; this ranking may be misordered"
        );
    } else if solution.underflowed {
        tracing::debug!(
            n,
            method = solution.method.label(),
            "rank centrality scores span more than f64 holds; use log scores to order the tail"
        );
    }
    solution
}

fn compute_scores_from_edges(n: usize, edges: impl Iterator<Item = ((usize, usize), f64)>, max_iters: usize, tol: f64) -> Vec<f64> {
    solve_scores_from_edges(n, edges, max_iters, tol).pi
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

    let solved = solve_scores_from_edges(idxs.len(), edges_iter, max_iters, tol);

    // Filter out entries where idx_to_item doesn't have the slot (shouldn't happen, but be safe).
    // Sorting on the *log* score, not `score`: on a long preference chain the
    // true distribution spans more decades than f64 holds, so `score` ties off
    // at the bottom while the log scores still order it correctly.
    let mut items: Vec<(RankedItem, f64)> = idxs
        .iter()
        .enumerate()
        .filter_map(|(j, &orig)| {
            let item = group.idx_to_item.get(orig)?.clone();
            let score = *solved.pi.get(j).unwrap_or(&0.0);
            let log_score = *solved.log_pi.get(j).unwrap_or(&f64::NEG_INFINITY);
            Some((RankedItem { item, score }, log_score))
        })
        .collect();

    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    items.into_iter().map(|(item, _)| item).collect()
}

/// Rank several disjoint node groups with a **single** pass over the edge map.
///
/// `groups[k]` holds indices into `group.idx_to_item`. Edges are bucketed by
/// group in one O(E) scan, so the total cost is
/// `O(N + E + Σ_k T_k·(n_k + e_k))` instead of the `O(K·E)` you get from calling
/// [`ranked_items_subset`] once per group — the filter inside that function walks
/// the whole edge map regardless of how few nodes it was asked about.
///
/// Groups are expected to be disjoint; if a node appears in several, the last
/// group claiming it wins. Edges crossing groups are dropped, which matches the
/// induced-subgraph semantics of [`ranked_items_subset`], so ranking each
/// connected component through either path gives identical results.
pub fn rank_partition(
    group: &GroupState,
    groups: &[Vec<usize>],
    max_iters: usize,
    tol: f64,
) -> Vec<Vec<RankedItem>> {
    const UNASSIGNED: u32 = u32::MAX;

    let n = group.idx_to_item.len();
    let mut slot_group: Vec<u32> = vec![UNASSIGNED; n];
    let mut slot_local: Vec<u32> = vec![UNASSIGNED; n];
    for (gi, nodes) in groups.iter().enumerate() {
        for (li, &node) in nodes.iter().enumerate() {
            if node < n {
                slot_group[node] = gi as u32;
                slot_local[node] = li as u32;
            }
        }
    }

    let mut buckets: Vec<Vec<((usize, usize), f64)>> = vec![Vec::new(); groups.len()];
    for (&(src, dst), &w) in &group.edges {
        if src >= n || dst >= n {
            continue;
        }
        let gi = slot_group[src];
        if gi == UNASSIGNED || gi != slot_group[dst] {
            continue;
        }
        buckets[gi as usize].push((
            (slot_local[src] as usize, slot_local[dst] as usize),
            w,
        ));
    }

    groups
        .iter()
        .zip(buckets)
        .map(|(nodes, edges)| {
            let solved =
                solve_scores_from_edges(nodes.len(), edges.into_iter(), max_iters, tol);
            // Sort on log scores (same reason as `ranked_items_subset`): deep
            // preference chains underflow f64 mass while log-space still orders.
            let mut items: Vec<(RankedItem, f64)> = nodes
                .iter()
                .enumerate()
                .filter_map(|(local, &global)| {
                    let item = group.idx_to_item.get(global)?.clone();
                    let score = *solved.pi.get(local).unwrap_or(&0.0);
                    let log_score = *solved.log_pi.get(local).unwrap_or(&f64::NEG_INFINITY);
                    Some((RankedItem { item, score }, log_score))
                })
                .collect();
            items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            items.into_iter().map(|(item, _)| item).collect()
        })
        .collect()
}

pub fn group_summary_scores(
    group: &mut GroupState,
    max_iters: usize,
    tol: f64,
) -> HashMap<ItemId, f64> {
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
        GroupState::new()
    }

    fn vote(ts: i64, a: &str, b: &str, l: i32, r: i32) -> VoteData {
        use crate::path_types::ItemId;
        VoteData {
            ts,
            a: ItemId::parse(a).unwrap(),
            b: ItemId::parse(b).unwrap(),
            ratio_left: l,
            ratio_right: r,
            body: "because".to_string(),
            principal: "test".to_string(),
            delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
            thread_tag: "untagged".to_string(),
        }
    }

    /// Regression for issue #146: pure forward star at default `>` ratio (2:1).
    /// Under the old (sum-of-weights) divisor every node had P_ii = 0 and the
    /// chain was bipartite; power iteration oscillated and returned the
    /// uniform initial distribution after an even number of steps. Using the
    /// paper's degree-based d_max gives every node a positive self-loop and
    /// the chain converges to the correct stationary distribution.
    #[test]
    fn star_topology_winner_at_top_via_subset() {
        let mut g = mk_group();
        g.apply_vote(vote(1, "zebra", "alpha", 2, 1));
        g.apply_vote(vote(2, "zebra", "beta", 2, 1));

        let mut items: Vec<(usize, String)> = g
            .idx_to_item
            .iter()
            .enumerate()
            .map(|(i, it)| (i, it.as_str().to_string()))
            .collect();
        items.sort_by(|a, b| a.1.cmp(&b.1));
        let idxs: Vec<usize> = items.iter().map(|(i, _)| *i).collect();

        let ranked = ranked_items_subset(&g, &idxs, 10000, 1e-8);
        for r in &ranked {
            eprintln!("{}: {}", r.item.as_str(), r.score);
        }
        assert_eq!(
            ranked[0].item.as_str(),
            "https://slug.social/zebra",
            "zebra won both votes and should rank #1"
        );
    }

    /// A long preference chain — the natural shape of a deep ontology or a
    /// hand-ordered list — is where power iteration got the answer wrong. With
    /// varied ratios and 1200 items the old solver reported success (its L1
    /// step fell below 1e-8 after 4325 sweeps, well inside the cap) while
    /// leaving 47% of items at the wrong rank, because an absolute step
    /// tolerance says nothing about entries that are themselves 1e-300.
    #[test]
    fn deep_chain_is_ranked_in_exactly_the_right_order() {
        let n = 1200usize;
        let mut g = mk_group();
        let name = |i: usize| format!("i{i:05}");
        for i in 0..n - 1 {
            let left = 2 + (i % 5) as i32;
            g.apply_vote(vote(i as i64, &name(i), &name(i + 1), left, 1));
        }

        let idxs: Vec<usize> = (0..g.idx_to_item.len()).collect();
        let ranked = ranked_items_subset(&g, &idxs, 10000, 1e-8);
        assert_eq!(ranked.len(), n);
        for (i, r) in ranked.iter().enumerate() {
            let want = format!("https://slug.social/{}", name(i));
            assert_eq!(r.item.as_str(), want, "position {i} of the chain ranking");
        }
    }

    /// Same graph, edges presented in a different order: the scores must come
    /// back bit-identical, not merely close.
    #[test]
    fn ranking_does_not_depend_on_edge_iteration_order() {
        let n = 200usize;
        let mut edges: Vec<((usize, usize), f64)> = Vec::new();
        for i in 0..n - 1 {
            edges.push(((i + 1, i), 3.0));
            edges.push(((i, i + 1), 1.0));
        }
        for i in 0..n / 4 {
            let j = (5 * i + 7) % n;
            if i != j {
                edges.push(((j, i), 2.0));
                edges.push(((i, j), 1.0));
            }
        }

        let reference = compute_scores_from_edges(n, edges.iter().copied(), 10000, 1e-8);
        let mut state = 0x5EEDu64;
        for _ in 0..5 {
            let mut shuffled = edges.clone();
            for k in (1..shuffled.len()).rev() {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let m = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) % (k as u64 + 1)) as usize;
                shuffled.swap(k, m);
            }
            let got = compute_scores_from_edges(n, shuffled.into_iter(), 10000, 1e-8);
            assert_eq!(got, reference);
        }
    }

    /// Convergence is reported, never assumed.
    #[test]
    fn solver_reports_convergence_for_every_shape() {
        for n in [2usize, 3, 64, 900] {
            let mut edges: Vec<((usize, usize), f64)> = Vec::new();
            for i in 0..n - 1 {
                edges.push(((i + 1, i), 2.0));
                edges.push(((i, i + 1), 1.0));
            }
            let solved = solve_scores_from_edges(n, edges.into_iter(), 10000, 1e-8);
            assert!(solved.converged, "chain n={n} reported non-convergence");
            assert!(solved.residual < 1e-10, "chain n={n}: {:e}", solved.residual);
            assert_eq!(solved.pi.len(), n);
            assert_eq!(solved.log_pi.len(), n);
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
        assert_eq!(comp0, vec!["https://slug.social/a", "https://slug.social/b"]);
        assert_eq!(comp1, vec!["https://slug.social/c", "https://slug.social/d"]);
    }

    /// `rank_partition` exists purely to avoid the O(components x edges) cost of
    /// calling `ranked_items_subset` in a loop, so the two must stay identical.
    #[test]
    fn rank_partition_matches_per_component_subset_ranking() {
        let mut g = mk_group();
        // Three components of different shapes: a chain, a star, and a lone pair.
        g.apply_vote(vote(1, "a", "b", 3, 1));
        g.apply_vote(vote(2, "b", "c", 2, 1));
        g.apply_vote(vote(3, "c", "d", 5, 2));
        g.apply_vote(vote(4, "hub", "s1", 2, 1));
        g.apply_vote(vote(5, "hub", "s2", 4, 1));
        g.apply_vote(vote(6, "hub", "s3", 1, 3));
        g.apply_vote(vote(7, "x", "y", 7, 2));

        let (comps, _) = connected_components_from_voted_pairs(
            g.idx_to_item.len(),
            g.voted_pairs.iter().copied(),
        );
        assert_eq!(comps.len(), 3);

        let batched = rank_partition(&g, &comps, 10000, 1e-8);
        assert_eq!(batched.len(), comps.len());
        for (comp, got) in comps.iter().zip(&batched) {
            let want = ranked_items_subset(&g, comp, 10000, 1e-8);
            assert_eq!(got.len(), want.len());
            for (a, b) in got.iter().zip(&want) {
                assert_eq!(a.item, b.item, "ordering diverged for component {comp:?}");
                assert!(
                    (a.score - b.score).abs() < 1e-12,
                    "score diverged for {}: {} vs {}",
                    a.item.as_str(),
                    a.score,
                    b.score
                );
            }
        }
    }

    /// Edges leaving a group are dropped, so a partition that splits a connected
    /// component ranks each piece on its induced subgraph only.
    #[test]
    fn rank_partition_drops_cross_group_edges() {
        let mut g = mk_group();
        g.apply_vote(vote(1, "a", "b", 3, 1));
        g.apply_vote(vote(2, "b", "c", 3, 1));

        let idx = |s: &str| g.item_to_idx[&ItemId::parse(s).unwrap()];
        let groups = vec![vec![idx("a"), idx("b")], vec![idx("c")]];
        let out = rank_partition(&g, &groups, 10000, 1e-8);

        assert_eq!(out[0].len(), 2);
        assert_eq!(out[0][0].item.as_str(), "https://slug.social/a");
        // A lone node carries the whole mass of its own subgraph.
        assert_eq!(out[1].len(), 1);
        assert!((out[1][0].score - 1.0).abs() < 1e-12);
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
            if names.contains(&"https://slug.social/a") {
                assert_eq!(names[0], "https://slug.social/a");
            } else {
                assert_eq!(names[0], "https://slug.social/d");
            }
        }
    }
}


