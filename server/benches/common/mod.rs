//! Synthetic graph + ingest generators shared by the ranking benchmarks.
//!
//! Everything here is deterministic (xorshift PRNG, fixed seeds) so successive
//! criterion runs compare like for like.

#![allow(dead_code)]

use slugsocial_server::events::{Event, Ingest};
use slugsocial_server::path_types::ItemId;
use slugsocial_server::reducer::{ContentState, GroupState, ReducerState, VoteData};

/// Deterministic xorshift64* so benchmarks never depend on `rand` seeding.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Comparison-graph shapes. Each stresses a different term in the cost model:
/// `Clique` maximizes edge count `E`, `Chain` maximizes the number of power
/// iterations (spectral gap shrinks as `1/n^2`), `ManyComponents` maximizes the
/// component count `C` that scoped ranking loops over.
#[derive(Clone, Copy, Debug)]
pub enum Topology {
    /// Path graph `0-1-2-…-(n-1)`. Worst-case mixing time.
    Chain,
    /// One hub compared against every other node. Sparse, fast mixing.
    Star,
    /// Every pair compared. `E = n(n-1)`, dense but well conditioned.
    Clique,
    /// `n` nodes, `degree * n / 2` random pairs. Realistic sparse graph.
    RandomSparse { degree: usize },
    /// `components` disjoint chains of `size` nodes each.
    ManyComponents { components: usize, size: usize },
}

impl Topology {
    pub fn label(&self) -> String {
        match self {
            Topology::Chain => "chain".into(),
            Topology::Star => "star".into(),
            Topology::Clique => "clique".into(),
            Topology::RandomSparse { degree } => format!("sparse-d{degree}"),
            Topology::ManyComponents { components, size } => {
                format!("comps{components}x{size}")
            }
        }
    }

    /// Unordered pairs (i, j) with i < j that should receive a vote.
    pub fn pairs(&self, n: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        match *self {
            Topology::Chain => {
                for i in 0..n.saturating_sub(1) {
                    out.push((i, i + 1));
                }
            }
            Topology::Star => {
                for i in 1..n {
                    out.push((0, i));
                }
            }
            Topology::Clique => {
                for i in 0..n {
                    for j in (i + 1)..n {
                        out.push((i, j));
                    }
                }
            }
            Topology::RandomSparse { degree } => {
                let mut rng = Rng::new(0xC0FFEE ^ n as u64);
                // Spanning chain first so the graph stays one component, then
                // random chords up to the requested average degree.
                for i in 0..n.saturating_sub(1) {
                    out.push((i, i + 1));
                }
                let extra = degree.saturating_sub(2) * n / 2;
                for _ in 0..extra {
                    let a = rng.below(n);
                    let b = rng.below(n);
                    if a != b {
                        out.push((a.min(b), a.max(b)));
                    }
                }
            }
            Topology::ManyComponents { components, size } => {
                for c in 0..components {
                    let base = c * size;
                    for i in 0..size.saturating_sub(1) {
                        out.push((base + i, base + i + 1));
                    }
                }
            }
        }
        out
    }

    /// Total node count implied by the topology for a nominal size `n`.
    pub fn node_count(&self, n: usize) -> usize {
        match *self {
            Topology::ManyComponents { components, size } => components * size,
            _ => n,
        }
    }
}

pub fn item_name(i: usize) -> String {
    format!("~/bench/i{i:06}")
}

pub fn item_id(i: usize) -> ItemId {
    ItemId::parse(&item_name(i)).expect("valid item id")
}

pub fn vote_data(a: usize, b: usize, ratio_left: i32, ratio_right: i32, ts: i64) -> VoteData {
    VoteData {
        ts,
        a: item_id(a),
        b: item_id(b),
        ratio_left,
        ratio_right,
        body: "synthetic".to_string(),
        principal: "bench".to_string(),
        delegate: None,
        thread_tag: "bench".to_string(),
    }
}

/// Build a `GroupState` directly (skips DSL parsing) for kernel benchmarks.
///
/// Every node is registered up front so `idx_to_item` order matches the
/// topology's node numbering, then one vote is applied per pair. Ratios vary
/// slightly so the stationary distribution is non-degenerate.
pub fn build_group(topo: Topology, n: usize) -> GroupState {
    let total = topo.node_count(n);
    let mut g = GroupState::new();
    for i in 0..total {
        g.ensure_item_pub(&item_name(i));
    }
    for (k, (i, j)) in topo.pairs(total).into_iter().enumerate() {
        let left = 2 + (k % 5) as i32;
        g.apply_vote(vote_data(i, j, left, 1, k as i64));
    }
    g
}

/// A `ContentState` whose `ranking_group` matches `build_group` and whose
/// `item_children` puts every node under a single parent (`~/bench`), which is
/// what garden pages resolve when rendering a scope.
pub fn build_content(topo: Topology, n: usize) -> ContentState {
    let total = topo.node_count(n);
    let mut content = ContentState::default();
    content.ranking_group = build_group(topo, n);
    let parent = ItemId::parse("~/bench").expect("parent");
    let children = content.item_children.entry(parent).or_default();
    for i in 0..total {
        children.insert(item_id(i));
    }
    for i in 0..total {
        content.items.insert(item_id(i));
        content
            .item_bodies
            .insert(item_id(i), format!("body of item {i}"));
    }
    content
}

pub fn bench_parent() -> ItemId {
    ItemId::parse("~/bench").expect("parent")
}

/// All nodes of the topology as an explicit scope list.
pub fn scope_items(topo: Topology, n: usize) -> Vec<ItemId> {
    (0..topo.node_count(n)).map(item_id).collect()
}

// ---------------------------------------------------------------------------
// DSL-level generators (exercise parse + reducer, not just the kernel)
// ---------------------------------------------------------------------------

/// Render a `.sorter` document declaring `items` and casting `votes`.
pub fn synth_document(item_idxs: &[usize], vote_pairs: &[(usize, usize)]) -> String {
    let mut s = String::with_capacity(item_idxs.len() * 48 + vote_pairs.len() * 64);
    s.push_str("#bench\n\n");
    for &i in item_idxs {
        s.push_str(&format!("{} {{ synthetic item {i} }}\n", item_name(i)));
    }
    s.push('\n');
    for (k, &(a, b)) in vote_pairs.iter().enumerate() {
        let left = 2 + (k % 5);
        s.push_str(&format!(
            "{{ synthetic comparison {k} }}\n{} {left}:1 {}\n",
            item_name(a),
            item_name(b)
        ));
    }
    s
}

pub fn ingest_event(id: usize, raw: String) -> Event {
    Event::Ingest(Ingest {
        ts: 1_700_000_000_000 + id as i64,
        id: format!("bench-{id:06}"),
        raw,
        principal: "bench".to_string(),
        delegate: None,
        room_id: "public".to_string(),
        thread_tag: "bench".to_string(),
    })
}

/// A stream of ingests that grows one garden: each post declares `votes_per_post`
/// new comparisons over a pool of `n` items, chained so the graph stays connected.
///
/// This mirrors the real durable path — every post appends to `events.jsonl` and
/// is replayed through `ReducerState::apply_event` on boot.
pub fn synth_ingest_stream(posts: usize, votes_per_post: usize, n: usize) -> Vec<Event> {
    let mut events = Vec::with_capacity(posts);
    let mut rng = Rng::new(0xBEEF);
    let mut next_new_item = 0usize;

    for p in 0..posts {
        let mut items = Vec::new();
        let mut pairs = Vec::new();
        for _ in 0..votes_per_post {
            // Bias toward introducing fresh items until the pool is full, then
            // compare existing ones — the shape a real garden fills in over time.
            let a = if next_new_item < n {
                let a = next_new_item;
                next_new_item += 1;
                items.push(a);
                a
            } else {
                rng.below(n)
            };
            let b = if next_new_item < n {
                let b = next_new_item;
                next_new_item += 1;
                items.push(b);
                b
            } else {
                rng.below(n)
            };
            if a != b {
                pairs.push((a, b));
            }
        }
        events.push(ingest_event(p, synth_document(&items, &pairs)));
    }
    events
}

/// Replay a prebuilt event stream into a fresh reducer, exactly as boot does.
pub fn replay(events: &[Event]) -> ReducerState {
    let mut state = ReducerState::default();
    for e in events {
        state.apply_event(e.clone());
    }
    state
}
