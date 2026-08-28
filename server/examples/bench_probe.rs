//! Sizing probe for the ranking benchmarks: prints graph shape and wall time for
//! boot replay at several log lengths so bench parameters can be calibrated.
//!
//! Run: `cargo run --release -p slugsocial-server --example bench_probe`

use std::time::Instant;

use slugsocial_server::events::{Event, Ingest};
use slugsocial_server::path_types::ItemId;
use slugsocial_server::ranking::{
    compute_group_ranking, connected_components_from_voted_pairs, ranked_items_subset,
};
use slugsocial_server::reducer::{GroupState, ReducerState, VoteData};
use slugsocial_server::scope_rank::build_rankings_for_item_set;

fn item_name(i: usize) -> String {
    format!("~/bench/i{i:06}")
}

fn item_id(i: usize) -> ItemId {
    ItemId::parse(&item_name(i)).unwrap()
}

struct Rng(u64);
impl Rng {
    fn new(s: u64) -> Self {
        Rng(s | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

fn vote(a: usize, b: usize, l: i32, ts: i64) -> VoteData {
    VoteData {
        ts,
        a: item_id(a),
        b: item_id(b),
        ratio_left: l,
        ratio_right: 1,
        body: "synthetic".into(),
        principal: "bench".into(),
        delegate: None,
        thread_tag: "bench".into(),
    }
}

fn chain_group(n: usize) -> GroupState {
    let mut g = GroupState::new();
    for i in 0..n {
        g.ensure_item_pub(&item_name(i));
    }
    for i in 0..n - 1 {
        g.apply_vote(vote(i, i + 1, 2, i as i64));
    }
    g
}

fn sparse_group(n: usize, degree: usize) -> GroupState {
    let mut g = GroupState::new();
    let mut rng = Rng::new(0xC0FFEE ^ n as u64);
    for i in 0..n {
        g.ensure_item_pub(&item_name(i));
    }
    let mut ts = 0i64;
    for i in 0..n - 1 {
        g.apply_vote(vote(i, i + 1, 2, ts));
        ts += 1;
    }
    for _ in 0..(degree.saturating_sub(2) * n / 2) {
        let a = rng.below(n);
        let b = rng.below(n);
        if a != b {
            g.apply_vote(vote(a, b, 3, ts));
            ts += 1;
        }
    }
    g
}

/// Pure random pairing with no spanning backbone — matches what the replay
/// workload actually builds once the item pool is full.
fn random_group(n: usize, votes: usize) -> GroupState {
    let mut g = GroupState::new();
    let mut rng = Rng::new(0xD00D ^ n as u64);
    for i in 0..n {
        g.ensure_item_pub(&item_name(i));
    }
    for ts in 0..votes {
        let a = rng.below(n);
        let b = rng.below(n);
        if a != b {
            g.apply_vote(vote(a, b, 2, ts as i64));
        }
    }
    g
}

fn synth_document(items: &[usize], pairs: &[(usize, usize)]) -> String {
    let mut s = String::from("#bench\n\n");
    for &i in items {
        s.push_str(&format!("{} {{ synthetic item {i} }}\n", item_name(i)));
    }
    s.push('\n');
    for (k, &(a, b)) in pairs.iter().enumerate() {
        let left = 2 + (k % 5);
        s.push_str(&format!(
            "{{ synthetic comparison {k} }}\n{} {left}:1 {}\n",
            item_name(a),
            item_name(b)
        ));
    }
    s
}

fn stream(posts: usize, votes_per_post: usize, n: usize) -> Vec<Event> {
    let mut rng = Rng::new(0xBEEF);
    let mut next_new = 0usize;
    let mut out = Vec::with_capacity(posts);
    for p in 0..posts {
        let mut items = Vec::new();
        let mut pairs = Vec::new();
        for _ in 0..votes_per_post {
            let a = if next_new < n {
                let a = next_new;
                next_new += 1;
                items.push(a);
                a
            } else {
                rng.below(n)
            };
            let b = if next_new < n {
                let b = next_new;
                next_new += 1;
                items.push(b);
                b
            } else {
                rng.below(n)
            };
            if a != b {
                pairs.push((a, b));
            }
        }
        out.push(Event::Ingest(Ingest {
            ts: 1_700_000_000_000 + p as i64,
            id: format!("bench-{p:06}"),
            raw: synth_document(&items, &pairs),
            principal: "bench".into(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "bench".into(),
        }));
    }
    out
}

fn main() {
    println!("== iteration counts (power iteration convergence) ==");
    for n in [16usize, 32, 64, 128, 256, 512, 1024] {
        let mut g = chain_group(n);
        let t = Instant::now();
        compute_group_ranking(&mut g, 10_000, 1e-8);
        let chain_ms = t.elapsed().as_secs_f64() * 1000.0;

        let mut s = sparse_group(n, 6);
        let t = Instant::now();
        compute_group_ranking(&mut s, 10_000, 1e-8);
        let sparse_ms = t.elapsed().as_secs_f64() * 1000.0;

        println!(
            "n={n:6}  chain: {chain_ms:9.3} ms (E={:6})   sparse-d6: {sparse_ms:9.3} ms (E={:6})",
            g.edges.len(),
            s.edges.len()
        );
    }

    // `max_iters` is a parameter, so sweeping it reveals where the loop actually
    // stops: once wall time stops growing with the cap, the tolerance was met.
    // If time keeps growing all the way to 10_000, the cap is binding and the
    // returned scores are NOT converged.
    println!("\n== where does power iteration actually stop? (time vs max_iters cap) ==");
    println!(
        "{:<28} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "graph", "k=10", "k=100", "k=1000", "k=10000", "verdict"
    );
    for (label, g) in [
        ("chain n=256", chain_group(256)),
        ("chain n=1024", chain_group(1024)),
        ("sparse d6 n=1024", sparse_group(1024, 6)),
        ("random n=280 v=550 (replay)", random_group(280, 550)),
        ("random n=400 v=800 (replay)", random_group(400, 800)),
        ("clique n=128", {
            let mut g = GroupState::new();
            for i in 0..128 {
                g.ensure_item_pub(&item_name(i));
            }
            let mut ts = 0i64;
            for i in 0..128 {
                for j in (i + 1)..128 {
                    g.apply_vote(vote(i, j, 2, ts));
                    ts += 1;
                }
            }
            g
        }),
    ] {
        let idxs: Vec<usize> = (0..g.idx_to_item.len()).collect();
        let mut times = Vec::new();
        for k in [10usize, 100, 1000, 10_000] {
            let t = Instant::now();
            let reps = if k >= 1000 { 3 } else { 20 };
            for _ in 0..reps {
                let _ = ranked_items_subset(&g, &idxs, k, 1e-8);
            }
            times.push(t.elapsed().as_secs_f64() * 1000.0 / reps as f64);
        }
        // Still growing between k=1000 and k=10000 means the cap binds.
        let saturated = times[3] > times[2] * 3.0;
        println!(
            "{label:<28} {:>8.2}ms {:>8.2}ms {:>8.2}ms {:>8.2}ms  {}",
            times[0],
            times[1],
            times[2],
            times[3],
            if saturated {
                "CAP BINDS (not converged)"
            } else {
                "converged early"
            }
        );
    }

    println!("\n== subset ranking pays full edge scan ==");
    for n in [1024usize, 4096, 16384, 65536] {
        let g = sparse_group(n, 6);
        let t = Instant::now();
        for _ in 0..100 {
            let _ = ranked_items_subset(&g, &[0, 1], 10_000, 1e-8);
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / 100.0;
        println!(
            "group n={n:6} E={:7}  rank 2 items: {us:10.1} us",
            g.edges.len()
        );
    }

    println!("\n== garden page render: fixed 4096 items, varying component count ==");
    for size in [1024usize, 256, 64, 16, 4, 2] {
        let comps = 4096 / size;
        let mut content = slugsocial_server::reducer::ContentState::default();
        let mut g = GroupState::new();
        for i in 0..4096 {
            g.ensure_item_pub(&item_name(i));
        }
        let mut ts = 0i64;
        for c in 0..comps {
            let base = c * size;
            for i in 0..size - 1 {
                g.apply_vote(vote(base + i, base + i + 1, 2, ts));
                ts += 1;
            }
        }
        content.ranking_group = g;
        let parent = ItemId::parse("~/bench").unwrap();
        let children = content.item_children.entry(parent).or_default();
        for i in 0..4096 {
            children.insert(item_id(i));
        }
        let items: Vec<ItemId> = (0..4096).map(item_id).collect();

        let t = Instant::now();
        let r = build_rankings_for_item_set(&content, &items);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "components={comps:5} size={size:5}  render: {ms:10.3} ms  (got {} comps)",
            r.component_rankings.len()
        );
    }

    println!("\n== connected components (honest O(n+P)) ==");
    for n in [16384usize, 65536, 262144] {
        let g = sparse_group(n, 6);
        let t = Instant::now();
        let (c, i) = connected_components_from_voted_pairs(
            g.idx_to_item.len(),
            g.voted_pairs.iter().copied(),
        );
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "n={n:7} P={:8}  cc: {ms:9.3} ms  ({} comps, {} isolates)",
            g.voted_pairs.len(),
            c.len(),
            i.len()
        );
    }

    // Attribute the per-post cost: replay the same 135 posts with the vote lines
    // stripped (items only), then rebuild the identical vote graph directly into
    // a GroupState. Whatever is left over is the rank-history machinery.
    println!("\n== cost attribution for a 135-post replay ==");
    {
        let posts = 135usize;
        let events = stream(posts, 4, posts * 2);

        let t = Instant::now();
        let mut full = ReducerState::default();
        for e in &events {
            full.apply_event(e.clone());
        }
        let full_s = t.elapsed().as_secs_f64();

        // Items only: same item declarations, zero votes -> zero ranking work.
        // Regenerated (not line-filtered) so the documents stay well formed.
        let items_only: Vec<Event> = events
            .iter()
            .enumerate()
            .map(|(p, e)| {
                let Event::Ingest(i) = e else { unreachable!() };
                let declared: Vec<usize> = i
                    .raw
                    .lines()
                    .filter_map(|l| l.strip_prefix("~/bench/i"))
                    .filter_map(|rest| rest.split_whitespace().next())
                    .filter_map(|num| num.parse::<usize>().ok())
                    .collect();
                let _ = p;
                Event::Ingest(Ingest {
                    raw: synth_document(&declared, &[]),
                    ..i.clone()
                })
            })
            .collect();
        let t = Instant::now();
        let mut novote = ReducerState::default();
        for e in &items_only {
            novote.apply_event(e.clone());
        }
        let parse_s = t.elapsed().as_secs_f64();

        // Raw graph construction only.
        let g_final = full.public().ranking_group.clone();
        let t = Instant::now();
        let mut g = GroupState::new();
        for (idx, it) in g_final.idx_to_item.iter().enumerate() {
            let _ = idx;
            g.ensure_item_pub(it.as_str());
        }
        for (k, (i, j)) in g_final.voted_pairs.iter().enumerate() {
            g.apply_vote(vote(*i, *j, 2, k as i64));
        }
        let graph_s = t.elapsed().as_secs_f64();

        println!("full replay                     : {full_s:8.3} s");
        println!(
            "  DSL parse + item bookkeeping  : {parse_s:8.3} s  ({:5.1}%)",
            parse_s / full_s * 100.0
        );
        println!(
            "  raw vote-graph construction   : {graph_s:8.3} s  ({:5.1}%)",
            graph_s / full_s * 100.0
        );
        println!(
            "  ranking + rank-history        : {:8.3} s  ({:5.1}%)",
            full_s - parse_s - graph_s,
            (full_s - parse_s - graph_s) / full_s * 100.0
        );

        // How much of that is component fan-out inside global_rank_of?
        let (comps, iso) = connected_components_from_voted_pairs(
            g_final.idx_to_item.len(),
            g_final.voted_pairs.iter().copied(),
        );
        let sizes: Vec<usize> = {
            let mut s: Vec<usize> = comps.iter().map(|c| c.len()).collect();
            s.sort_unstable_by(|a, b| b.cmp(a));
            s
        };
        println!(
            "final graph: N={} E={} P={} components={} (largest {:?}) isolates={}",
            g_final.idx_to_item.len(),
            g_final.edges.len(),
            g_final.voted_pairs.len(),
            comps.len(),
            &sizes[..sizes.len().min(5)],
            iso.len()
        );

        // One global_rank_of-equivalent: rank every component from scratch.
        let t = Instant::now();
        for c in &comps {
            let _ = ranked_items_subset(&g_final, c, 10_000, 1e-8);
        }
        let one_global = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "one full global ordering at final size: {one_global:.2} ms; memoized ingest computes it once after voting"
        );
    }

    println!("\n== boot replay scaling (the slow path) ==");
    let sweep: Vec<usize> = std::env::args()
        .nth(1)
        .map(|a| a.split(',').filter_map(|s| s.parse().ok()).collect())
        .unwrap_or_else(|| vec![135]);
    for posts in sweep {
        let events = stream(posts, 4, posts * 2);
        let t = Instant::now();
        let mut state = ReducerState::default();
        for e in &events {
            state.apply_event(e.clone());
        }
        let secs = t.elapsed().as_secs_f64();
        let pub_ = state.public();
        println!(
            "posts={posts:6}  replay: {secs:9.3} s   N={:6} E={:7} P={:7}  ({:.2} ms/post)",
            pub_.ranking_group.idx_to_item.len(),
            pub_.ranking_group.edges.len(),
            pub_.ranking_group.voted_pairs.len(),
            secs * 1000.0 / posts as f64
        );
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }
}
