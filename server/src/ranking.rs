use std::collections::HashMap;

use crate::reducer::GroupState;

#[derive(Debug, Clone)]
pub struct RankedItem {
    pub item: String,
    pub score: f64,
}

/// Compute rank centrality scores for a GroupState.
/// This matches the approach in `pagerank.rs` but avoids dependencies by doing
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

    // Build adjacency lists from aggregated edges.
    let mut out_edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut out_deg: Vec<f64> = vec![0.0; n];

    for (&(src, dst), &w) in group.edges.iter() {
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
        group.cached_scores = vec![1.0 / n as f64; n];
        group.dirty = false;
        return;
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

    // Normalize (numeric drift can happen)
    let sum: f64 = scores.iter().sum();
    if sum.is_finite() && sum > 0.0 {
        for s in &mut scores {
            *s /= sum;
        }
    }

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


