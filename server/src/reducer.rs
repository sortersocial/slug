use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::events::{
    canonicalize_actor, canonicalize_item, canonicalize_tag, item_parent_path, Event, Ingest,
};

/// Parsed vote data (internal representation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteData {
    pub ts: i64,
    pub a: String,
    pub b: String,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub body: String,
    pub actor: String,
    /// Thread tag where this vote was cast. Validation requires every ingest to declare a thread (#tag or quoted title); "untagged" only for replayed legacy events.
    pub thread: String,
}

/// Single ranking group (one-ranking model). All votes contribute to this group.
#[derive(Debug, Clone)]
pub struct GroupState {
    pub item_to_idx: HashMap<String, usize>,
    pub idx_to_item: Vec<String>,

    /// Aggregated directed edge weights: (src_idx, dst_idx) -> weight.
    pub edges: HashMap<(usize, usize), f64>,

    /// Unordered pairs that have at least one vote recorded between them (i<j).
    pub voted_pairs: HashSet<(usize, usize)>,

    pub dirty: bool,
    pub cached_scores: Vec<f64>,

    pub recent_votes: VecDeque<VoteData>,
}

impl Default for GroupState {
    fn default() -> Self {
        Self::new()
    }
}

impl GroupState {
    pub fn new() -> Self {
        Self {
            item_to_idx: HashMap::new(),
            idx_to_item: Vec::new(),
            edges: HashMap::new(),
            voted_pairs: HashSet::new(),
            dirty: true,
            cached_scores: Vec::new(),
            recent_votes: VecDeque::with_capacity(200),
        }
    }

    fn ensure_item(&mut self, item: &str) -> usize {
        if let Some(&idx) = self.item_to_idx.get(item) {
            return idx;
        }
        let idx = self.idx_to_item.len();
        self.idx_to_item.push(item.to_string());
        self.item_to_idx.insert(item.to_string(), idx);
        self.dirty = true;
        idx
    }

    /// Public test helper: insert an item into the group without a vote (for unit tests).
    pub fn ensure_item_pub(&mut self, item: &str) -> usize {
        self.ensure_item(item)
    }

    fn add_edge_weight(&mut self, src: usize, dst: usize, w: f64) {
        if w <= 0.0 {
            return;
        }
        *self.edges.entry((src, dst)).or_insert(0.0) += w;
        self.dirty = true;
    }

    pub fn apply_vote(&mut self, mut vote: VoteData) {
        vote.a = canonicalize_item(&vote.a);
        vote.b = canonicalize_item(&vote.b);
        vote.actor = canonicalize_actor(&vote.actor);
        if vote.ratio_left < 0 {
            vote.ratio_left = 0;
        }
        if vote.ratio_right < 0 {
            vote.ratio_right = 0;
        }

        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
        self.voted_pairs.insert((i, j));

        let mut w_a = vote.ratio_left.max(0) as f64;
        let mut w_b = vote.ratio_right.max(0) as f64;
        if w_a == 0.0 && w_b == 0.0 {
            w_a = 1.0;
            w_b = 1.0;
        }

        self.add_edge_weight(b_idx, a_idx, w_a);
        self.add_edge_weight(a_idx, b_idx, w_b);

        self.recent_votes.push_front(vote);
        while self.recent_votes.len() > 200 {
            self.recent_votes.pop_back();
        }
    }
}

/// Compact rank-history entry stored per item in the ledger.
/// `caused_by` is resolved lazily at query time from `ingests_by_id`.
#[derive(Debug, Clone)]
pub struct RankHistoryEntry {
    pub ts: i64,
    pub scope_rank: usize,
    pub scope_rank_delta: i32,
    pub scope_total: usize,
    pub global_rank: usize,
    pub global_rank_delta: i32,
    pub global_total: usize,
    pub score: f64,
    pub thread: String,
    pub post_id: String,
}

/// First-class thread state.
#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    pub last_activity_ts: i64,
}

#[derive(Debug, Default, Clone)]
pub struct ReducerState {
    /// Single ranking group (one-ranking model).
    pub ranking_group: GroupState,

    pub items: HashSet<String>,
    pub item_bodies: HashMap<String, String>,
    /// Parent path -> direct children. Root items have parent "".
    pub item_children: HashMap<String, HashSet<String>>,

    pub ingests_by_id: HashMap<String, Ingest>,
    /// Thread -> recent ingest ids (most recent first).
    pub ingests_by_thread: HashMap<String, VecDeque<String>>,

    /// Per-item vote history (most recent first).
    pub item_votes: HashMap<String, VecDeque<VoteData>>,

    /// Per-item ingest references (most recent first).
    pub item_snippets: HashMap<String, VecDeque<String>>,

    /// Item path -> thread tags that mention or vote on this item. Connective tissue for garden body/pair/matchup.
    pub item_threads: HashMap<String, HashSet<String>>,

    /// First-class thread state: bump time, subscriber count.
    pub threads: HashMap<String, ThreadState>,

    /// Actor passkey hashes: actor -> hex-encoded SHA-256 of the registered passkey.
    /// First registration wins; subsequent ActorKeyRegistration events for the same actor are ignored.
    pub actor_keys: HashMap<String, String>,

    /// Last ingest timestamp (ms) per actor. Used by feed to default `since`.
    pub actor_last_post_ts: HashMap<String, i64>,

    /// All ingest IDs in chronological order (oldest first). Used by the feed endpoint.
    pub ingests_ordered: Vec<String>,

    /// Per-item rank history, oldest first.
    pub rank_history: HashMap<String, Vec<RankHistoryEntry>>,
}

impl ReducerState {
    /// Register parent→child edges for the full ancestor chain.
    /// For `a/b/c/d` this creates: `a/b/c→d`, `a/b→a/b/c`, `a→a/b`, `""→a`.
    /// Stops early when an intermediate is already registered (its ancestors must be too).
    /// @e2bdefa9-a6fa-4725-b0a2-c0b09d95bb20:claudecode:anthropic/claude-opus-4
    fn add_child_edge(&mut self, item: &str) {
        let mut child = item.to_string();
        loop {
            let parent = item_parent_path(&child).unwrap_or_default();
            let is_new = self.item_children
                .entry(parent.clone())
                .or_default()
                .insert(child);
            if parent.is_empty() || !is_new {
                break;
            }
            child = parent;
        }
    }

    /// Resolve an item path as a first-class canonical path.
    fn normalize_item(item: &str) -> Option<String> {
        let c = canonicalize_item(item);
        if c.is_empty() {
            return None;
        }
        Some(c)
    }

    /// 1-indexed rank of `item` within its connected component in the parent scope.
    /// 0 if the item has no votes connecting it to siblings (unranked).
    fn scope_rank_of(group: &GroupState, item: &str, item_children: &HashMap<String, HashSet<String>>) -> usize {
        let scope = item_parent_path(item).unwrap_or_default();
        let children = match item_children.get(&scope) {
            None => return 0,
            Some(c) => c,
        };
        let &item_global_idx = match group.item_to_idx.get(item) {
            None => return 0,
            Some(i) => i,
        };
        // Map scope children to compact local indices.
        let sibling_idxs: Vec<usize> = children.iter()
            .filter_map(|c| group.item_to_idx.get(c).copied())
            .collect();
        let global_to_local: HashMap<usize, usize> = sibling_idxs.iter()
            .enumerate().map(|(l, &g)| (g, l)).collect();
        let item_local = match global_to_local.get(&item_global_idx) {
            None => return 0,
            Some(&l) => l,
        };
        // Connected components within scope.
        let (comps, _) = crate::ranking::connected_components_from_voted_pairs(
            sibling_idxs.len(),
            group.voted_pairs.iter().filter_map(|(a, b)| {
                Some((global_to_local.get(a).copied()?, global_to_local.get(b).copied()?))
            }),
        );
        // Find the component containing this item.
        let comp_local = match comps.iter().find(|c| c.contains(&item_local)) {
            None => return 0,
            Some(c) => c,
        };
        let comp_global: Vec<usize> = comp_local.iter()
            .filter_map(|&l| sibling_idxs.get(l).copied())
            .collect();
        let ranked = crate::ranking::ranked_items_subset(group, &comp_global, 10000, 1e-8);
        ranked.iter().position(|r| r.item == item).map(|i| i + 1).unwrap_or(0)
    }

    /// 1-indexed position of `item` in the component-aware global flat list.
    /// Components sorted largest-first; items ranked within each component.
    /// 0 if the item is not in the ranking group.
    fn global_rank_of(group: &GroupState, item: &str) -> usize {
        if !group.item_to_idx.contains_key(item) {
            return 0;
        }
        let n = group.idx_to_item.len();
        let (mut comps, _) = crate::ranking::connected_components_from_voted_pairs(
            n, group.voted_pairs.iter().copied(),
        );
        comps.sort_by(|a, b| b.len().cmp(&a.len()));
        let mut pos = 1usize;
        for comp in &comps {
            let ranked = crate::ranking::ranked_items_subset(group, comp, 10000, 1e-8);
            for r in &ranked {
                if r.item == item {
                    return pos;
                }
                pos += 1;
            }
        }
        0
    }

    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Ingest(mut ing) => {
                ing.actor = canonicalize_actor(&ing.actor);

                self.ingests_by_id.insert(ing.id.clone(), ing.clone());

                let doc = match crate::dsl::parse_full(&ing.raw) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("WARNING: Skipping malformed ingest event {}: {}", ing.id, e);
                        return;
                    }
                };

                // Pre-scan: collect items directly voted on in this ingest.
                let voted_items: Vec<String> = doc.statements.iter().filter_map(|s| {
                    if let crate::dsl::Stmt::Vote { item1, item2, .. } = s {
                        Some([item1, item2])
                    } else {
                        None
                    }
                }).flat_map(|pair| pair.into_iter())
                    .filter_map(|raw| Self::normalize_item(raw))
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter().collect();

                // Snapshot ranks before votes are applied (force-compute if stale).
                let before: HashMap<String, (usize, usize)> = if !voted_items.is_empty() {
                    crate::ranking::compute_group_ranking(&mut self.ranking_group, 10000, 1e-8);
                    voted_items.iter()
                        .map(|it| (it.clone(), (
                            Self::scope_rank_of(&self.ranking_group, it, &self.item_children),
                            Self::global_rank_of(&self.ranking_group, it),
                        )))
                        .collect()
                } else {
                    HashMap::new()
                };

                let mut current_thread: Option<String> = None;
                let mut current_actor: Option<String> = Some(ing.actor.clone());

                // Threads explicitly declared in this ingest (#tag or quoted title).
                let mut touched_threads: HashSet<String> = HashSet::new();
                // Items referenced in this ingest (for snippet indexing).
                let mut ingest_items: HashSet<String> = HashSet::new();

                for stmt in doc.statements {
                    match stmt {
                        crate::dsl::Stmt::Hashtag { name } => {
                            let t = canonicalize_tag(&name);
                            touched_threads.insert(t.clone());
                            current_thread = Some(t);
                        }
                        crate::dsl::Stmt::Actor { name } => {
                            current_actor = Some(canonicalize_actor(&name));
                        }
                        crate::dsl::Stmt::Item { title, body } => {
                            let Some(item) = Self::normalize_item(&title) else {
                                continue;
                            };
                            nav!(self.items, set_elem(item.clone()));
                            ingest_items.insert(item.clone());
                            self.add_child_edge(&item);

                            if let Some(body_text) = body {
                                if !body_text.trim().is_empty() {
                                    nav!(self.item_bodies, keypath(item.clone()), setval(body_text));
                                }
                            }
                        }
                        crate::dsl::Stmt::Vote {
                            item1,
                            item2,
                            ratio_left,
                            ratio_right,
                            explanation,
                        } => {
                            let Some(item_a) = Self::normalize_item(&item1) else {
                                continue;
                            };
                            let Some(item_b) = Self::normalize_item(&item2) else {
                                continue;
                            };

                            let actor = current_actor.clone().unwrap_or_else(|| ing.actor.clone());
                            let vote = VoteData {
                                ts: ing.ts,
                                a: item_a.clone(),
                                b: item_b.clone(),
                                ratio_left,
                                ratio_right,
                                body: explanation,
                                actor,
                                thread: current_thread.clone().unwrap_or_else(|| "untagged".to_string()), // legacy replay: pre-validation events may have no thread declaration
                            };

                            ingest_items.insert(item_a.clone());
                            ingest_items.insert(item_b.clone());

                            nav!(self.items, set_elem(item_a.clone()));
                            nav!(self.items, set_elem(item_b.clone()));
                            self.add_child_edge(&item_a);
                            self.add_child_edge(&item_b);

                            self.ranking_group.apply_vote(vote.clone());

                            // Index vote by item.
                            for it in [&item_a, &item_b] {
                                nav!(self.item_votes, keypath(it.clone()), push_front(vote.clone()));
                            }
                        }
                        crate::dsl::Stmt::Prose { .. } => {}
                    }
                }

                // Index snippets by item for this ingest.
                for item in ingest_items.iter() {
                    nav!(self.item_snippets, keypath(item.clone()), push_front(ing.id.clone()));
                }

                // Index item -> threads (connective tissue for garden body/pair/matchup).
                for item in ingest_items.iter() {
                    nav_each!(self.item_threads, keypath(item.clone()), set_elem, touched_threads.iter().cloned());
                }

                // Bump thread state for explicitly declared threads.
                for thread in &touched_threads {
                    let ts = self.threads.entry(thread.clone()).or_default();
                    nav!(ts.last_activity_ts, selected(ing.ts > ts.last_activity_ts, setval(ing.ts)));
                }

                // Index ingest by touched threads.
                for thread in touched_threads {
                    nav!(self.ingests_by_thread, keypath(thread), push_front(ing.id.clone()));
                }

                nav!(self.ingests_ordered, push_back(ing.id.clone()));

                // Snapshot ranks after votes and push history entries.
                if !voted_items.is_empty() {
                    crate::ranking::compute_group_ranking(&mut self.ranking_group, 10000, 1e-8);
                    let thread = current_thread.clone().unwrap_or_else(|| "untagged".to_string());
                    for item in &voted_items {
                        let after_scope  = Self::scope_rank_of(&self.ranking_group, item, &self.item_children);
                        let after_global = Self::global_rank_of(&self.ranking_group, item);
                        let score = self.ranking_group.item_to_idx.get(item)
                            .and_then(|&i| self.ranking_group.cached_scores.get(i))
                            .copied().unwrap_or(0.0);
                        let (before_scope, before_global) = before.get(item).copied().unwrap_or((0, 0));
                        let prev = self.rank_history.get(item).and_then(|v| v.last());
                        let scope_delta  = if prev.is_none() { 0 } else { after_scope as i32 - before_scope as i32 };
                        let global_delta = if prev.is_none() { 0 } else { after_global as i32 - before_global as i32 };
                        let scope = item_parent_path(item).unwrap_or_default();
                        let scope_total  = self.item_children.get(&scope).map(|s| s.len()).unwrap_or(0);
                        let global_total = self.ranking_group.idx_to_item.len();
                        self.rank_history.entry(item.clone()).or_default().push(RankHistoryEntry {
                            ts: ing.ts,
                            scope_rank: after_scope,
                            scope_rank_delta: scope_delta,
                            scope_total,
                            global_rank: after_global,
                            global_rank_delta: global_delta,
                            global_total,
                            score,
                            thread: thread.clone(),
                            post_id: ing.id.clone(),
                        });
                    }
                }

                nav!(self.actor_last_post_ts, keypath(ing.actor.clone()), setval(ing.ts));
            }
            Event::ActorKeyRegistration { actor, key_hash, .. } => {
                let actor = canonicalize_actor(&actor);
                nav!(self.actor_keys, keypath(actor), or_insert(key_hash));
            }
        }
    }
}
