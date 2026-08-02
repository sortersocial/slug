//! Head-to-head comparison of stationary-distribution solvers for Rank Centrality.
//!
//! Run: `cargo run --release -p slugsocial-server --example solver_probe`
//!
//! Reports, per (topology, solver): wall time, backward error `‖πP − π‖₁`, and
//! how badly the produced *order* disagrees with ground truth. On a chain the
//! ground truth order is known exactly by construction, and on any tree the
//! exact scores are known in closed form from detailed balance, so "how wrong
//! is the current implementation" is answerable without trusting any solver.

use std::time::Instant;

use slugsocial_server::path_types::ItemId;
use slugsocial_server::ranking::chain_from_edges;
use slugsocial_server::reducer::{GroupState, VoteData};
use slugsocial_server::stationary::{
    bicgstab, dense_gth, dense_lu, power, power_aitken, solve, sor, sparse_gth, Method, RankChain,
    Solution, SolveOptions, SparseGthOutcome,
};

const TOL: f64 = 1e-8;
const CAP: usize = 10_000;

// ---------------------------------------------------------------------------
// Graph generators (mirrors server/benches/common/mod.rs so numbers line up)
// ---------------------------------------------------------------------------

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

fn item_name(i: usize) -> String {
    format!("~/bench/i{i:06}")
}

fn vote(a: usize, b: usize, l: i32, r: i32, ts: i64) -> VoteData {
    VoteData {
        ts,
        a: ItemId::parse(&item_name(a)).unwrap(),
        b: ItemId::parse(&item_name(b)).unwrap(),
        ratio_left: l,
        ratio_right: r,
        body: "synthetic".into(),
        principal: "bench".into(),
        delegate: None,
        thread_tag: "bench".into(),
    }
}

#[derive(Clone, Copy)]
enum Topo {
    /// `0 > 1 > 2 > …`, every vote at a fixed ratio. Ground-truth order known.
    Chain {
        ratio: i32,
    },
    /// Chain with per-edge varying ratios (what `benches/common` builds).
    ChainVaried,
    Star,
    Clique,
    Sparse {
        degree: usize,
    },
    Components {
        count: usize,
        size: usize,
    },
}

fn pairs(t: Topo, n: usize) -> Vec<(usize, usize, i32)> {
    let mut out = Vec::new();
    match t {
        Topo::Chain { ratio } => {
            for i in 0..n.saturating_sub(1) {
                out.push((i, i + 1, ratio));
            }
        }
        Topo::ChainVaried => {
            for i in 0..n.saturating_sub(1) {
                out.push((i, i + 1, 2 + (i % 5) as i32));
            }
        }
        Topo::Star => {
            for i in 1..n {
                out.push((0, i, 2 + (i % 5) as i32));
            }
        }
        Topo::Clique => {
            let mut k = 0;
            for i in 0..n {
                for j in (i + 1)..n {
                    out.push((i, j, 2 + (k % 5)));
                    k += 1;
                }
            }
        }
        Topo::Sparse { degree } => {
            let mut rng = Rng::new(0xC0FFEE ^ n as u64);
            for i in 0..n.saturating_sub(1) {
                out.push((i, i + 1, 2));
            }
            for k in 0..(degree.saturating_sub(2) * n / 2) {
                let a = rng.below(n);
                let b = rng.below(n);
                if a != b {
                    out.push((a, b, 2 + (k % 5) as i32));
                }
            }
        }
        Topo::Components { count, size } => {
            for c in 0..count {
                let base = c * size;
                for i in 0..size.saturating_sub(1) {
                    out.push((base + i, base + i + 1, 2));
                }
            }
        }
    }
    out
}

fn node_count(t: Topo, n: usize) -> usize {
    match t {
        Topo::Components { count, size } => count * size,
        _ => n,
    }
}

fn build_chain(t: Topo, n: usize) -> RankChain {
    let total = node_count(t, n);
    let mut g = GroupState::new();
    for i in 0..total {
        g.ensure_item_pub(&item_name(i));
    }
    for (k, (i, j, ratio)) in pairs(t, total).into_iter().enumerate() {
        g.apply_vote(vote(i, j, ratio, 1, k as i64));
    }
    chain_from_edges(total, g.edges.iter().map(|(&k, &w)| (k, w)))
}

// ---------------------------------------------------------------------------
// Accuracy metrics
// ---------------------------------------------------------------------------

/// Kendall-tau distance between the order induced by `a` and by `b`, as a
/// fraction of all pairs. 0 = identical ranking, 0.5 = as good as random.
fn kendall_distance(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len();
    if n < 2 {
        return 0.0;
    }
    // Sort indices by `a` descending, then count inversions with respect to `b`.
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&x, &y| b[y].partial_cmp(&b[x]).unwrap_or(std::cmp::Ordering::Equal));
    let mut seq: Vec<f64> = idx.iter().map(|&i| a[i]).collect();
    let mut buf = seq.clone();
    let inv = count_inversions(&mut seq, &mut buf);
    let total = (n as f64) * (n as f64 - 1.0) / 2.0;
    inv as f64 / total
}

/// Inversions = pairs out of descending order (strict; ties are concordant).
fn count_inversions(v: &mut [f64], buf: &mut [f64]) -> u64 {
    let n = v.len();
    if n < 2 {
        return 0;
    }
    let mid = n / 2;
    let (l, r) = v.split_at_mut(mid);
    let (bl, br) = buf.split_at_mut(mid);
    let mut inv = count_inversions(l, bl) + count_inversions(r, br);
    let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
    while i < l.len() && j < r.len() {
        if l[i] >= r[j] {
            buf[k] = l[i];
            i += 1;
        } else {
            // every remaining element of `l` is smaller than r[j]: inversion
            inv += (l.len() - i) as u64;
            buf[k] = r[j];
            j += 1;
        }
        k += 1;
    }
    while i < l.len() {
        buf[k] = l[i];
        i += 1;
        k += 1;
    }
    while j < r.len() {
        buf[k] = r[j];
        j += 1;
        k += 1;
    }
    v.copy_from_slice(&buf[..n]);
    inv
}

/// The pre-change chain builder, kept verbatim so determinism can be measured
/// rather than argued about: adjacency rows and their weight sums are produced
/// by iterating a `HashMap`, whose order varies per instance.
fn legacy_chain(n: usize, edges: impl Iterator<Item = ((usize, usize), f64)>) -> RankChain {
    use std::collections::{HashMap, HashSet};
    let mut raw: HashMap<(usize, usize), f64> = HashMap::new();
    for ((src, dst), w) in edges {
        if src >= n || dst >= n || w <= 0.0 {
            continue;
        }
        *raw.entry((src, dst)).or_insert(0.0) += w;
    }
    let keys: Vec<(usize, usize)> = raw.keys().copied().collect();
    let mut normalized: HashMap<(usize, usize), f64> = HashMap::new();
    for (i, j) in keys {
        if normalized.contains_key(&(i, j)) {
            continue;
        }
        let w_ij = *raw.get(&(i, j)).unwrap_or(&0.0);
        let w_ji = *raw.get(&(j, i)).unwrap_or(&0.0);
        let total = w_ij + w_ji;
        if total <= 0.0 {
            continue;
        }
        normalized.insert((i, j), w_ij / total);
        if w_ji > 0.0 {
            normalized.insert((j, i), w_ji / total);
        }
    }
    let mut rows: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut neighbors: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for ((src, dst), w) in &normalized {
        rows[*src].push((*dst, *w));
        neighbors[*src].insert(*dst);
        neighbors[*dst].insert(*src);
    }
    let row_sum: Vec<f64> = rows
        .iter()
        .map(|r| r.iter().map(|&(_, w)| w).sum())
        .collect();
    let d_max = neighbors.iter().map(|s| s.len()).max().unwrap_or(0) as f64;
    RankChain {
        n,
        rows,
        row_sum,
        d_max,
    }
}

/// Pairs whose relative order differs between two score vectors.
fn order_flips(a: &[f64], b: &[f64]) -> usize {
    let n = a.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&x, &y| b[y].partial_cmp(&b[x]).unwrap_or(std::cmp::Ordering::Equal));
    let mut seq: Vec<f64> = idx.iter().map(|&i| a[i]).collect();
    let mut buf = seq.clone();
    count_inversions(&mut seq, &mut buf) as usize
}

/// Adjacent positions where a chain's known-correct descending order breaks,
/// split into strict inversions (genuinely the wrong way round) and ties (the
/// two items are indistinguishable, so their displayed order is arbitrary).
fn chain_order_violations(pi: &[f64]) -> (usize, usize) {
    let mut inv = 0;
    let mut tie = 0;
    for i in 0..pi.len().saturating_sub(1) {
        if pi[i] < pi[i + 1] {
            inv += 1;
        } else if pi[i] == pi[i + 1] {
            tie += 1;
        }
    }
    (inv, tie)
}

/// Exact π for a chain via detailed balance, in log space (no overflow).
///
/// A chain is a tree, so the Markov chain is reversible and
/// `π_i / π_{i+1} = a_{i+1,i} / a_{i,i+1}` holds exactly edge by edge.
fn chain_exact_log(t: Topo, n: usize) -> Vec<f64> {
    let mut logs = vec![0.0f64; n];
    for (i, (_, _, ratio)) in pairs(t, n).into_iter().enumerate() {
        // node i preferred over node i+1 at `ratio`:1, so π_i / π_{i+1} = ratio
        logs[i + 1] = logs[i] - (ratio as f64).ln();
    }
    logs
}

/// Largest absolute error in `ln π_i` against an exact log-space reference,
/// restricted to entries the reference says are representable in f64.
fn max_log_error(pi: &[f64], exact_log: &[f64]) -> (f64, usize) {
    let ref_max = exact_log.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let pi_max = pi.iter().cloned().fold(0.0f64, f64::max);
    if pi_max <= 0.0 {
        return (f64::INFINITY, pi.len());
    }
    let mut worst = 0.0f64;
    let mut lost = 0usize;
    for i in 0..pi.len() {
        let want = exact_log[i] - ref_max;
        if want < -700.0 {
            continue; // genuinely below f64 range; not the solver's fault
        }
        if pi[i] <= 0.0 {
            lost += 1;
            continue;
        }
        let got = (pi[i] / pi_max).ln();
        worst = worst.max((got - want).abs());
    }
    (worst, lost)
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

struct Run {
    name: &'static str,
    ms: f64,
    sol: Solution,
}

fn time_it(name: &'static str, mut f: impl FnMut() -> Solution) -> Run {
    // One warm-up, then the best of three: solver cost here is deterministic,
    // so the minimum is the cleanest estimate of the real cost.
    let sol = f();
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        let _ = f();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    Run {
        name,
        ms: best,
        sol,
    }
}

fn run_all(chain: &RankChain, include_dense: bool) -> Vec<Run> {
    let opts = SolveOptions {
        tol: TOL,
        max_iters: CAP,
        ..SolveOptions::default()
    };
    let big_opts = SolveOptions {
        tol: TOL,
        max_iters: 20_000_000,
        ..SolveOptions::default()
    };

    let mut runs = vec![
        time_it("power(10k) [current]", || power(chain, opts)),
        time_it("power+aitken", || power_aitken(chain, opts)),
        time_it("sor(1.0)", || sor(chain, opts, 1.0)),
        time_it("bicgstab", || bicgstab(chain, opts)),
        time_it("solve() [hybrid]", || solve(chain, opts)),
    ];
    if include_dense {
        runs.push(time_it("sparse-gth(uncapped)", || force_sparse_gth(chain)));
        runs.push(time_it("dense-gth", || dense_gth(chain, TOL)));
        runs.push(time_it("dense-lu", || dense_lu(chain, TOL)));
    }
    // Power iteration with an effectively unlimited cap: what the current
    // algorithm would produce if it were allowed to finish.
    if chain.n <= 1200 && chain.nnz() <= 8000 {
        runs.push(time_it("power(unbounded)", || power(chain, big_opts)));
    }
    runs
}

/// Sparse GTH with the budget disabled, for measuring what the direct path
/// would cost even on graphs the hybrid would refuse.
fn force_sparse_gth(chain: &RankChain) -> Solution {
    let unlimited = SolveOptions {
        direct_work_budget: u64::MAX,
        dense_core_max: usize::MAX,
        ..SolveOptions::default()
    };
    match sparse_gth(chain, unlimited, TOL) {
        SparseGthOutcome::Solved(s) => s,
        SparseGthOutcome::TooDense { .. } => unreachable!(),
    }
}

fn print_block(label: &str, chain: &RankChain, reference: &[f64], exact_log: Option<&[f64]>) {
    println!(
        "\n--- {label}   n={} nnz={} d_max={}",
        chain.n,
        chain.nnz(),
        chain.d_max as usize
    );
    println!(
        "{:<22} {:>9} {:>11} {:>5} {:>7} {:>10} {:>9} {:>10} {:>10} {:>10}",
        "method",
        "time(ms)",
        "resid L1",
        "conv",
        "iters",
        "kendall-d",
        "log-err",
        "inv/displ",
        "tie/maxsh",
        "kd(log pi)"
    );
    for r in run_all(chain, chain.n <= 1500) {
        let kd = kendall_distance(&r.sol.pi, reference);
        let kd_log = kendall_distance(&r.sol.log_pi, reference);
        let (logerr, _) = match exact_log {
            Some(e) => max_log_error(&r.sol.pi, e),
            None => (f64::NAN, 0usize),
        };
        let (inv, tie) = chain_order_violations(&r.sol.pi);
        let (inv, tie) = if exact_log.is_some() {
            // How the ranking would actually be displayed: sort by score
            // descending (ties broken by index, as a stable sort does) and see
            // how many items land somewhere other than their true rank.
            let n = r.sol.pi.len();
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| {
                r.sol.pi[b]
                    .partial_cmp(&r.sol.pi[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let misplaced = order.iter().enumerate().filter(|&(p, &i)| p != i).count();
            let shift = order
                .iter()
                .enumerate()
                .map(|(p, &i)| p.abs_diff(i))
                .max()
                .unwrap_or(0);
            (format!("{inv}/{misplaced}"), format!("{tie}/{shift}"))
        } else {
            ("-".to_string(), "-".to_string())
        };
        println!(
            "{:<22} {:>9.3} {:>11.2e} {:>5} {:>7} {:>10.2e} {:>9} {:>10} {:>10} {:>10.2e}",
            r.name,
            r.ms,
            r.sol.residual,
            if r.sol.converged { "yes" } else { "NO" },
            r.sol.iterations,
            kd,
            if logerr.is_nan() {
                "-".to_string()
            } else {
                format!("{logerr:.2e}")
            },
            inv,
            tie,
            kd_log
        );
    }
}

fn main() {
    let want: Vec<String> = std::env::args().skip(1).collect();
    let on = |s: &str| want.is_empty() || want.iter().any(|w| w == s);
    println!("Rank Centrality stationary solvers — tol={TOL:e}, power cap={CAP}");

    // ---- Chains: ground truth known exactly, and where the cap binds. ----
    if on("chain") {
        println!("\n================ CHAIN (ratio 2:1, exact order known) ================");
        for n in [100usize, 256, 512, 1024, 2048, 4000] {
            let t = Topo::Chain { ratio: 2 };
            let chain = build_chain(t, n);
            let exact = chain_exact_log(t, n);
            // Reference order is the construction order: node 0 best.
            let reference: Vec<f64> = (0..n).map(|i| -(i as f64)).collect();
            print_block(&format!("chain n={n}"), &chain, &reference, Some(&exact));
            println!(
                "   ground truth: π_0/π_{} = 2^{} = 10^{:.0}",
                n - 1,
                n - 1,
                (n - 1) as f64 * 2f64.log10()
            );
        }
    }
    if on("varied") {
        println!(
            "\n================ CHAIN (varied ratios 2..6, benches/common shape) ================"
        );
        for n in [256usize, 1024] {
            let t = Topo::ChainVaried;
            let chain = build_chain(t, n);
            let exact = chain_exact_log(t, n);
            let reference: Vec<f64> = (0..n).map(|i| -(i as f64)).collect();
            print_block(
                &format!("chain-varied n={n}"),
                &chain,
                &reference,
                Some(&exact),
            );
        }

        // ---- Other topologies: reference is sparse GTH (subtraction-free). ----
    }
    if on("topo") {
        println!("\n================ OTHER TOPOLOGIES ================");
        let cases: Vec<(String, Topo, usize)> = vec![
            ("star".into(), Topo::Star, 1024),
            ("clique".into(), Topo::Clique, 128),
            ("clique".into(), Topo::Clique, 400),
            ("sparse-d6".into(), Topo::Sparse { degree: 6 }, 1024),
            ("sparse-d6".into(), Topo::Sparse { degree: 6 }, 4000),
            ("sparse-d20".into(), Topo::Sparse { degree: 20 }, 1024),
            (
                "components 64x64".into(),
                Topo::Components {
                    count: 64,
                    size: 64,
                },
                0,
            ),
        ];
        for (label, t, n) in cases {
            let chain = build_chain(t, n);
            let reference = solve(
                &chain,
                SolveOptions {
                    tol: TOL,
                    max_iters: CAP,
                    ..SolveOptions::default()
                },
            )
            .pi;
            print_block(
                &format!("{label} n={}", node_count(t, n)),
                &chain,
                &reference,
                None,
            );
        }

        // ---- Which method does the hybrid pick, and what does elimination cost? ----
    }
    if on("dispatch") {
        println!("\n================ HYBRID DISPATCH + FILL-IN ================");
        println!(
            "{:<24} {:>7} {:>9} {:>14} {:>10} {:>12}",
            "graph", "n", "nnz", "picked", "iters", "resid"
        );
        let dispatch: Vec<(String, Topo, usize)> = vec![
            ("chain".into(), Topo::Chain { ratio: 2 }, 4000),
            ("star".into(), Topo::Star, 4000),
            ("clique".into(), Topo::Clique, 200),
            ("clique".into(), Topo::Clique, 600),
            ("clique".into(), Topo::Clique, 1200),
            ("sparse-d6".into(), Topo::Sparse { degree: 6 }, 4000),
            ("sparse-d20".into(), Topo::Sparse { degree: 20 }, 4000),
            ("sparse-d6".into(), Topo::Sparse { degree: 6 }, 20000),
        ];
        for (label, t, n) in dispatch {
            let chain = build_chain(t, n);
            let opts = SolveOptions {
                tol: TOL,
                max_iters: CAP,
                ..SolveOptions::default()
            };
            let t0 = Instant::now();
            let s = solve(&chain, opts);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            println!(
                "{:<24} {:>7} {:>9} {:>14} {:>10} {:>12.2e}   {:.2} ms {}",
                label,
                chain.n,
                chain.nnz(),
                s.method.label(),
                s.iterations,
                s.residual,
                ms,
                if s.converged { "" } else { "NOT CONVERGED" }
            );
        }

        // ---- Determinism: does HashMap iteration order move the scores? ----
    }
    if on("determinism") {
        println!("\n================ DETERMINISM ================");
        for (label, t, n) in [
            ("sparse-d6 n=1024", Topo::Sparse { degree: 6 }, 1024usize),
            ("clique n=128", Topo::Clique, 128),
        ] {
            let mut first: Option<Vec<f64>> = None;
            let mut worst = 0.0f64;
            for _ in 0..8 {
                let chain = build_chain(t, n);
                let pi = force_sparse_gth(&chain).pi;
                match &first {
                    None => first = Some(pi),
                    Some(f) => {
                        for i in 0..pi.len() {
                            worst = worst.max((pi[i] - f[i]).abs());
                        }
                    }
                }
            }
            println!("{label:<24} max |Δπ| across 8 rebuilds: {worst:.3e}");
        }

        // Control: the pre-change builder, rebuilt from scratch each round so
        // every rebuild gets a fresh `HashMap` with a different iteration order.
        println!("\ncontrol — legacy HashMap-ordered builder, 8 rebuilds of the same graph:");
        for (label, t, n) in [
            ("sparse-d6 n=1024", Topo::Sparse { degree: 6 }, 1024usize),
            ("clique n=128", Topo::Clique, 128),
            ("chain n=512", Topo::Chain { ratio: 2 }, 512),
        ] {
            let total = node_count(t, n);
            let mut g = GroupState::new();
            for i in 0..total {
                g.ensure_item_pub(&item_name(i));
            }
            for (k, (i, j, ratio)) in pairs(t, total).into_iter().enumerate() {
                g.apply_vote(vote(i, j, ratio, 1, k as i64));
            }
            let opts = SolveOptions {
                tol: TOL,
                max_iters: CAP,
                ..SolveOptions::default()
            };
            let mut reference: Option<Vec<f64>> = None;
            let mut worst_abs = 0.0f64;
            let mut worst_rel = 0.0f64;
            let mut flips = 0usize;
            let mut distinct = 0usize;
            for _ in 0..8 {
                let c = legacy_chain(total, g.edges.iter().map(|(&k, &w)| (k, w)));
                let pi = power(&c, opts).pi;
                match &reference {
                    None => reference = Some(pi),
                    Some(r) => {
                        if pi != *r {
                            distinct += 1;
                        }
                        for i in 0..pi.len() {
                            worst_abs = worst_abs.max((pi[i] - r[i]).abs());
                            if r[i] > 0.0 {
                                worst_rel = worst_rel.max((pi[i] - r[i]).abs() / r[i]);
                            }
                        }
                        flips += order_flips(&pi, r);
                    }
                }
            }
            println!(
                "{label:<20} differing rebuilds {distinct}/7  max |Δπ|={worst_abs:.2e}  \
                 max rel={worst_rel:.2e}  rank flips {flips}"
            );
        }

        // Cost of the sort that buys determinism, isolated from the `HashMap`
        // aggregation `chain_from_edges` has always done.
        println!("\nsort cost (edge ordering inside chain_from_edges):");
        for (label, t, n) in [
            ("sparse-d6 n=4000", Topo::Sparse { degree: 6 }, 4000usize),
            ("clique n=400", Topo::Clique, 400),
        ] {
            let total = node_count(t, n);
            let mut g = GroupState::new();
            for i in 0..total {
                g.ensure_item_pub(&item_name(i));
            }
            for (k, (i, j, ratio)) in pairs(t, total).into_iter().enumerate() {
                g.apply_vote(vote(i, j, ratio, 1, k as i64));
            }
            let t0 = Instant::now();
            for _ in 0..20 {
                let c = chain_from_edges(total, g.edges.iter().map(|(&k, &w)| (k, w)));
                std::hint::black_box(c.nnz());
            }
            let with_sort = t0.elapsed().as_secs_f64() * 1000.0 / 20.0;

            let edges: Vec<((usize, usize), f64)> = g.edges.iter().map(|(&k, &w)| (k, w)).collect();
            let t0 = Instant::now();
            for _ in 0..20 {
                let mut keys: Vec<(usize, usize)> = edges.iter().map(|&(k, _)| k).collect();
                keys.sort_unstable();
                std::hint::black_box(keys.len());
            }
            let sort_only = t0.elapsed().as_secs_f64() * 1000.0 / 20.0;
            println!(
                "{label:<20} chain_from_edges {with_sort:7.3} ms, of which sorting is \
                 {sort_only:6.3} ms  (E={})",
                g.edges.len()
            );
        }

        // ---- Cross-method equivalence on small graphs, at machine precision. ----
    }
    if on("equiv") {
        println!("\n================ EQUIVALENCE (small n, all methods) ================");
        for (label, t, n) in [
            ("chain n=64", Topo::Chain { ratio: 2 }, 64usize),
            ("star n=64", Topo::Star, 64),
            ("clique n=64", Topo::Clique, 64),
            ("sparse-d6 n=200", Topo::Sparse { degree: 6 }, 200),
        ] {
            let chain = build_chain(t, n);
            let gth = dense_gth(&chain, TOL).pi;
            let mut worst: Vec<(String, f64)> = Vec::new();
            let opts = SolveOptions {
                tol: 1e-14,
                max_iters: 200_000,
                ..SolveOptions::default()
            };
            let cands: Vec<(&str, Vec<f64>)> = vec![
                ("sparse-gth", force_sparse_gth(&chain).pi),
                ("dense-lu", dense_lu(&chain, TOL).pi),
                ("power(200k)", power(&chain, opts).pi),
                ("sor", sor(&chain, opts, 1.0).pi),
                ("bicgstab", bicgstab(&chain, opts).pi),
            ];
            for (name, pi) in cands {
                let rel = (0..chain.n)
                    .filter(|&i| gth[i] > 1e-300)
                    .map(|i| (pi[i] - gth[i]).abs() / gth[i])
                    .fold(0.0f64, f64::max);
                worst.push((name.to_string(), rel));
            }
            let cells: Vec<String> = worst.iter().map(|(n, v)| format!("{n}={v:.1e}")).collect();
            println!("{label:<18} max rel err vs dense-GTH: {}", cells.join("  "));
        }
    }
    let _ = Method::Power;
}
