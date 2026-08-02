//! Head-to-head cost of the stationary-distribution solvers behind Rank Centrality.
//!
//! Deliberately small: a couple of representative sizes per topology, ten
//! samples each, ~1 s of measurement per point. The point is the *ordering*
//! between solvers, which is stable at this resolution; for a finer sweep plus
//! accuracy numbers use `cargo run --release -p slugsocial-server --example
//! solver_probe`, which finishes in seconds.
//!
//! What each topology is here to show:
//!
//! - **chain** — spectral gap `Θ(1/n²)`. Power iteration needs `Θ(n²)` sweeps
//!   and blows the 10 000 cap past n ≈ 1500; sparse GTH is `O(n)` because a
//!   tree eliminates with zero fill.
//! - **star** — uniformization by `d_max = n-1` makes the chain almost purely
//!   lazy, so power iteration crawls (it fails the cap at n = 1024) while
//!   Gauss–Seidel finishes in two sweeps.
//! - **clique / sparse** — well-conditioned. Iterative wins outright and the
//!   direct methods are the ones paying.

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use slugsocial_server::ranking::chain_from_edges;
use slugsocial_server::stationary::{
    bicgstab, dense_gth, dense_lu, power, power_aitken, solve, sor, sparse_gth, RankChain,
    SolveOptions, SparseGthOutcome,
};

mod common;
use common::{build_group, Topology};

const TOL: f64 = 1e-8;
const CAP: usize = 10_000;

fn opts() -> SolveOptions {
    SolveOptions {
        tol: TOL,
        max_iters: CAP,
        ..SolveOptions::default()
    }
}

fn chain_of(topo: Topology, n: usize) -> RankChain {
    let g = build_group(topo, n);
    chain_from_edges(g.idx_to_item.len(), g.edges.iter().map(|(&k, &w)| (k, w)))
}

fn quick<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(10)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(1));
    g
}

/// Every solver on the same input, per topology.
fn solver_shootout(c: &mut Criterion) {
    let cases: Vec<(Topology, usize, bool)> = vec![
        // (topology, n, include the O(n³) dense arms)
        (Topology::Chain, 256, true),
        (Topology::Chain, 1024, true),
        (Topology::Star, 1024, true),
        (Topology::Clique, 256, true),
        (Topology::RandomSparse { degree: 6 }, 1024, false),
    ];

    for (topo, n, dense) in cases {
        let chain = chain_of(topo, n);
        let mut g = quick(c, &format!("solvers/{}", topo.label()));

        g.bench_with_input(BenchmarkId::new("power", n), &chain, |b, ch| {
            b.iter(|| black_box(power(ch, opts()).residual))
        });
        g.bench_with_input(BenchmarkId::new("power+aitken", n), &chain, |b, ch| {
            b.iter(|| black_box(power_aitken(ch, opts()).residual))
        });
        g.bench_with_input(BenchmarkId::new("sor", n), &chain, |b, ch| {
            b.iter(|| black_box(sor(ch, opts(), 1.0).residual))
        });
        g.bench_with_input(BenchmarkId::new("bicgstab", n), &chain, |b, ch| {
            b.iter(|| black_box(bicgstab(ch, opts()).residual))
        });
        g.bench_with_input(BenchmarkId::new("sparse-gth", n), &chain, |b, ch| {
            let unlimited = SolveOptions {
                direct_work_budget: u64::MAX,
                dense_core_max: usize::MAX,
                ..opts()
            };
            b.iter(|| match sparse_gth(ch, unlimited, TOL) {
                SparseGthOutcome::Solved(s) => black_box(s.residual),
                SparseGthOutcome::TooDense { .. } => unreachable!(),
            })
        });
        if dense {
            g.bench_with_input(BenchmarkId::new("dense-gth", n), &chain, |b, ch| {
                b.iter(|| black_box(dense_gth(ch, TOL).residual))
            });
            g.bench_with_input(BenchmarkId::new("dense-lu", n), &chain, |b, ch| {
                b.iter(|| black_box(dense_lu(ch, TOL).residual))
            });
        }
        g.finish();
    }
}

/// The shipped path, including the cost of deciding which solver to use.
fn hybrid_dispatch(c: &mut Criterion) {
    let cases: Vec<(String, Topology, usize)> = vec![
        ("chain".into(), Topology::Chain, 4000),
        ("star".into(), Topology::Star, 4000),
        ("clique".into(), Topology::Clique, 400),
        (
            "sparse-d6".into(),
            Topology::RandomSparse { degree: 6 },
            4000,
        ),
        (
            "components".into(),
            Topology::ManyComponents {
                components: 64,
                size: 64,
            },
            0,
        ),
    ];
    let mut g = quick(c, "hybrid");
    for (label, topo, n) in cases {
        let chain = chain_of(topo, n);
        g.bench_with_input(BenchmarkId::new(label, chain.n), &chain, |b, ch| {
            b.iter(|| black_box(solve(ch, opts()).residual))
        });
    }
    g.finish();
}

/// Building the chain (aggregate, pairwise-normalize, sort) versus solving it.
/// Sorting is what buys run-to-run determinism; this is where to see its price.
fn chain_construction(c: &mut Criterion) {
    let mut g = quick(c, "chain_from_edges");
    for (topo, n) in [
        (Topology::RandomSparse { degree: 6 }, 4000usize),
        (Topology::Clique, 400),
    ] {
        let group = build_group(topo, n);
        let total = group.idx_to_item.len();
        g.bench_with_input(
            BenchmarkId::new(topo.label(), group.edges.len()),
            &group,
            |b, grp| {
                b.iter(|| {
                    let ch = chain_from_edges(total, grp.edges.iter().map(|(&k, &w)| (k, w)));
                    black_box(ch.nnz())
                })
            },
        );
    }
    g.finish();
}

criterion_group!(
    benches,
    solver_shootout,
    hybrid_dispatch,
    chain_construction
);
criterion_main!(benches);
