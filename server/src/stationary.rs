//! Stationary-distribution solvers for the Rank Centrality Markov chain.
//!
//! `ranking::compute_scores_from_edges` builds a chain over compared items and
//! needs π with `πP = π`, `Σπ = 1`. This module owns every candidate solver so
//! they can be compared against each other on identical input, and so the
//! ranking code has exactly one place to ask for "the stationary distribution,
//! and tell me whether you actually got there".
//!
//! # The chain
//!
//! Rank Centrality (Negahban, Oh, Shah 2012, §3.1) uses
//!
//! ```text
//!   P_ij = a_ij / d_max        (i ≠ j, compared)
//!   P_ii = 1 - (Σ_k a_ik) / d_max
//! ```
//!
//! where `a_ij = A_ij / (A_ij + A_ji)` and `d_max` is the maximum *unweighted*
//! degree. `d_max` is a uniformization constant: it exists only to keep the
//! chain aperiodic so power iteration cannot oscillate (issue #146). Because
//!
//! ```text
//!   πP = π   ⟺   πQ = 0,   Q = P - I,   Q_ij = a_ij / d_max  (i ≠ j)
//! ```
//!
//! and `Q` is only defined by its off-diagonal entries (the diagonal is minus
//! the row sum), scaling every off-diagonal by the same `1/d_max` leaves π
//! unchanged. **Direct solvers therefore do not need `d_max` at all** and are
//! structurally immune to the periodicity bug. Only the iterative solvers,
//! which literally walk `P`, care.
//!
//! # Dynamic range
//!
//! On a tree (a chain is a tree) the chain is reversible and detailed balance
//! pins the answer exactly: `π_i / π_j = a_ji / a_ij` for every edge. A chain
//! of `n` items each preferred 2:1 over the next therefore has
//! `π_max / π_min = 2^(n-1)`, which leaves the f64 range at n ≈ 1075. This is a
//! property of the model, not of any solver: past that width the tail of the
//! ranking is not representable in double precision and underflows to zero.
//! [`Solution::underflowed`] reports it, and [`Solution::log_pi`] stays exact
//! there, which is why it — not `pi` — is the sort key for a ranking.
//!
//! # Strategy ([`solve`])
//!
//! 1. Split disconnected components; each is solved on its own and keeps the
//!    share of the mass its node count started with (what power iteration from
//!    a uniform start converges to).
//! 2. Sparse GTH state reduction, minimum-degree order, with a degree guard and
//!    a work budget. Trees and chains reduce completely in `O(n)`; whatever
//!    survives is an irreducible core solved by dense GTH when it fits.
//! 3. Otherwise Gauss–Seidel, then power iteration as a second opinion. Graphs
//!    that defeat step 2 are exactly the well-connected ones these finish in
//!    tens of sweeps.
//!
//! Nothing returns without a checked residual: see [`Solution::converged`].

/// Which solver produced a [`Solution`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// Baseline: repeated `π ← πP` until the L1 step falls under `tol`.
    Power,
    /// Power iteration with periodic Aitken Δ² extrapolation.
    PowerAitken,
    /// Gauss–Seidel / SOR sweeps on the balance equations.
    Sor,
    /// Dense LU with partial pivoting on `Qᵀπ = e`, one row replaced by `Σπ = 1`.
    DenseLu,
    /// Dense Grassmann–Taksar–Heyman state reduction, `O(n³)`, subtraction-free.
    DenseGth,
    /// GTH state reduction over a sparse graph with minimum-degree elimination.
    SparseGth,
    /// Jacobi-preconditioned BiCGSTAB on the singular balance system.
    BiCgStab,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Power => "power",
            Method::PowerAitken => "power+aitken",
            Method::Sor => "sor",
            Method::DenseLu => "dense-lu",
            Method::DenseGth => "dense-gth",
            Method::SparseGth => "sparse-gth",
            Method::BiCgStab => "bicgstab",
        }
    }

    /// Direct methods answer exactly (up to rounding); iterative ones can stall.
    pub fn is_direct(self) -> bool {
        matches!(self, Method::DenseLu | Method::DenseGth | Method::SparseGth)
    }
}

/// A stationary distribution plus everything needed to judge whether to trust it.
#[derive(Debug, Clone)]
pub struct Solution {
    /// Non-negative, sums to 1 (unless every entry underflowed).
    pub pi: Vec<f64>,
    /// `ln π_i`, shifted so the largest entry is 0. Direct solvers fill this in
    /// exactly even where `pi` has flushed to zero, so it is the only faithful
    /// sort key on graphs whose score spread exceeds the f64 range.
    /// `f64::NEG_INFINITY` for genuinely unreachable states.
    pub log_pi: Vec<f64>,
    pub method: Method,
    /// Sweeps performed. Zero for direct methods.
    pub iterations: usize,
    /// `‖πP − π‖₁`, the honest backward error of the answer that was returned.
    pub residual: f64,
    /// False means the number that came back is *not* the stationary
    /// distribution to the requested tolerance. Never silently true.
    pub converged: bool,
    /// The exact answer spans more than f64 can hold, so the tail of the
    /// ranking has been flushed to zero and its internal order is lost.
    pub underflowed: bool,
}

/// Tuning for [`solve`]. Defaults encode the recommended hybrid strategy.
#[derive(Debug, Clone, Copy)]
pub struct SolveOptions {
    pub tol: f64,
    pub max_iters: usize,
    /// Row-merge budget for the sparse elimination in [`sparse_gth`]. Bounds
    /// the work wasted before giving up on the direct path.
    pub direct_work_budget: u64,
    /// Largest irreducible core still worth an `O(n³)` dense GTH.
    pub dense_core_max: usize,
}

impl Default for SolveOptions {
    fn default() -> Self {
        SolveOptions {
            tol: 1e-8,
            max_iters: 10_000,
            direct_work_budget: DEFAULT_DIRECT_WORK_BUDGET,
            dense_core_max: DEFAULT_DENSE_CORE_MAX,
        }
    }
}

/// Measured: sparse elimination runs at roughly 20–40 M row-merge steps per
/// second, so 2 M caps the abandoned work at a few tens of milliseconds. Trees
/// and chains finish two to three orders of magnitude under it.
pub const DEFAULT_DIRECT_WORK_BUDGET: u64 = 2_000_000;

/// Dense GTH is `O(n³)`; measured at ~5 ms for n=400 and ~40 ms for n=800, so
/// 512 keeps the direct path inside a garden render's budget. Above it, the
/// iterative solvers are both faster and (on well-connected graphs) accurate.
pub const DEFAULT_DENSE_CORE_MAX: usize = 512;

/// The comparison chain in the one form every solver consumes.
///
/// `rows[i]` holds `(j, a_ij)` for the pairwise-normalized weights, sorted by
/// `j` so that every floating-point summation in this module runs in a fixed
/// order regardless of how the caller's `HashMap` happened to iterate.
#[derive(Debug, Clone)]
pub struct RankChain {
    pub n: usize,
    pub rows: Vec<Vec<(usize, f64)>>,
    pub row_sum: Vec<f64>,
    /// Maximum unweighted degree; the uniformization constant for `P`.
    pub d_max: f64,
}

impl RankChain {
    /// Build from pairwise-normalized weights. `edges` may arrive in any order;
    /// they are sorted here so results are bit-for-bit reproducible, and
    /// repeated `(i, j)` pairs are coalesced so every row holds each column
    /// exactly once (elimination relies on that).
    pub fn from_normalized(n: usize, mut edges: Vec<((usize, usize), f64)>, d_max: usize) -> Self {
        edges.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut rows: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        for ((i, j), w) in edges {
            // Written positively so a NaN weight is dropped rather than kept.
            let usable = i < n && j < n && i != j && w > 0.0;
            if !usable {
                continue;
            }
            match rows[i].last_mut() {
                Some(last) if last.0 == j => last.1 += w,
                _ => rows[i].push((j, w)),
            }
        }
        let row_sum: Vec<f64> = rows
            .iter()
            .map(|r| r.iter().map(|&(_, w)| w).sum())
            .collect();
        RankChain {
            n,
            rows,
            row_sum,
            d_max: d_max as f64,
        }
    }

    pub fn nnz(&self) -> usize {
        self.rows.iter().map(|r| r.len()).sum()
    }

    /// Incoming edges, `cols[j] = [(i, a_ij)]`, sorted by `i`.
    fn columns(&self) -> Vec<Vec<(usize, f64)>> {
        let mut cols: Vec<Vec<(usize, f64)>> = vec![Vec::new(); self.n];
        for i in 0..self.n {
            for &(j, w) in &self.rows[i] {
                cols[j].push((i, w));
            }
        }
        cols
    }

    /// `‖πP − π‖₁` for an arbitrary vector, using the uniformized `P`.
    ///
    /// This is the yardstick every method is judged by, so it must not depend
    /// on how the candidate was produced.
    pub fn residual(&self, pi: &[f64]) -> f64 {
        let n = self.n;
        let mut next = vec![0.0f64; n];
        for i in 0..n {
            let stay = (self.d_max - self.row_sum[i]) / self.d_max;
            next[i] += pi[i] * stay;
            for &(j, w) in &self.rows[i] {
                next[j] += pi[i] * (w / self.d_max);
            }
        }
        (0..n).map(|i| (next[i] - pi[i]).abs()).sum()
    }
}

/// A `Solution` for an input with nothing to solve (no items, or one).
pub fn trivial(pi: Vec<f64>) -> Solution {
    Solution {
        log_pi: vec![0.0; pi.len()],
        pi,
        method: Method::SparseGth,
        iterations: 0,
        residual: 0.0,
        converged: true,
        underflowed: false,
    }
}

/// Normalize in place to sum 1. Returns whether the tail underflowed to zero.
fn normalize(pi: &mut [f64]) -> bool {
    let sum: f64 = pi.iter().sum();
    if sum.is_finite() && sum > 0.0 {
        for p in pi.iter_mut() {
            *p /= sum;
        }
    } else {
        let uniform = 1.0 / pi.len() as f64;
        for p in pi.iter_mut() {
            *p = uniform;
        }
        return true;
    }
    pi.iter().any(|&p| p <= 0.0)
}

fn finish(
    pi: Vec<f64>,
    chain: &RankChain,
    method: Method,
    iterations: usize,
    tol: f64,
) -> Solution {
    let log_pi = log_of(&pi);
    finish_with_logs(pi, log_pi, chain, method, iterations, tol)
}

fn log_of(pi: &[f64]) -> Vec<f64> {
    let max = pi.iter().cloned().fold(0.0f64, f64::max);
    if max <= 0.0 {
        return vec![f64::NEG_INFINITY; pi.len()];
    }
    pi.iter().map(|&p| (p / max).ln()).collect()
}

fn finish_with_logs(
    pi: Vec<f64>,
    log_pi: Vec<f64>,
    chain: &RankChain,
    method: Method,
    iterations: usize,
    tol: f64,
) -> Solution {
    let mut pi = pi;
    let underflowed = normalize(&mut pi);
    let residual = chain.residual(&pi);
    Solution {
        converged: residual.is_finite() && residual <= tol.max(f64::EPSILON * chain.n as f64),
        pi,
        log_pi,
        method,
        iterations,
        residual,
        underflowed,
    }
}

/// `ln(Σ exp(t))` computed by shifting out the largest term, so a sum of
/// astronomically different magnitudes neither overflows nor loses the small ones.
fn log_sum_exp(terms: &[f64]) -> f64 {
    let max = terms.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !max.is_finite() {
        return max;
    }
    let sum: f64 = terms.iter().map(|&t| (t - max).exp()).sum();
    max + sum.ln()
}

/// Rebuild `pi` from exact log-scores, shifted so the maximum is 1.
///
/// `exp` degrades into subnormals rather than jumping to zero, so this keeps
/// roughly 745 decades of usable spread where the linear back-substitution
/// keeps 308 — the difference between an exact ranking of a 2400-long chain and
/// an exact ranking of a 1000-long one.
fn pi_from_logs(log_pi: &[f64]) -> Vec<f64> {
    let max = log_pi.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !max.is_finite() {
        return vec![1.0 / log_pi.len() as f64; log_pi.len()];
    }
    log_pi.iter().map(|&l| (l - max).exp()).collect()
}

// ---------------------------------------------------------------------------
// Iterative solvers
// ---------------------------------------------------------------------------

/// One `π ← πP` sweep.
fn step(chain: &RankChain, pi: &[f64], next: &mut [f64]) {
    next.fill(0.0);
    for i in 0..chain.n {
        let stay = (chain.d_max - chain.row_sum[i]) / chain.d_max;
        next[i] += pi[i] * stay;
        for &(j, w) in &chain.rows[i] {
            next[j] += pi[i] * (w / chain.d_max);
        }
    }
}

/// Baseline power iteration: exactly what `ranking.rs` does today.
pub fn power(chain: &RankChain, opts: SolveOptions) -> Solution {
    let n = chain.n;
    let mut pi = vec![1.0 / n as f64; n];
    let mut next = vec![0.0f64; n];
    let mut iters = 0usize;

    for _ in 0..opts.max_iters {
        step(chain, &pi, &mut next);
        iters += 1;
        let diff: f64 = (0..n).map(|i| (pi[i] - next[i]).abs()).sum();
        pi.copy_from_slice(&next);
        if diff < opts.tol {
            break;
        }
    }
    finish(pi, chain, Method::Power, iters, opts.tol)
}

/// Power iteration with periodic componentwise Aitken Δ² extrapolation.
///
/// Aitken assumes the error is dominated by a single geometric mode, which is
/// exactly the regime that makes plain power iteration slow. The extrapolated
/// vector is only accepted when it lowers the residual, so a bad extrapolation
/// costs one wasted sweep instead of divergence.
pub fn power_aitken(chain: &RankChain, opts: SolveOptions) -> Solution {
    let n = chain.n;
    let mut x0 = vec![1.0 / n as f64; n];
    let mut x1 = vec![0.0f64; n];
    let mut x2 = vec![0.0f64; n];
    let mut cand = vec![0.0f64; n];
    let mut iters = 0usize;
    let mut best_resid = f64::INFINITY;

    while iters < opts.max_iters {
        step(chain, &x0, &mut x1);
        step(chain, &x1, &mut x2);
        iters += 2;

        let diff: f64 = (0..n).map(|i| (x2[i] - x1[i]).abs()).sum();
        if diff < opts.tol {
            return finish(x2, chain, Method::PowerAitken, iters, opts.tol);
        }

        // Aitken: x* ≈ x2 - (Δx1)² / Δ²x0, componentwise.
        let mut usable = true;
        for i in 0..n {
            let d1 = x1[i] - x0[i];
            let d2 = x2[i] - x1[i];
            let denom = d2 - d1;
            if denom.abs() <= f64::MIN_POSITIVE {
                cand[i] = x2[i];
                continue;
            }
            let v = x2[i] - d2 * d2 / denom;
            if !v.is_finite() || v < 0.0 {
                usable = false;
                break;
            }
            cand[i] = v;
        }

        std::mem::swap(&mut x0, &mut x2);
        if !usable {
            continue;
        }
        let sum: f64 = cand.iter().sum();
        if !(sum.is_finite() && sum > 0.0) {
            continue;
        }
        for c in cand.iter_mut() {
            *c /= sum;
        }
        let r_cand = chain.residual(&cand);
        let r_cur = chain.residual(&x0);
        if r_cand < r_cur && r_cand < best_resid {
            best_resid = r_cand;
            x0.copy_from_slice(&cand);
            if r_cand <= opts.tol {
                return finish(x0, chain, Method::PowerAitken, iters, opts.tol);
            }
        }
    }
    finish(x0, chain, Method::PowerAitken, iters, opts.tol)
}

/// Gauss–Seidel / SOR on the balance equations.
///
/// Balance says `π_j · Σ_k a_jk = Σ_{i≠j} π_i a_ij`, i.e. every node's outflow
/// equals its inflow. Sweeping that in place (using already-updated components
/// within the sweep) propagates information across the whole graph in one pass
/// instead of one edge per pass, which is precisely what power iteration cannot
/// do on a long chain.
pub fn sor(chain: &RankChain, opts: SolveOptions, omega: f64) -> Solution {
    let n = chain.n;
    let cols = chain.columns();
    let mut pi = vec![1.0 / n as f64; n];
    let mut iters = 0usize;

    for _ in 0..opts.max_iters {
        iters += 1;
        let mut delta = 0.0f64;
        for j in 0..n {
            let out = chain.row_sum[j];
            if out <= 0.0 {
                continue;
            }
            let inflow: f64 = cols[j].iter().map(|&(i, w)| pi[i] * w).sum();
            let gs = inflow / out;
            let updated = (1.0 - omega) * pi[j] + omega * gs;
            let updated = if updated.is_finite() && updated > 0.0 {
                updated
            } else {
                pi[j]
            };
            delta += (updated - pi[j]).abs();
            pi[j] = updated;
        }
        // The iteration is only defined up to scale; renormalize so `tol` means
        // the same thing here as it does for power iteration.
        let sum: f64 = pi.iter().sum();
        if sum.is_finite() && sum > 0.0 {
            for p in pi.iter_mut() {
                *p /= sum;
            }
        }
        if delta < opts.tol {
            break;
        }
    }
    finish(pi, chain, Method::Sor, iters, opts.tol)
}

/// Jacobi-preconditioned BiCGSTAB on `Qᵀπ = 0` with row 0 replaced by `Σπ = 1`.
///
/// Included as the Krylov representative. GMRES/Arnoldi would need restart
/// bookkeeping and a stored basis for the same job; BiCGSTAB gives the same
/// short-recurrence answer for a nonsymmetric operator at fixed memory.
pub fn bicgstab(chain: &RankChain, opts: SolveOptions) -> Solution {
    let n = chain.n;
    let cols = chain.columns();

    // A x = b where row 0 is Σx = 1 and row j>0 is (inflow - outflow) at j.
    let apply = |x: &[f64], out: &mut [f64]| {
        out[0] = x.iter().sum();
        for j in 1..n {
            let inflow: f64 = cols[j].iter().map(|&(i, w)| x[i] * w).sum();
            out[j] = inflow - x[j] * chain.row_sum[j];
        }
    };
    // Jacobi preconditioner: the diagonal of that operator.
    let diag: Vec<f64> = (0..n)
        .map(|j| {
            let d = if j == 0 { 1.0 } else { -chain.row_sum[j] };
            if d.abs() < 1e-300 {
                1.0
            } else {
                d
            }
        })
        .collect();
    let precond = |v: &[f64], out: &mut [f64]| {
        for i in 0..n {
            out[i] = v[i] / diag[i];
        }
    };

    let mut b = vec![0.0f64; n];
    b[0] = 1.0;

    let mut x = vec![1.0 / n as f64; n];
    let mut ax = vec![0.0f64; n];
    apply(&x, &mut ax);
    let mut r: Vec<f64> = (0..n).map(|i| b[i] - ax[i]).collect();
    let r_hat = r.clone();

    let mut rho = 1.0f64;
    let mut alpha = 1.0f64;
    let mut omega = 1.0f64;
    let mut v = vec![0.0f64; n];
    let mut p = vec![0.0f64; n];
    let mut y = vec![0.0f64; n];
    let mut z = vec![0.0f64; n];
    let mut s = vec![0.0f64; n];
    let mut t = vec![0.0f64; n];
    let dot = |a: &[f64], b: &[f64]| -> f64 { (0..a.len()).map(|i| a[i] * b[i]).sum() };
    let bnorm = 1.0f64;
    let mut iters = 0usize;

    for _ in 0..opts.max_iters {
        iters += 1;
        let rho_new = dot(&r_hat, &r);
        if rho_new.abs() < 1e-300 {
            break;
        }
        let beta = (rho_new / rho) * (alpha / omega);
        rho = rho_new;
        for i in 0..n {
            p[i] = r[i] + beta * (p[i] - omega * v[i]);
        }
        precond(&p, &mut y);
        apply(&y, &mut v);
        let denom = dot(&r_hat, &v);
        if denom.abs() < 1e-300 {
            break;
        }
        alpha = rho / denom;
        for i in 0..n {
            s[i] = r[i] - alpha * v[i];
        }
        if dot(&s, &s).sqrt() / bnorm < opts.tol * 1e-2 {
            for i in 0..n {
                x[i] += alpha * y[i];
            }
            break;
        }
        precond(&s, &mut z);
        apply(&z, &mut t);
        let tt = dot(&t, &t);
        if tt.abs() < 1e-300 {
            break;
        }
        omega = dot(&t, &s) / tt;
        for i in 0..n {
            x[i] += alpha * y[i] + omega * z[i];
            r[i] = s[i] - omega * t[i];
        }
        if dot(&r, &r).sqrt() / bnorm < opts.tol * 1e-2 {
            break;
        }
        if omega.abs() < 1e-300 {
            break;
        }
    }

    // BiCGSTAB has no sign constraint; clamp before normalizing.
    for xi in x.iter_mut() {
        if !xi.is_finite() || *xi < 0.0 {
            *xi = 0.0;
        }
    }
    finish(x, chain, Method::BiCgStab, iters, opts.tol)
}

// ---------------------------------------------------------------------------
// Direct solvers
// ---------------------------------------------------------------------------

/// Dense LU with partial pivoting on the balance system.
///
/// `πQ = 0` is rank `n-1`, so equation 0 is replaced by the normalization
/// `Σπ = 1`, giving a nonsingular `n×n` system solved in `O(n³)`.
pub fn dense_lu(chain: &RankChain, tol: f64) -> Solution {
    let n = chain.n;
    // Row-major A, where A[j][i] is the coefficient of π_i in equation j.
    let mut a = vec![0.0f64; n * n];
    for v in a.iter_mut().take(n) {
        *v = 1.0; // equation 0: Σ π_i = 1
    }
    for i in 0..n {
        for &(j, w) in &chain.rows[i] {
            if j != 0 {
                a[j * n + i] += w; // inflow to j from i
            }
        }
        if i != 0 {
            a[i * n + i] -= chain.row_sum[i]; // outflow from i
        }
    }
    let mut b = vec![0.0f64; n];
    b[0] = 1.0;

    // Gaussian elimination, partial pivoting.
    let mut perm: Vec<usize> = (0..n).collect();
    for k in 0..n {
        let mut piv = k;
        let mut best = a[perm[k] * n + k].abs();
        for r in (k + 1)..n {
            let v = a[perm[r] * n + k].abs();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if best == 0.0 {
            continue;
        }
        perm.swap(k, piv);
        let pk = perm[k];
        let pivot = a[pk * n + k];
        for r in (k + 1)..n {
            let pr = perm[r];
            let f = a[pr * n + k] / pivot;
            if f == 0.0 {
                continue;
            }
            a[pr * n + k] = 0.0;
            for c in (k + 1)..n {
                a[pr * n + c] -= f * a[pk * n + c];
            }
            b[pr] -= f * b[pk];
        }
    }
    let mut x = vec![0.0f64; n];
    for k in (0..n).rev() {
        let pk = perm[k];
        let mut acc = b[pk];
        for c in (k + 1)..n {
            acc -= a[pk * n + c] * x[c];
        }
        let d = a[pk * n + k];
        x[k] = if d.abs() > 0.0 { acc / d } else { 0.0 };
    }
    for xi in x.iter_mut() {
        if !xi.is_finite() || *xi < 0.0 {
            *xi = 0.0;
        }
    }
    finish(x, chain, Method::DenseLu, 0, tol)
}

/// Rescale threshold for GTH back-substitution.
///
/// Back-substitution accumulates `π` in *unnormalized* form, and on a strongly
/// ordered chain the values grow geometrically. Rescaling the partial vector
/// whenever it nears the top of the f64 range keeps every ratio exact (they are
/// all defined up to one global scalar) and avoids returning `inf`.
const GTH_RESCALE_ABOVE: f64 = 1e250;

/// Dense Grassmann–Taksar–Heyman state reduction on a compact rate matrix.
///
/// Every operation is an addition of non-negative numbers or a division by a
/// positive sum: there is no subtraction anywhere, so there is no cancellation
/// and no pivoting is needed. That is why GTH is the reference implementation
/// here even though dense LU costs the same `O(n³)`.
///
/// Returns `ln π` shifted so the largest entry is 0. Finishing in logs matters
/// because the answer itself can span more than f64's range.
fn gth_dense_logs(n: usize, rows: &[Vec<(usize, f64)>]) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![0.0];
    }
    let mut a = vec![0.0f64; n * n];
    for (i, row) in rows.iter().enumerate().take(n) {
        for &(j, w) in row {
            if j < n && j != i {
                a[i * n + j] += w;
            }
        }
    }

    for k in (1..n).rev() {
        let s: f64 = (0..k).map(|j| a[k * n + j]).sum();
        let reaches_survivors = s > 0.0;
        if !reaches_survivors {
            // State k cannot reach the surviving block, so it feeds no mass
            // back into it.
            for i in 0..k {
                a[i * n + k] = 0.0;
            }
            continue;
        }
        for i in 0..k {
            let f = a[i * n + k] / s;
            a[i * n + k] = f;
            if f == 0.0 {
                continue;
            }
            for j in 0..k {
                a[i * n + j] += f * a[k * n + j];
            }
        }
    }

    // Cheap path: accumulate linearly, take logs at the end.
    let mut x = vec![0.0f64; n];
    x[0] = 1.0;
    let mut rescaled = false;
    for k in 1..n {
        let mut acc = 0.0f64;
        for i in 0..k {
            acc += x[i] * a[i * n + k];
        }
        x[k] = acc;
        if acc > GTH_RESCALE_ABOVE {
            let inv = 1.0 / acc;
            for v in x.iter_mut().take(k + 1) {
                *v *= inv;
            }
            rescaled = true;
        }
    }
    if !rescaled {
        return shift_logs(x.iter().map(|&v| v.ln()).collect());
    }

    // The span overflowed a single f64 vector, so redo the accumulation in log
    // space. The elimination coefficients are unchanged; only the sum differs.
    let mut logx = vec![f64::NEG_INFINITY; n];
    logx[0] = 0.0;
    let mut terms: Vec<f64> = Vec::with_capacity(n);
    for k in 1..n {
        terms.clear();
        for i in 0..k {
            let c = a[i * n + k];
            if c > 0.0 && logx[i].is_finite() {
                terms.push(logx[i] + c.ln());
            }
        }
        logx[k] = log_sum_exp(&terms);
    }
    shift_logs(logx)
}

/// Dense GTH over the whole chain. `O(n³)` time, `O(n²)` memory.
pub fn dense_gth(chain: &RankChain, tol: f64) -> Solution {
    let logs = gth_dense_logs(chain.n, &chain.rows);
    let pi = pi_from_logs(&logs);
    finish_with_logs(pi, logs, chain, Method::DenseGth, 0, tol)
}

/// Shift log-scores so the maximum is exactly 0.
fn shift_logs(mut logs: Vec<f64>) -> Vec<f64> {
    let max = logs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if max.is_finite() {
        for l in logs.iter_mut() {
            *l -= max;
        }
    }
    logs
}

/// Outcome of attempting the sparse direct path.
pub enum SparseGthOutcome {
    Solved(Solution),
    /// Elimination could not finish cheaply: states were left with degree above
    /// the guard, or the work budget ran out. Fall back to an iterative solver.
    TooDense {
        core: usize,
        work: u64,
    },
}

/// `μ = m - n + c` over the symmetrized comparison graph: the number of
/// independent cycles, i.e. how far the graph is from being a forest. `O(n + m)`.
fn cyclomatic_number(chain: &RankChain) -> usize {
    let n = chain.n;
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    let mut edges = 0usize;
    let mut merges = 0usize;
    for i in 0..n {
        for &(j, _) in &chain.rows[i] {
            // Count each compared pair once: the reverse arc exists whenever
            // both sides got votes, and the pair is an edge either way.
            if j < i && chain.rows[j].binary_search_by(|p| p.0.cmp(&i)).is_ok() {
                continue;
            }
            edges += 1;
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri != rj {
                parent[ri] = rj;
                merges += 1;
            }
        }
    }
    // c = n - merges, so μ = m - n + c = m - merges.
    edges.saturating_sub(merges)
}

/// Above this live degree, eliminating a state creates more fill than it
/// removes. Trees, chains, series-parallel graphs and most hand-built
/// ontologies reduce away completely under this guard; random graphs stall
/// almost immediately, which is exactly the signal to go iterative.
const MAX_ELIM_DEGREE: usize = 12;

/// GTH state reduction over the sparse graph, eliminating minimum-degree states first.
///
/// Elimination on a graph is fill-bounded: removing a state connects its
/// surviving in-neighbours to its surviving out-neighbours. A chain — or any
/// tree — has an elimination order with *zero* fill, so this is `O(n)` on
/// exactly the topology where power iteration needs `Θ(n²)` sweeps. The two
/// failure modes are complementary: what stalls here (dense, well-connected
/// graphs) is what power iteration finishes in a few dozen sweeps.
///
/// States that cannot be eliminated cheaply form an irreducible *core*, handed
/// to dense GTH when it is small enough and reported as `TooDense` otherwise.
pub fn sparse_gth(chain: &RankChain, opts: SolveOptions, tol: f64) -> SparseGthOutcome {
    let n = chain.n;
    if n == 1 {
        return SparseGthOutcome::Solved(finish(vec![1.0], chain, Method::SparseGth, 0, tol));
    }

    // Predict the core before doing any work. A graph whose every vertex has
    // degree ≥ 3 has at most `2μ - 2` vertices, where `μ = m - n + c` is the
    // cyclomatic number, so `2μ` bounds what elimination can leave behind. A
    // forest has `μ = 0` and reduces to nothing; a random graph has `μ ≈ m` and
    // reduces to almost nothing *but itself*, which is the case worth skipping
    // before paying for it.
    if n > opts.dense_core_max && 2 * cyclomatic_number(chain) > opts.dense_core_max {
        return SparseGthOutcome::TooDense { core: n, work: 0 };
    }

    // Live out-rates, plus in-neighbour lists that may carry stale or duplicate
    // entries — they are compacted against `alive` when a state is popped.
    let mut out: Vec<Vec<(usize, f64)>> = chain.rows.clone();
    let mut inn: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        for &(j, _) in &chain.rows[i] {
            inn[j].push(i);
        }
    }

    let mut alive = vec![true; n];
    // Position of each column in the row currently being updated, -1 when
    // absent. Turns a row merge into O(len) instead of a map lookup per entry.
    let mut slot: Vec<i64> = vec![-1; n];
    let mut recovery: Vec<(usize, Vec<(usize, f64)>)> = Vec::with_capacity(n);
    let mut work: u64 = 0;

    let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(usize, usize)>> =
        std::collections::BinaryHeap::new();
    for k in 0..n {
        heap.push(std::cmp::Reverse((out[k].len() + inn[k].len(), k)));
    }

    let mut remaining = n;
    while remaining > 1 {
        let Some(std::cmp::Reverse((d, k))) = heap.pop() else {
            break;
        };
        if !alive[k] {
            continue;
        }

        // Compact k's neighbour lists, then re-check the heap key against the
        // true degree: a stale key means another elimination has changed it.
        out[k].retain(|&(j, w)| alive[j] && w > 0.0);
        inn[k].sort_unstable();
        inn[k].dedup();
        inn[k].retain(|&i| alive[i] && i != k);
        let cur = out[k].len() + inn[k].len();
        if cur != d {
            heap.push(std::cmp::Reverse((cur, k)));
            continue;
        }
        if out[k].len().max(inn[k].len()) > MAX_ELIM_DEGREE {
            // The cheapest remaining state is already expensive, so every other
            // one is too. What is left is the core.
            break;
        }

        let s: f64 = out[k].iter().map(|&(_, w)| w).sum();
        let in_list = std::mem::take(&mut inn[k]);
        let out_list = std::mem::take(&mut out[k]);

        let mut coeffs: Vec<(usize, f64)> = Vec::with_capacity(in_list.len());
        for &i in &in_list {
            for (p, &(j, _)) in out[i].iter().enumerate() {
                slot[j] = p as i64;
            }
            work = work.saturating_add(out[i].len() as u64 + out_list.len() as u64);

            let w_ik = if slot[k] >= 0 {
                let p = slot[k] as usize;
                let v = out[i][p].1;
                out[i][p].1 = 0.0;
                v
            } else {
                0.0
            };
            let f = if s > 0.0 { w_ik / s } else { 0.0 };
            if f > 0.0 {
                coeffs.push((i, f));
                for &(j, r) in &out_list {
                    if j == i {
                        continue;
                    }
                    if slot[j] >= 0 {
                        out[i][slot[j] as usize].1 += f * r;
                    } else {
                        slot[j] = out[i].len() as i64;
                        out[i].push((j, f * r));
                        inn[j].push(i);
                    }
                }
            }
            for &(j, _) in out[i].iter() {
                slot[j] = -1;
            }
            slot[k] = -1;
            out[i].retain(|&(j, w)| j != k && w > 0.0);
        }

        alive[k] = false;
        remaining -= 1;
        recovery.push((k, coeffs));

        for &i in &in_list {
            if alive[i] {
                heap.push(std::cmp::Reverse((out[i].len() + inn[i].len(), i)));
            }
        }
        for &(j, _) in &out_list {
            if alive[j] {
                heap.push(std::cmp::Reverse((out[j].len() + inn[j].len(), j)));
            }
        }

        if work > opts.direct_work_budget {
            return SparseGthOutcome::TooDense {
                core: (0..n).filter(|&i| alive[i]).count(),
                work,
            };
        }
    }

    let core: Vec<usize> = (0..n).filter(|&i| alive[i]).collect();
    if core.len() > opts.dense_core_max {
        return SparseGthOutcome::TooDense {
            core: core.len(),
            work,
        };
    }

    // Solve the irreducible core densely, then unwind the eliminated states.
    let mut compact = vec![usize::MAX; n];
    for (c, &g) in core.iter().enumerate() {
        compact[g] = c;
    }
    let core_rows: Vec<Vec<(usize, f64)>> = core
        .iter()
        .map(|&g| {
            let mut row: Vec<(usize, f64)> = out[g]
                .iter()
                .filter(|&&(j, w)| alive[j] && w > 0.0)
                .map(|&(j, w)| (compact[j], w))
                .collect();
            row.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            row
        })
        .collect();

    let core_logs = gth_dense_logs(core.len(), &core_rows);
    let mut logx = vec![f64::NEG_INFINITY; n];
    for (c, &g) in core.iter().enumerate() {
        logx[g] = core_logs[c];
    }
    let mut terms: Vec<f64> = Vec::new();
    for (k, coeffs) in recovery.iter().rev() {
        terms.clear();
        for &(i, f) in coeffs {
            if f > 0.0 && logx[i].is_finite() {
                terms.push(logx[i] + f.ln());
            }
        }
        logx[*k] = log_sum_exp(&terms);
    }

    let shifted = shift_logs(logx);
    let pi = pi_from_logs(&shifted);
    SparseGthOutcome::Solved(finish_with_logs(
        pi,
        shifted,
        chain,
        Method::SparseGth,
        0,
        tol,
    ))
}

// ---------------------------------------------------------------------------
// Recommended strategy
// ---------------------------------------------------------------------------

/// Solve for π using the hybrid strategy: sparse direct first, iterative only
/// when the direct path is genuinely too expensive.
///
/// This never returns an unconverged answer without saying so — check
/// [`Solution::converged`].
pub fn solve(chain: &RankChain, opts: SolveOptions) -> Solution {
    if chain.n == 0 {
        return Solution {
            pi: vec![],
            log_pi: vec![],
            method: Method::SparseGth,
            iterations: 0,
            residual: 0.0,
            converged: true,
            underflowed: false,
        };
    }
    if chain.n == 1 {
        return Solution {
            pi: vec![1.0],
            log_pi: vec![0.0],
            method: Method::SparseGth,
            iterations: 0,
            residual: 0.0,
            converged: true,
            underflowed: false,
        };
    }
    if chain.nnz() == 0 {
        let n = chain.n;
        return Solution {
            pi: vec![1.0 / n as f64; n],
            log_pi: vec![0.0; n],
            method: Method::SparseGth,
            iterations: 0,
            residual: 0.0,
            converged: true,
            underflowed: false,
        };
    }

    // A chain over several disconnected components has no unique stationary
    // distribution: any split of mass between them is stationary. Power
    // iteration from a uniform start picks the one where each component holds
    // its share of the nodes, so reproduce that explicitly instead of letting a
    // direct solver pick one component and zero the rest.
    let comps = weak_components(chain);
    if comps.len() > 1 {
        return solve_by_component(chain, &comps, opts);
    }
    solve_connected(chain, opts)
}

/// Weakly connected components of the comparison graph, each sorted ascending.
fn weak_components(chain: &RankChain) -> Vec<Vec<usize>> {
    let n = chain.n;
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        for &(j, _) in &chain.rows[i] {
            adj[i].push(j);
            adj[j].push(i);
        }
    }
    let mut seen = vec![false; n];
    let mut comps = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut comp = Vec::new();
        while let Some(x) = stack.pop() {
            comp.push(x);
            for &y in &adj[x] {
                if !seen[y] {
                    seen[y] = true;
                    stack.push(y);
                }
            }
        }
        comp.sort_unstable();
        comps.push(comp);
    }
    comps
}

fn solve_by_component(chain: &RankChain, comps: &[Vec<usize>], opts: SolveOptions) -> Solution {
    let n = chain.n;
    let mut log_pi = vec![f64::NEG_INFINITY; n];
    let mut converged = true;
    let mut iterations = 0usize;
    let mut method = Method::SparseGth;

    for comp in comps {
        let mut compact = vec![usize::MAX; n];
        for (c, &g) in comp.iter().enumerate() {
            compact[g] = c;
        }
        let rows: Vec<Vec<(usize, f64)>> = comp
            .iter()
            .map(|&g| {
                chain.rows[g]
                    .iter()
                    .filter(|&&(j, _)| compact[j] != usize::MAX)
                    .map(|&(j, w)| (compact[j], w))
                    .collect()
            })
            .collect();
        let row_sum = rows
            .iter()
            .map(|r| r.iter().map(|&(_, w)| w).sum())
            .collect();
        let sub = RankChain {
            n: comp.len(),
            rows,
            row_sum,
            d_max: chain.d_max,
        };
        let sol = solve_connected(&sub, opts);
        converged &= sol.converged;
        iterations = iterations.max(sol.iterations);
        if !sol.method.is_direct() {
            method = sol.method;
        }
        // Each component keeps the share of the mass its node count started
        // with, spread internally by its own stationary distribution.
        let share = (comp.len() as f64 / n as f64).ln() - log_sum_exp(&sol.log_pi);
        for (c, &g) in comp.iter().enumerate() {
            log_pi[g] = sol.log_pi[c] + share;
        }
    }

    let shifted = shift_logs(log_pi);
    let pi = pi_from_logs(&shifted);
    let mut out = finish_with_logs(pi, shifted, chain, method, iterations, opts.tol);
    // One component missing tolerance condemns the whole vector, even if the
    // combined residual happens to look small next to the dominant component.
    out.converged &= converged;
    out
}

fn solve_connected(chain: &RankChain, opts: SolveOptions) -> Solution {
    match sparse_gth(chain, opts, opts.tol) {
        SparseGthOutcome::Solved(sol) if sol.converged => return sol,
        // A direct solve that misses tolerance means the conditioning is worse
        // than the elimination could handle; let the iterative path try rather
        // than hand back an answer nothing has checked.
        SparseGthOutcome::Solved(_) | SparseGthOutcome::TooDense { .. } => {}
    }

    // Gauss–Seidel first: on every graph dense enough to reach this branch it
    // converged in tens of sweeps and beat power iteration on both time and
    // residual. Power iteration stays as the second opinion.
    let sweeps = sor(chain, opts, 1.0);
    if sweeps.converged {
        return sweeps;
    }
    let pow = power_aitken(chain, opts);
    if pow.converged || pow.residual < sweeps.residual {
        pow
    } else {
        sweeps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f64 = 1e-8;

    fn opts() -> SolveOptions {
        SolveOptions::default()
    }

    /// Build a chain the way production does: raw directed vote weights through
    /// `ranking::chain_from_edges`. `votes` are `(winner, loser, w_win, w_lose)`;
    /// Rank Centrality walks *toward* the winner, so the loser's row carries the
    /// weight (see `GroupState::apply_vote`).
    fn chain_from_votes(n: usize, votes: &[(usize, usize, f64, f64)]) -> RankChain {
        let mut raw: std::collections::HashMap<(usize, usize), f64> = Default::default();
        for &(w, l, ww, wl) in votes {
            *raw.entry((l, w)).or_insert(0.0) += ww;
            *raw.entry((w, l)).or_insert(0.0) += wl;
        }
        crate::ranking::chain_from_edges(n, raw.into_iter())
    }

    fn path_graph(n: usize, ratio: f64) -> RankChain {
        let votes: Vec<(usize, usize, f64, f64)> =
            (0..n - 1).map(|i| (i, i + 1, ratio, 1.0)).collect();
        chain_from_votes(n, &votes)
    }

    fn star_graph(n: usize) -> RankChain {
        let votes: Vec<(usize, usize, f64, f64)> =
            (1..n).map(|i| (0, i, 2.0 + (i % 5) as f64, 1.0)).collect();
        chain_from_votes(n, &votes)
    }

    fn clique_graph(n: usize) -> RankChain {
        let mut votes = Vec::new();
        let mut k = 0;
        for i in 0..n {
            for j in (i + 1)..n {
                votes.push((i, j, 2.0 + (k % 5) as f64, 1.0));
                k += 1;
            }
        }
        chain_from_votes(n, &votes)
    }

    fn sparse_graph(n: usize, degree: usize) -> RankChain {
        let mut state = 0x00C0_FFEEu64;
        let mut next = || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        let mut votes: Vec<(usize, usize, f64, f64)> =
            (0..n - 1).map(|i| (i, i + 1, 2.0, 1.0)).collect();
        for k in 0..(degree.saturating_sub(2) * n / 2) {
            let a = (next() % n as u64) as usize;
            let b = (next() % n as u64) as usize;
            if a != b {
                votes.push((a, b, 2.0 + (k % 5) as f64, 1.0));
            }
        }
        chain_from_votes(n, &votes)
    }

    fn max_rel_error(got: &[f64], want: &[f64]) -> f64 {
        (0..want.len())
            .filter(|&i| want[i] > 1e-280)
            .map(|i| (got[i] - want[i]).abs() / want[i])
            .fold(0.0f64, f64::max)
    }

    fn force_sparse(chain: &RankChain) -> Solution {
        let unlimited = SolveOptions {
            direct_work_budget: u64::MAX,
            dense_core_max: usize::MAX,
            ..opts()
        };
        match sparse_gth(chain, unlimited, TOL) {
            SparseGthOutcome::Solved(s) => s,
            SparseGthOutcome::TooDense { .. } => panic!("uncapped elimination gave up"),
        }
    }

    // -----------------------------------------------------------------
    // Closed forms: these pin the answer without trusting any solver.
    // -----------------------------------------------------------------

    /// A tree is reversible, so detailed balance fixes π edge by edge:
    /// `π_i / π_j = a_ji / a_ij`. For a path at a constant ratio that is a
    /// geometric sequence, known exactly.
    #[test]
    fn path_matches_detailed_balance_closed_form() {
        for n in [2usize, 3, 8, 64, 300] {
            let chain = path_graph(n, 3.0);
            let mut want: Vec<f64> = (0..n).map(|i| 3f64.powi(-(i as i32))).collect();
            let sum: f64 = want.iter().sum();
            for w in want.iter_mut() {
                *w /= sum;
            }
            for sol in [
                force_sparse(&chain),
                dense_gth(&chain, TOL),
                dense_lu(&chain, TOL),
                solve(&chain, opts()),
            ] {
                assert!(
                    max_rel_error(&sol.pi, &want) < 1e-12,
                    "{} wrong on path n={n}: {:?}",
                    sol.method.label(),
                    &sol.pi[..4.min(n)]
                );
                assert!(sol.converged, "{} not converged", sol.method.label());
            }
        }
    }

    /// The 3-cycle a>b>c>a is symmetric under rotation, so π must be uniform.
    /// This is the `test/fixtures/ranking/cycle.sorter` anchor.
    #[test]
    fn three_cycle_is_exactly_uniform() {
        let chain = chain_from_votes(3, &[(0, 1, 2.0, 1.0), (1, 2, 2.0, 1.0), (2, 0, 2.0, 1.0)]);
        let sol = solve(&chain, opts());
        for (i, &p) in sol.pi.iter().enumerate() {
            assert!(
                (p - 1.0 / 3.0).abs() < 1e-14,
                "node {i} got {p}, expected 1/3"
            );
        }
    }

    /// Issue #146: a pure forward star at 2:1. The hub beat both spokes, so it
    /// must rank first — and by detailed balance on this tree, exactly 2:1:1.
    #[test]
    fn star_regression_issue_146() {
        let chain = chain_from_votes(3, &[(0, 1, 2.0, 1.0), (0, 2, 2.0, 1.0)]);
        let sol = solve(&chain, opts());
        assert!((sol.pi[0] - 0.5).abs() < 1e-14, "hub: {}", sol.pi[0]);
        assert!((sol.pi[1] - 0.25).abs() < 1e-14, "spoke a: {}", sol.pi[1]);
        assert!((sol.pi[2] - 0.25).abs() < 1e-14, "spoke b: {}", sol.pi[2]);
    }

    /// A single comparison at ratio r:1 must give exactly r:1.
    #[test]
    fn single_pair_is_the_ratio() {
        for r in [1.0f64, 2.0, 3.0, 7.0] {
            let chain = chain_from_votes(2, &[(0, 1, r, 1.0)]);
            let sol = solve(&chain, opts());
            let want = r / (r + 1.0);
            assert!(
                (sol.pi[0] - want).abs() < 1e-15,
                "ratio {r}: got {:?}",
                sol.pi
            );
        }
    }

    // -----------------------------------------------------------------
    // Cross-method equivalence
    // -----------------------------------------------------------------

    #[test]
    fn all_methods_agree_on_well_conditioned_graphs() {
        let cases: Vec<(&str, RankChain)> = vec![
            ("star-64", star_graph(64)),
            ("clique-40", clique_graph(40)),
            ("sparse-d6-120", sparse_graph(120, 6)),
            ("path-40", path_graph(40, 2.0)),
        ];
        let tight = SolveOptions {
            tol: 1e-14,
            max_iters: 500_000,
            ..opts()
        };
        for (label, chain) in cases {
            let reference = dense_gth(&chain, TOL).pi;
            for sol in [
                force_sparse(&chain),
                dense_lu(&chain, TOL),
                sor(&chain, tight, 1.0),
                power(&chain, tight),
                bicgstab(&chain, tight),
                solve(&chain, opts()),
            ] {
                // On a path the scores span 2^39, so an iterative method that
                // has driven the *absolute* residual to 1e-14 is still far off
                // in relative terms at the tail. Only the direct methods get
                // full relative precision there; that gap is the whole point.
                let rel = max_rel_error(&sol.pi, &reference);
                let bound = if label == "path-40" && !sol.method.is_direct() {
                    1e-2
                } else {
                    1e-9
                };
                assert!(
                    rel < bound,
                    "{label}: {} disagrees with dense GTH by {rel:e}",
                    sol.method.label()
                );
            }
        }
    }

    /// Every solver's own residual must match what an independent recomputation
    /// says, so `Solution::residual` can be trusted as the acceptance criterion.
    #[test]
    fn reported_residual_matches_recomputation() {
        let chain = sparse_graph(200, 6);
        for sol in [
            solve(&chain, opts()),
            power(&chain, opts()),
            sor(&chain, opts(), 1.0),
            dense_gth(&chain, TOL),
        ] {
            let recomputed = chain.residual(&sol.pi);
            assert!(
                (recomputed - sol.residual).abs() <= 1e-18 + recomputed * 1e-9,
                "{}: reported {:e} vs recomputed {recomputed:e}",
                sol.method.label(),
                sol.residual
            );
        }
    }

    // -----------------------------------------------------------------
    // No silent non-convergence
    // -----------------------------------------------------------------

    #[test]
    fn power_iteration_reports_its_own_failure() {
        let chain = path_graph(1500, 2.0);
        let capped = SolveOptions {
            max_iters: 50,
            ..opts()
        };
        let sol = power(&chain, capped);
        assert!(!sol.converged, "50 sweeps cannot solve a 1500-node path");
        assert_eq!(sol.iterations, 50, "should have used the whole budget");
        assert!(sol.residual > TOL);
    }

    /// The same input the capped power iteration fails on must come back solved
    /// through the hybrid, with the flag set honestly.
    #[test]
    fn hybrid_converges_where_power_iteration_cannot() {
        for n in [1500usize, 4000] {
            let chain = path_graph(n, 2.0);
            let pow = power(&chain, opts());
            assert!(
                !pow.converged,
                "n={n}: power iteration was expected to hit the 10k cap"
            );

            let sol = solve(&chain, opts());
            assert!(sol.converged, "n={n}: hybrid failed");
            assert_eq!(sol.method, Method::SparseGth);
            assert!(sol.residual < 1e-12, "n={n}: residual {:e}", sol.residual);
        }
    }

    /// On a path the true ranking is the construction order. `pi` runs out of
    /// exponent range past ~1000 nodes, but `log_pi` must stay strictly ordered
    /// all the way down — that is what makes it the right sort key.
    #[test]
    fn long_path_ranking_is_strictly_ordered_in_log_space() {
        let n = 3000;
        let chain = path_graph(n, 2.0);
        let sol = solve(&chain, opts());
        assert!(sol.converged);
        assert!(
            sol.underflowed,
            "a 3000-node path at 2:1 spans 10^903 and must report underflow"
        );
        for i in 0..n - 1 {
            assert!(
                sol.log_pi[i] > sol.log_pi[i + 1],
                "log scores tie or invert at {i}: {} vs {}",
                sol.log_pi[i],
                sol.log_pi[i + 1]
            );
        }
        // Every adjacent step is exactly ln 2 apart (detailed balance).
        for i in 0..n - 1 {
            let step = sol.log_pi[i] - sol.log_pi[i + 1];
            assert!(
                (step - 2f64.ln()).abs() < 1e-9,
                "step at {i} is {step}, expected ln 2"
            );
        }
    }

    // -----------------------------------------------------------------
    // Structure
    // -----------------------------------------------------------------

    /// A chain over disconnected components has infinitely many stationary
    /// distributions. Power iteration from uniform picks the one where each
    /// component keeps its share of the nodes; the solver must pick the same
    /// one, or whole-group rankings would silently change meaning.
    #[test]
    fn disjoint_components_split_mass_by_size() {
        let chain = chain_from_votes(4, &[(0, 1, 3.0, 1.0), (2, 3, 2.0, 1.0)]);
        let sol = solve(&chain, opts());
        assert!(sol.pi.iter().all(|p| p.is_finite()));
        // Each pair holds 1/2 of the mass, split 3:1 and 2:1 internally.
        for (i, want) in [(0, 0.375), (1, 0.125), (2, 1.0 / 3.0), (3, 1.0 / 6.0)] {
            assert!(
                (sol.pi[i] - want).abs() < 1e-14,
                "node {i}: got {} want {want}",
                sol.pi[i]
            );
        }

        // And that is what unbounded power iteration converges to.
        let reference = power(
            &chain,
            SolveOptions {
                tol: 1e-15,
                max_iters: 200_000,
                ..opts()
            },
        );
        assert!(max_rel_error(&sol.pi, &reference.pi) < 1e-9);
    }

    /// A lone item with no comparisons keeps a `1/n` share, exactly as the
    /// uniform-start power iteration leaves it.
    #[test]
    fn isolated_nodes_keep_a_uniform_share() {
        let chain = chain_from_votes(3, &[(0, 1, 3.0, 1.0)]);
        let sol = solve(&chain, opts());
        assert!(
            (sol.pi[2] - 1.0 / 3.0).abs() < 1e-14,
            "isolate: {}",
            sol.pi[2]
        );
        assert!((sol.pi[0] - 0.5).abs() < 1e-14, "winner: {}", sol.pi[0]);
        assert!(
            (sol.pi[1] - 1.0 / 6.0).abs() < 1e-14,
            "loser: {}",
            sol.pi[1]
        );
    }

    #[test]
    fn cyclomatic_number_counts_independent_cycles() {
        assert_eq!(
            cyclomatic_number(&path_graph(50, 2.0)),
            0,
            "a path is a tree"
        );
        assert_eq!(cyclomatic_number(&star_graph(50)), 0, "a star is a tree");
        let triangle = chain_from_votes(3, &[(0, 1, 2.0, 1.0), (1, 2, 2.0, 1.0), (2, 0, 2.0, 1.0)]);
        assert_eq!(cyclomatic_number(&triangle), 1);
        // K_n has n(n-1)/2 edges and n-1 spanning-tree edges.
        assert_eq!(cyclomatic_number(&clique_graph(6)), 15 - 5);
    }

    #[test]
    fn hybrid_picks_direct_for_trees_and_iterative_for_dense_graphs() {
        assert_eq!(
            solve(&path_graph(4000, 2.0), opts()).method,
            Method::SparseGth
        );
        assert_eq!(solve(&clique_graph(60), opts()).method, Method::SparseGth);
        let big_sparse = solve(&sparse_graph(2000, 6), opts());
        assert_ne!(big_sparse.method, Method::SparseGth);
        assert!(big_sparse.converged);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let empty = RankChain::from_normalized(0, vec![], 0);
        assert!(solve(&empty, opts()).pi.is_empty());

        let single = RankChain::from_normalized(1, vec![], 0);
        assert_eq!(solve(&single, opts()).pi, vec![1.0]);

        // Nodes with no comparisons at all: uniform, and no division by zero.
        let isolated = RankChain::from_normalized(5, vec![], 0);
        let sol = solve(&isolated, opts());
        assert!(sol.pi.iter().all(|&p| (p - 0.2).abs() < 1e-15));

        // A unanimous edge (the loser never scored) leaves one direction empty.
        let unanimous = chain_from_votes(2, &[(0, 1, 1.0, 0.0)]);
        let sol = solve(&unanimous, opts());
        assert!(sol.pi[0] > sol.pi[1], "{:?}", sol.pi);
        assert!(sol.pi.iter().all(|p| p.is_finite()));
    }

    // -----------------------------------------------------------------
    // Determinism
    // -----------------------------------------------------------------

    /// The same graph presented in a different edge order must give a
    /// bit-identical answer — `RankChain::from_normalized` sorts precisely so
    /// that `HashMap` iteration order cannot leak into the low bits.
    #[test]
    fn edge_input_order_does_not_change_the_result() {
        let n = 300;
        let base: Vec<((usize, usize), f64)> = {
            let mut v = Vec::new();
            for i in 0..n - 1 {
                v.push(((i + 1, i), 2.0 / 3.0));
                v.push(((i, i + 1), 1.0 / 3.0));
            }
            for i in 0..n / 3 {
                v.push(((i, (7 * i + 11) % n), 0.5));
                v.push((((7 * i + 11) % n, i), 0.5));
            }
            v
        };
        let reference = solve(&RankChain::from_normalized(n, base.clone(), 6), opts()).pi;

        let mut state = 12345u64;
        for _ in 0..6 {
            let mut shuffled = base.clone();
            for i in (1..shuffled.len()).rev() {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let j = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) % (i as u64 + 1)) as usize;
                shuffled.swap(i, j);
            }
            let got = solve(&RankChain::from_normalized(n, shuffled, 6), opts()).pi;
            assert_eq!(got, reference, "shuffled edge order changed the scores");
        }
    }
}
