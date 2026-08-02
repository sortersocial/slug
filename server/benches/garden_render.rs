//! Page-render benchmarks: what a single `GET /~/bench` costs.
//!
//! Garden pages call `scope_rank::build_children_rankings` synchronously on every
//! request with no caching (see AGENTS.md durability matrix — the reducer
//! projection is derived, and scoped rankings are not memoized at all).
//!
//! Cost model for `build_rankings_for_item_set(content, items)` with `S` items in
//! scope, `C` connected components among them, `E` directed edges and `P` voted
//! pairs in the *whole* group:
//!
//!   component split = O(S + P)
//!   per component   = O(E)      — `ranked_items_subset` filters all of `group.edges`
//!                   + O(P)      — the `pairs` count re-scans all of `voted_pairs`
//!                   + O(T*(n_c + e_c))
//!   total           = O(S + C * (E + P) + Σ_c T_c * (n_c + e_c))
//!
//! The `C * (E + P)` term is the problem: it is independent of component size, so
//! a scope of 5000 items split into 2500 pairwise components costs 2500 full scans
//! of the group's edge and pair maps. `component_fanout` isolates that term by
//! holding total item count fixed while varying how it is partitioned.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use slugsocial_server::scope_rank::{build_children_rankings, build_rankings_for_item_set};

mod common;
use common::{bench_parent, build_content, scope_items, Topology};

/// One connected scope of growing size — the "healthy garden" case.
fn single_component_scope(c: &mut Criterion) {
    let mut g = c.benchmark_group("render/single_component");
    g.sample_size(10);

    for n in [256usize, 1024, 4096] {
        let content = build_content(Topology::RandomSparse { degree: 6 }, n);
        let parent = bench_parent();
        g.throughput(Throughput::Elements(n as u64));
        g.bench_with_input(BenchmarkId::from_parameter(n), &content, |b, content| {
            b.iter(|| black_box(build_children_rankings(content, &parent).component_rankings.len()))
        });
    }
    g.finish();
}

/// Fixed 4096 items in scope, partitioned into ever more components.
///
/// Total power-iteration work *falls* as components shrink, so any growth in
/// wall time is pure `C * (E + P)` overhead from the per-component rescans.
fn component_fanout(c: &mut Criterion) {
    let mut g = c.benchmark_group("render/component_fanout");
    g.sample_size(10);

    const TOTAL: usize = 4096;
    for size in [1024usize, 256, 64, 16, 4, 2] {
        let components = TOTAL / size;
        let topo = Topology::ManyComponents { components, size };
        let content = build_content(topo, TOTAL);
        let parent = bench_parent();
        g.throughput(Throughput::Elements(components as u64));
        g.bench_with_input(
            BenchmarkId::new("components", components),
            &content,
            |b, content| {
                b.iter(|| {
                    black_box(build_children_rankings(content, &parent).component_rankings.len())
                })
            },
        );
    }
    g.finish();
}

/// Chain components: worst mixing *and* high component count together. This is
/// the shape a real garden drifts toward — many small, weakly-connected clusters
/// of siblings that were each compared a handful of times.
fn worst_case_scope(c: &mut Criterion) {
    let mut g = c.benchmark_group("render/worst_case");
    g.sample_size(10);

    for (components, size) in [(500usize, 8usize), (2000, 8), (2000, 32)] {
        let topo = Topology::ManyComponents { components, size };
        let content = build_content(topo, components * size);
        let items = scope_items(topo, components * size);
        g.throughput(Throughput::Elements((components * size) as u64));
        g.bench_with_input(
            BenchmarkId::new(format!("{components}x{size}"), components * size),
            &(content, items),
            |b, (content, items)| {
                b.iter(|| {
                    black_box(
                        build_rankings_for_item_set(content, items)
                            .component_rankings
                            .len(),
                    )
                })
            },
        );
    }
    g.finish();
}

criterion_group!(
    benches,
    single_component_scope,
    component_fanout,
    worst_case_scope
);
criterion_main!(benches);
