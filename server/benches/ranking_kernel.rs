//! Kernel-level benchmarks for Rank Centrality power iteration.
//!
//! Cost model for `ranking::compute_scores_from_edges(n, edges, K, tol)`:
//!
//!   setup  = O(E)                     — raw collect, pairwise normalize, adjacency build
//!   iterate= O(T * (n + E))           — T = iterations actually run, capped at K = 10_000
//!   total  = O(E + T * (n + E))
//!
//! `T` is governed by the spectral gap of the Markov chain, not by `n` directly.
//! For a path graph the gap is Θ(1/n²), so T ≈ Θ(n² · log(1/tol)) and the 10k cap
//! binds at surprisingly small n. For a star or clique the gap is Θ(1) and T is
//! a few dozen. `topology_convergence` is the group that shows this split.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use slugsocial_server::ranking::{
    compute_group_ranking, connected_components_from_voted_pairs, ranked_items_subset,
};

mod common;
use common::{build_group, Topology};

const MAX_ITERS: usize = 10_000;
const TOL: f64 = 1e-8;

/// How the graph shape (not just its size) drives iteration count.
fn topology_convergence(c: &mut Criterion) {
    let mut g = c.benchmark_group("kernel/topology");
    g.sample_size(20);

    for n in [64usize, 256, 1024] {
        for topo in [
            Topology::Chain,
            Topology::Star,
            Topology::RandomSparse { degree: 6 },
        ] {
            let base = build_group(topo, n);
            g.throughput(Throughput::Elements(n as u64));
            g.bench_with_input(
                BenchmarkId::new(topo.label(), n),
                &base,
                |b, base| {
                    b.iter_batched_ref(
                        || base.clone(),
                        |group| {
                            group.dirty = true;
                            compute_group_ranking(group, MAX_ITERS, TOL);
                            black_box(group.cached_scores.len())
                        },
                        criterion::BatchSize::SmallInput,
                    )
                },
            );
        }
    }
    g.finish();
}

/// Dense graphs: E grows as n², so per-iteration cost dominates.
fn dense_graphs(c: &mut Criterion) {
    let mut g = c.benchmark_group("kernel/clique");
    g.sample_size(10);

    for n in [64usize, 128, 256] {
        let base = build_group(Topology::Clique, n);
        let edges = base.edges.len() as u64;
        g.throughput(Throughput::Elements(edges));
        g.bench_with_input(BenchmarkId::from_parameter(n), &base, |b, base| {
            b.iter_batched_ref(
                || base.clone(),
                |group| {
                    group.dirty = true;
                    compute_group_ranking(group, MAX_ITERS, TOL);
                    black_box(group.cached_scores.len())
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }
    g.finish();
}

/// `ranked_items_subset` filters the *whole* `group.edges` map on every call, so
/// ranking a 2-node component inside a 100k-edge group still pays O(E).
///
/// The two arms below rank the same tiny subset out of groups of growing size.
/// Flat-ish per-element cost here would mean the filter is cheap; it is not.
fn subset_pays_full_edge_scan(c: &mut Criterion) {
    let mut g = c.benchmark_group("kernel/subset_edge_scan");
    g.sample_size(20);

    for n in [1024usize, 4096, 16384] {
        let group = build_group(Topology::RandomSparse { degree: 6 }, n);
        let tiny: Vec<usize> = vec![0, 1];
        g.throughput(Throughput::Elements(group.edges.len() as u64));
        g.bench_with_input(
            BenchmarkId::new("rank_2_of_n", n),
            &(group, tiny),
            |b, (group, idxs)| {
                b.iter(|| black_box(ranked_items_subset(group, idxs, MAX_ITERS, TOL).len()))
            },
        );
    }
    g.finish();
}

/// Undirected BFS over voted pairs: O(n + P). This one is honest.
fn connected_components(c: &mut Criterion) {
    let mut g = c.benchmark_group("kernel/connected_components");
    g.sample_size(50);

    for n in [1024usize, 16384, 131072] {
        let group = build_group(Topology::RandomSparse { degree: 6 }, n);
        g.throughput(Throughput::Elements(group.voted_pairs.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(n), &group, |b, group| {
            b.iter(|| {
                let (comps, isolates) = connected_components_from_voted_pairs(
                    group.idx_to_item.len(),
                    group.voted_pairs.iter().copied(),
                );
                black_box(comps.len() + isolates.len())
            })
        });
    }
    g.finish();
}

/// The `dirty` flag is the only cache in the system. Confirms a warm read is free
/// and quantifies exactly what a single spurious invalidation costs.
fn cache_hit_vs_miss(c: &mut Criterion) {
    let mut g = c.benchmark_group("kernel/cache");
    g.sample_size(20);

    let base = build_group(Topology::RandomSparse { degree: 6 }, 4096);

    let mut warm = base.clone();
    compute_group_ranking(&mut warm, MAX_ITERS, TOL);
    g.bench_function("hit", |b| {
        b.iter(|| {
            compute_group_ranking(black_box(&mut warm), MAX_ITERS, TOL);
            black_box(warm.cached_scores.len())
        })
    });

    g.bench_function("miss", |b| {
        b.iter_batched_ref(
            || base.clone(),
            |group| {
                group.dirty = true;
                compute_group_ranking(group, MAX_ITERS, TOL);
                black_box(group.cached_scores.len())
            },
            criterion::BatchSize::SmallInput,
        )
    });
    g.finish();
}

criterion_group!(
    benches,
    topology_convergence,
    dense_graphs,
    subset_pays_full_edge_scan,
    connected_components,
    cache_hit_vs_miss
);
criterion_main!(benches);
