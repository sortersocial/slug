//! Write-path and boot-replay benchmarks — the slowest thing the server does.
//!
//! `ReducerState::apply_ingest_to_content` recomputes rankings twice per post
//! (once before applying votes to capture "before" ranks for rank history, once
//! after) and, for each of the `U` distinct items mentioned in a vote, calls both
//! `scope_rank_of` and `global_rank_of` on each side of that pair.
//!
//! Per ingest, with `N` items / `E` edges / `P` voted pairs / `C` components in
//! the group and `T` power iterations:
//!
//!   2 × compute_group_ranking  = O(E + T·(N + E))
//!   2U × scope_rank_of         = O(U · (P + E + T·(n_s + e_s)))
//!   2U × global_rank_of        = O(U · C · (E + P))  + O(U · T · (N + E))
//!   ────────────────────────────────────────────────────────────────────
//!   per ingest ≈ O(U · C · E)
//!
//! `global_rank_of` is the dominant term: to find one item's position it ranks
//! *every* connected component in the group from scratch, and each of those calls
//! rescans the full edge map. It is called 2U times per post and its result is a
//! single integer written into `rank_history`.
//!
//! Boot replay is `Σ` of that over every post in `events.jsonl`, and because `C`,
//! `E` and `N` all grow with the number of posts, replay of `M` posts is
//! super-quadratic in `M` — this is the ~20s benchmark below.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use slugsocial_server::reducer::ReducerState;

mod common;
use common::{ingest_event, replay, synth_document, synth_ingest_stream, Rng};

/// One post applied to an already-large garden. Isolates the per-write cost that
/// a user waits on when submitting through `POST /api/v0/rpc`.
fn single_ingest_into_existing_garden(c: &mut Criterion) {
    let mut g = c.benchmark_group("ingest/single_post");
    g.sample_size(10);

    // Kept small on purpose: building each base garden is itself a replay, and
    // replay is super-quadratic, so a 3200-post base would take hours to set up.
    for existing_posts in [50usize, 100, 135] {
        let warmup = synth_ingest_stream(existing_posts, 4, existing_posts * 2);
        let base = replay(&warmup);

        // The new post compares two items already present in the garden.
        let raw = synth_document(&[], &[(0, 1)]);
        let event = ingest_event(existing_posts, raw);

        g.throughput(Throughput::Elements(1));
        g.bench_with_input(
            BenchmarkId::from_parameter(existing_posts),
            &(base, event),
            |b, (base, event)| {
                b.iter_batched(
                    || base.clone(),
                    |mut state| {
                        state.apply_event(event.clone());
                        black_box(state.public().ranking_group.idx_to_item.len())
                    },
                    criterion::BatchSize::LargeInput,
                )
            },
        );
    }
    g.finish();
}

/// A single post carrying many votes. Shows the `U` factor: rank history is
/// recomputed per voted item, so a bulk import post pays `global_rank_of` once
/// per item it touches rather than once for the post.
fn votes_per_post_scaling(c: &mut Criterion) {
    let mut g = c.benchmark_group("ingest/votes_per_post");
    g.sample_size(10);

    const POOL: usize = 200;
    let warmup = synth_ingest_stream(100, 4, POOL);
    let base = replay(&warmup);

    for votes in [1usize, 4, 16, 64] {
        let mut rng = Rng::new(0x5EED ^ votes as u64);
        let pairs: Vec<(usize, usize)> = (0..votes)
            .map(|_| {
                let a = rng.below(POOL);
                let b = (a + 1 + rng.below(POOL - 1)) % POOL;
                (a, b)
            })
            .collect();
        let event = ingest_event(9999, synth_document(&[], &pairs));

        g.throughput(Throughput::Elements(votes as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(votes),
            &(base.clone(), event),
            |b, (base, event)| {
                b.iter_batched(
                    || base.clone(),
                    |mut state| {
                        state.apply_event(event.clone());
                        black_box(state.public().ranking_group.edges.len())
                    },
                    criterion::BatchSize::LargeInput,
                )
            },
        );
    }
    g.finish();
}

/// Cold boot: replay the whole event log. `server/src/main.rs` does exactly this
/// before the process starts serving, so this benchmark *is* startup latency.
///
/// Scaling sweep — the per-post cost grows with log length, so wall time grows
/// faster than linearly in `posts`.
fn boot_replay_scaling(c: &mut Criterion) {
    let mut g = c.benchmark_group("replay/scaling");
    g.sample_size(10);

    for posts in [25usize, 50, 75, 100] {
        let events = synth_ingest_stream(posts, 4, posts * 2);
        g.throughput(Throughput::Elements(posts as u64));
        g.bench_with_input(BenchmarkId::from_parameter(posts), &events, |b, events| {
            b.iter(|| {
                let mut state = ReducerState::default();
                for e in events {
                    state.apply_event(e.clone());
                }
                black_box(state.public().ranking_group.idx_to_item.len())
            })
        });
    }
    g.finish();
}

/// The slow path, sized so one iteration costs roughly 20 seconds.
///
/// 135 posts × 4 votes builds a garden of only 270 items and 536 compared pairs
/// — a genuinely small instance. Nothing here is adversarial: ordinary ratios,
/// four votes per post, one parent scope, no redactions. Override the size with
/// `SLUG_BENCH_REPLAY_POSTS` (cost scales ≈ M^2.5, so 200 posts ≈ 50s).
///
/// Criterion runs 10 samples, so budget ~4 minutes of wall clock for this group.
fn boot_replay_slow_path(c: &mut Criterion) {
    let mut g = c.benchmark_group("replay/slow_path");
    g.sample_size(10);
    g.sampling_mode(criterion::SamplingMode::Flat);
    g.measurement_time(std::time::Duration::from_secs(300));
    g.warm_up_time(std::time::Duration::from_secs(1));

    let posts = std::env::var("SLUG_BENCH_REPLAY_POSTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(135);
    let events = synth_ingest_stream(posts, 4, posts * 2);

    g.throughput(Throughput::Elements(posts as u64));
    g.bench_function(BenchmarkId::new("posts", posts), |b| {
        b.iter(|| {
            let mut state = ReducerState::default();
            for e in &events {
                state.apply_event(e.clone());
            }
            black_box(state.public().ranking_group.idx_to_item.len())
        })
    });
    g.finish();
}

criterion_group!(
    benches,
    single_ingest_into_existing_garden,
    votes_per_post_scaling,
    boot_replay_scaling,
    boot_replay_slow_path
);
criterion_main!(benches);
