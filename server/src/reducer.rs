use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::canonical_path::canonicalize_tag;
use crate::dsl;
use crate::events::{Event, Ingest, ThreadCapability};
use crate::path_types::ItemId;

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScopeId {
    Public,
    Room(String),
}

/// Wire `room` field → content scope (`"public"` → [`ScopeId::Public`]).
pub fn scope_from_room_wire(room: &str) -> ScopeId {
    let r = room.trim();
    if r.is_empty() || r == "public" {
        ScopeId::Public
    } else {
        ScopeId::Room(r.to_string())
    }
}

/// Parsed vote data (internal representation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteData {
    pub ts: i64,
    pub a: ItemId,
    pub b: ItemId,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub body: String,
    pub principal: String,
    pub delegate: Option<String>,
    /// Forum channel where this vote was cast (tag only, not room id).
    pub thread_tag: String,
}

#[derive(Debug, Clone)]
pub struct GroupState {
    pub item_to_idx: HashMap<ItemId, usize>,
    pub idx_to_item: Vec<ItemId>,

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

    fn ensure_item(&mut self, item: &ItemId) -> usize {
        if let Some(&idx) = self.item_to_idx.get(item) {
            return idx;
        }
        let idx = self.idx_to_item.len();
        self.idx_to_item.push(item.clone());
        self.item_to_idx.insert(item.clone(), idx);
        self.dirty = true;
        idx
    }

    /// Public test helper: insert an item into the group without a vote (for unit tests).
    pub fn ensure_item_pub(&mut self, item: &str) -> usize {
        if let Some(canon) = ItemId::parse(item) {
            self.ensure_item(&canon)
        } else {
            // Fallback: treat as raw storage string
            let canon = ItemId::opaque(item.to_string());
            self.ensure_item(&canon)
        }
    }

    fn add_edge_weight(&mut self, src: usize, dst: usize, w: f64) {
        if w <= 0.0 {
            return;
        }
        *self.edges.entry((src, dst)).or_insert(0.0) += w;
        self.dirty = true;
    }

    pub fn apply_vote(&mut self, mut vote: VoteData) {
        vote.a = ItemId::parse(vote.a.as_str()).unwrap_or_else(|| vote.a.clone());
        vote.b = ItemId::parse(vote.b.as_str()).unwrap_or_else(|| vote.b.clone());
        vote.thread_tag = canonicalize_tag(&vote.thread_tag);
        if vote.ratio_left < 0 {
            vote.ratio_left = 0;
        }
        if vote.ratio_right < 0 {
            vote.ratio_right = 0;
        }
        if vote.ratio_left == 0 || vote.ratio_right == 0 {
            // Zero on either side produces no valid edge; drop before registering items or pair.
            return;
        }

        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
        self.voted_pairs.insert((i, j));

        let w_a = vote.ratio_left as f64;
        let w_b = vote.ratio_right as f64;

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

/// Invite link materialized from log events [`crate::events::InviteMinted`] /
/// [`crate::events::InviteRedeemed`] when those are replayed.
///
/// **`RoomMintInvite` today** stores tokens only in [`crate::state::AppState::invites`] (RAM);
/// they are not appended to the JSONL log, so they disappear on restart. This struct is for
/// replay and any future persisted-mint path, not for the current ephemeral RPC mint.
#[derive(Debug, Clone)]
pub struct ActiveInviteState {
    pub room_id: String,
    pub capabilities: HashSet<ThreadCapability>,
    pub inviter: String,
    pub uses_remaining: u32,
    pub expires_ts_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub enum RoomTimelineKind {
    RoomCreated {
        owner: String,
        slug: String,
    },
    RoomDeleted {
        deleted_by: String,
    },
    GrantAdded {
        username: String,
        granted_by: String,
        capabilities: Vec<ThreadCapability>,
    },
    GrantRevoked {
        username: String,
        revoked_by: String,
        capabilities: Vec<ThreadCapability>,
    },
}

#[derive(Clone, Debug)]
pub struct RoomTimelineEntry {
    pub ts: i64,
    pub kind: RoomTimelineKind,
}

#[derive(Debug, Clone)]
#[derive(Default)]
pub struct ForumThreadState {
    pub last_activity_ts: i64,
    /// Username of the most recent person who bumped this thread.
    pub last_actor: String,
}


#[derive(Debug, Clone, Default)]
pub struct ContentState {
    pub ranking_group: GroupState,
    pub items: HashSet<ItemId>,
    pub item_bodies: HashMap<ItemId, String>,
    /// Parent [`ItemId`] -> direct children.
    pub item_children: HashMap<ItemId, HashSet<ItemId>>,
    /// Per-item vote history (most recent first).
    pub item_votes: HashMap<ItemId, VecDeque<VoteData>>,
    /// Per-item ingest references (most recent first).
    pub item_snippets: HashMap<ItemId, VecDeque<String>>,
    /// Item path -> threads that mention or vote on this item.
    pub item_threads: HashMap<ItemId, HashSet<String>>,
    /// Per-item rank history, oldest first.
    pub rank_history: HashMap<ItemId, Vec<RankHistoryEntry>>,
}

#[derive(Debug, Clone)]
pub struct ReducerState {
    pub content: HashMap<ScopeId, ContentState>,

    /// (provider, provider_id) -> username
    pub users_by_provider: HashMap<(String, String), String>,
    /// token_id -> (username, salt, token_hash)
    pub tokens_by_id: HashMap<String, (String, String, String)>,
    pub agent_bindings: HashMap<String, String>,


    pub ingests_by_id: HashMap<String, Ingest>,
    /// (scope, thread_tag) → ingest ids, newest first.
    pub ingests_by_scope_thread: HashMap<(ScopeId, String), VecDeque<String>>,
    /// Private room ids (`shortid/slug`) known from [`RoomCreated`].
    pub rooms: HashSet<String>,
    /// (scope, thread_tag) → last activity.
    pub forum_threads: HashMap<(ScopeId, String), ForumThreadState>,
    pub actor_last_post_ts: HashMap<String, i64>,

    /// All ingest IDs in chronological order (oldest first). Used by the feed endpoint.
    pub ingests_ordered: Vec<String>,
    /// Username → ingest ids in post order (oldest first), for profile pages.
    pub posts_by_actor: HashMap<String, VecDeque<String>>,
    /// Ingest ids removed by author redaction (content and thread body omitted; tombstone in UI).
    pub redacted_posts: HashSet<String>,
    /// Redaction event timestamp (ms) per post id.
    pub post_redact_ts: HashMap<String, i64>,
    /// room_id → username → capabilities
    pub grants: HashMap<String, HashMap<String, HashSet<ThreadCapability>>>,
    /// room_id → chronological room admin lines (for thread UI).
    pub room_timeline: HashMap<String, Vec<RoomTimelineEntry>>,
    /// Invite token → active invite (absent when fully consumed or never minted).
    pub invites: HashMap<String, ActiveInviteState>,
}

impl ReducerState {
    pub fn content_for_scope(&self, scope: &ScopeId) -> Option<&ContentState> {
        self.content.get(scope)
    }

    /// 0-based chronological index of `post_id` in `(scope, thread_tag)` (forum routes `/t/tag/N`).
    pub fn try_thread_post_index_chronological(
        &self,
        scope: &ScopeId,
        thread_tag: &str,
        post_id: &str,
    ) -> Option<usize> {
        let tag = canonicalize_tag(thread_tag);
        self.ingests_by_scope_thread
            .get(&(scope.clone(), tag))
            .and_then(|q| q.iter().rev().position(|pid| pid == post_id))
    }

    pub fn thread_post_index_chronological(
        &self,
        scope: &ScopeId,
        thread_tag: &str,
        post_id: &str,
    ) -> usize {
        self.try_thread_post_index_chronological(scope, thread_tag, post_id)
            .expect(
                "post_id must appear in ingests_by_scope_thread for this scope and thread",
            )
    }

    /// Ingest ids authored by `actor`, oldest first. Unfiltered; use with access checks per ingest.
    pub fn posts_by_actor_ids(&self, actor: &str) -> Vec<String> {
        self.posts_by_actor
            .get(actor)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Same order as [`Self::posts_by_actor_ids`], omitting ingests in scopes the viewer cannot see.
    pub fn visible_posts_for_actor(&self, actor: &str, viewer: Option<&str>) -> Vec<String> {
        self.posts_by_actor_ids(actor)
            .into_iter()
            .filter(|id| {
                !self.redacted_posts.contains(id)
                    && self.ingests_by_id.get(id).is_some_and(|ing| {
                        let scope = scope_from_room_wire(&ing.room_id);
                        match &scope {
                            ScopeId::Public => true,
                            ScopeId::Room(rid) => {
                                viewer.is_some_and(|u| self.user_has_cap(rid, u, ThreadCapability::View))
                            }
                        }
                    })
            })
            .collect()
    }

    pub fn user_has_cap(&self, room_id: &str, username: &str, cap: ThreadCapability) -> bool {
        self.grants
            .get(room_id)
            .and_then(|t| t.get(username))
            .map(|caps| caps.contains(&cap))
            .unwrap_or(false)
    }

    /// Invite link is present, not expired, and has uses left.
    pub fn invite_token_active(&self, token: &str, now_ms: i64) -> Option<&ActiveInviteState> {
        let inv = self.invites.get(token)?;
        if inv.uses_remaining == 0 {
            return None;
        }
        if let Some(exp) = inv.expires_ts_ms {
            if now_ms > exp {
                return None;
            }
        }
        Some(inv)
    }

    pub fn content_for_scope_mut(&mut self, scope: ScopeId) -> &mut ContentState {
        self.content.entry(scope).or_default()
    }

    pub fn public(&self) -> &ContentState {
        self.content.get(&ScopeId::Public).expect("public scope missing")
    }

    /// Register parent→child edges for the full ancestor chain.
    /// For `a/b/c/d` this creates: `a/b/c→d`, `a/b→a/b/c`, `a→a/b`, `""→a`.
    /// Stops early when an intermediate is already registered (its ancestors must be too).
    /// @e2bdefa9-a6fa-4725-b0a2-c0b09d95bb20:claudecode:anthropic/claude-opus-4
    fn add_child_edge(content: &mut ContentState, item: &ItemId) {
        let mut child = item.clone();
        loop {
            let Some(parent) = child.parent() else { break };
            let is_new = content
                .item_children
                .entry(parent.clone())
                .or_default()
                .insert(child);
            if !is_new { break; }
            child = parent;
        }
    }

    /// Resolve an item path as a first-class [`ItemId`].
    fn normalize_item(item: &str) -> Option<ItemId> {
        ItemId::parse(item)
    }

    /// 1-indexed rank of `item` within its connected component in the parent scope.
    /// 0 if the item has no votes connecting it to siblings (unranked).
    fn scope_rank_of(
        group: &GroupState,
        item: &ItemId,
        item_children: &HashMap<ItemId, HashSet<ItemId>>,
    ) -> usize {
        let scope = match item.parent() {
            Some(p) => p,
            None => return 0,
        };
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
        ranked.iter().position(|r| &r.item == item).map(|i| i + 1).unwrap_or(0)
    }

    /// 1-indexed position of `item` in the component-aware global flat list.
    /// Components sorted largest-first; items ranked within each component.
    /// 0 if the item is not in the ranking group.
    fn global_rank_of(group: &GroupState, item: &ItemId) -> usize {
        if !group.item_to_idx.contains_key(item) {
            return 0;
        }
        let n = group.idx_to_item.len();
        let (mut comps, _) = crate::ranking::connected_components_from_voted_pairs(
            n, group.voted_pairs.iter().copied(),
        );
        comps.sort_by_key(|b| std::cmp::Reverse(b.len()));
        let mut pos = 1usize;
        for comp in &comps {
            let ranked = crate::ranking::ranked_items_subset(group, comp, 10000, 1e-8);
            for r in &ranked {
                if &r.item == item {
                    return pos;
                }
                pos += 1;
            }
        }
        0
    }

    /// Apply one ingest's DSL effects to `content` (votes, items, snippets, rank history).
    fn apply_ingest_to_content(content: &mut ContentState, ing: &Ingest) -> Result<(), ()> {
        let doc = dsl::parse_full(&ing.raw).map_err(|_| ())?;
        let canonical_thread = canonicalize_tag(&ing.thread_tag);

        let voted_items: Vec<ItemId> = doc
            .statements
            .iter()
            .filter_map(|s| {
                if let dsl::Stmt::Vote { item1, item2, .. } = s {
                    Some([item1, item2])
                } else {
                    None
                }
            })
            .flat_map(|pair| pair.into_iter())
            .filter_map(|raw| Self::normalize_item(raw))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let principal = ing.principal.clone();
        let delegate = ing.delegate.clone();

        let before: HashMap<ItemId, (usize, usize)> = if !voted_items.is_empty() {
            crate::ranking::compute_group_ranking(&mut content.ranking_group, 10000, 1e-8);
            voted_items
                .iter()
                .map(|it| {
                    (
                        it.clone(),
                        (
                            Self::scope_rank_of(&content.ranking_group, it, &content.item_children),
                            Self::global_rank_of(&content.ranking_group, it),
                        ),
                    )
                })
                .collect()
        } else {
            HashMap::new()
        };

        let mut ingest_items: HashSet<ItemId> = HashSet::new();

        for stmt in doc.statements {
            match stmt {
                dsl::Stmt::Item { title, body } => {
                    let Some(item) = Self::normalize_item(&title) else {
                        continue;
                    };
                    nav!(content.items, set_elem(item.clone()));
                    ingest_items.insert(item.clone());
                    Self::add_child_edge(content, &item);

                    if let Some(body_text) = body {
                        if !body_text.trim().is_empty() {
                            nav!(content.item_bodies, keypath(item.clone()), setval(body_text));
                        }
                    }
                }
                dsl::Stmt::Vote {
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

                    let vote = VoteData {
                        ts: ing.ts,
                        a: item_a.clone(),
                        b: item_b.clone(),
                        ratio_left,
                        ratio_right,
                        body: explanation,
                        principal: principal.clone(),
                        delegate: delegate.clone(),
                        thread_tag: canonical_thread.clone(),
                    };

                    ingest_items.insert(item_a.clone());
                    ingest_items.insert(item_b.clone());

                    nav!(content.items, set_elem(item_a.clone()));
                    nav!(content.items, set_elem(item_b.clone()));
                    Self::add_child_edge(content, &item_a);
                    Self::add_child_edge(content, &item_b);

                    content.ranking_group.apply_vote(vote.clone());

                    for it in [&item_a, &item_b] {
                        nav!(content.item_votes, keypath(it.clone()), push_front(vote.clone()));
                    }
                }
                dsl::Stmt::Prose { .. } => {}
            }
        }

        for item in ingest_items.iter() {
            nav!(content.item_snippets, keypath(item.clone()), push_front(ing.id.clone()));
        }

        for item in ingest_items.iter() {
            nav!(
                content.item_threads,
                keypath(item.clone()),
                set_elem(canonical_thread.clone())
            );
        }

        if !voted_items.is_empty() {
            crate::ranking::compute_group_ranking(&mut content.ranking_group, 10000, 1e-8);
            let thread = canonical_thread.clone();
            for item in &voted_items {
                let after_scope = Self::scope_rank_of(&content.ranking_group, item, &content.item_children);
                let after_global = Self::global_rank_of(&content.ranking_group, item);
                let score = content
                    .ranking_group
                    .item_to_idx
                    .get(item)
                    .and_then(|&i| content.ranking_group.cached_scores.get(i))
                    .copied()
                    .unwrap_or(0.0);
                let (before_scope, before_global) = before.get(item).copied().unwrap_or((0, 0));
                let prev = content.rank_history.get(item).and_then(|v| v.last());
                let scope_delta = if prev.is_none() {
                    0
                } else {
                    after_scope as i32 - before_scope as i32
                };
                let global_delta = if prev.is_none() {
                    0
                } else {
                    after_global as i32 - before_global as i32
                };
                let scope_total = item
                    .parent()
                    .and_then(|p| content.item_children.get(&p))
                    .map(|s| s.len())
                    .unwrap_or(0);
                let global_total = content.ranking_group.idx_to_item.len();
                content.rank_history.entry(item.clone()).or_default().push(RankHistoryEntry {
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

        Ok(())
    }

    fn rebuild_scope_content(&mut self, scope: ScopeId) {
        let mut cs = ContentState::default();
        for id in &self.ingests_ordered {
            if self.redacted_posts.contains(id) {
                continue;
            }
            let Some(ing) = self.ingests_by_id.get(id) else {
                continue;
            };
            if scope_from_room_wire(&ing.room_id) != scope {
                continue;
            }
            let _ = Self::apply_ingest_to_content(&mut cs, ing);
        }
        self.content.insert(scope, cs);
        // `ingests_by_scope_thread` is intentionally not rebuilt: tombstoned ids stay in the deque so
        // per-post URLs and chronological indices remain stable; only projected garden state resets.
    }

    /// Drop all reducer state keyed by a private room id (forum, garden scope, invites, grants).
    fn purge_private_room(&mut self, room_id: &str) {
        let scope = ScopeId::Room(room_id.to_string());
        self.rooms.remove(room_id);
        self.grants.remove(room_id);
        self.room_timeline.remove(room_id);
        self.invites.retain(|_, inv| inv.room_id != room_id);
        self.content.remove(&scope);
        self.forum_threads.retain(|(s, _), _| s != &scope);
        self.ingests_by_scope_thread.retain(|(s, _), _| s != &scope);

        let mut to_drop: Vec<String> = self
            .ingests_by_id
            .iter()
            .filter(|(_, ing)| ing.room_id.trim() == room_id)
            .map(|(id, _)| id.clone())
            .collect();
        to_drop.sort();
        to_drop.dedup();
        for id in to_drop {
            if let Some(ing) = self.ingests_by_id.remove(&id) {
                self.posts_by_actor
                    .entry(ing.principal)
                    .and_modify(|q| {
                        q.retain(|x| x != &id);
                    });
            }
            self.ingests_ordered.retain(|x| x != &id);
            self.redacted_posts.remove(&id);
            self.post_redact_ts.remove(&id);
        }
    }

    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::UserRegistered(ur) => {
                self.users_by_provider.insert(
                    (ur.provider.to_lowercase(), ur.provider_id.clone()),
                    ur.username,
                );
            }
            Event::TokenIssued(ti) => {
                self.tokens_by_id.insert(
                    ti.token_id.clone(),
                    (ti.username, ti.salt.clone(), ti.token_hash.clone()),
                );
            }
            Event::AgentBound(ab) => {
                if ab.agent.is_empty() {
                    return;
                }
                self.agent_bindings.insert(ab.agent, ab.username);
            }
            Event::RoomCreated(rc) => {
                self.rooms.insert(rc.room_id.clone());
                self.room_timeline
                    .entry(rc.room_id.clone())
                    .or_default()
                    .push(RoomTimelineEntry {
                        ts: rc.ts,
                        kind: RoomTimelineKind::RoomCreated {
                            owner: rc.owner.clone(),
                            slug: rc.slug.clone(),
                        },
                    });
            }
            Event::RoomDeleted(rd) => {
                let room_id = rd.room_id.clone();
                if self.rooms.contains(&room_id) {
                    self.room_timeline
                        .entry(room_id.clone())
                        .or_default()
                        .push(RoomTimelineEntry {
                            ts: rd.ts,
                            kind: RoomTimelineKind::RoomDeleted {
                                deleted_by: rd.deleted_by.clone(),
                            },
                        });
                }
                self.purge_private_room(&room_id);
            }
            Event::Ingest(mut ing) => {
                ing.thread_tag = canonicalize_tag(&ing.thread_tag);
                let room_key = ing.room_id.trim().to_string();
                let scope = scope_from_room_wire(&room_key);
                let canonical_thread = ing.thread_tag.clone();
                let scope_thread_key = (scope.clone(), canonical_thread.clone());

                {
                    let content = self.content_for_scope_mut(scope.clone());
                    if Self::apply_ingest_to_content(content, &ing).is_err() {
                        eprintln!(
                            "WARNING: Skipping malformed ingest event {}: parse failed",
                            ing.id
                        );
                        return;
                    }
                }

                self.ingests_by_id.insert(ing.id.clone(), ing.clone());

                let ft = self.forum_threads.entry(scope_thread_key.clone()).or_default();
                let prev_ts = ft.last_activity_ts;
                if ing.ts > prev_ts {
                    ft.last_activity_ts = ing.ts;
                    ft.last_actor = ing.principal.clone();
                }

                nav!(self.ingests_by_scope_thread, keypath(scope_thread_key), push_front(ing.id.clone()));

                nav!(self.ingests_ordered, push_back(ing.id.clone()));

                nav!(self.posts_by_actor, keypath(ing.principal.clone()), push_back(ing.id.clone()));

                nav!(self.actor_last_post_ts, keypath(ing.principal.clone()), setval(ing.ts));
            }
            Event::PostRedacted(pr) => {
                self.redacted_posts.insert(pr.post_id.clone());
                self.post_redact_ts.insert(pr.post_id.clone(), pr.ts);
                let Some(ing) = self.ingests_by_id.get(&pr.post_id).cloned() else {
                    return;
                };
                let scope = scope_from_room_wire(ing.room_id.trim());
                // Rebuilds garden projection only; `ingests_by_scope_thread` is left as-is — see `rebuild_scope_content`.
                self.rebuild_scope_content(scope);
            }
            Event::GrantAdded(ga) => {
                let room_id = ga.room_id.clone();
                let caps = self.grants
                    .entry(ga.room_id)
                    .or_default()
                    .entry(ga.username.clone())
                    .or_default();
                for cap in ga.capabilities.iter().copied() {
                    caps.insert(cap);
                }
                self.room_timeline
                    .entry(room_id)
                    .or_default()
                    .push(RoomTimelineEntry {
                        ts: ga.ts,
                        kind: RoomTimelineKind::GrantAdded {
                            username: ga.username.clone(),
                            granted_by: ga.granted_by.clone(),
                            capabilities: ga.capabilities.clone(),
                        },
                    });
            }
            Event::GrantRevoked(gr) => {
                let room_id = gr.room_id.clone();
                if let Some(room_grants) = self.grants.get_mut(&gr.room_id) {
                    let username = gr.username.clone();
                    if let Some(caps) = room_grants.get_mut(&username) {
                        for cap in &gr.capabilities {
                            caps.remove(cap);
                        }
                        if caps.is_empty() {
                            room_grants.remove(&username);
                        }
                    }
                    if room_grants.is_empty() {
                        self.grants.remove(&gr.room_id);
                    }
                }
                self.room_timeline
                    .entry(room_id)
                    .or_default()
                    .push(RoomTimelineEntry {
                        ts: gr.ts,
                        kind: RoomTimelineKind::GrantRevoked {
                            username: gr.username.clone(),
                            revoked_by: gr.revoked_by.clone(),
                            capabilities: gr.capabilities.clone(),
                        },
                    });
            }
            Event::InviteMinted(im) => {
                self.invites.insert(
                    im.token.clone(),
                    ActiveInviteState {
                        room_id: im.room_id.clone(),
                        capabilities: im.capabilities.iter().copied().collect(),
                        inviter: im.inviter.clone(),
                        uses_remaining: im.max_uses,
                        expires_ts_ms: im.expires_ts_ms,
                    },
                );
            }
            Event::InviteRedeemed(ir) => {
                if let Some(inv) = self.invites.get_mut(&ir.token) {
                    inv.uses_remaining = inv.uses_remaining.saturating_sub(1);
                    if inv.uses_remaining == 0 {
                        self.invites.remove(&ir.token);
                    }
                }
            }
        }
    }
}

impl Default for ReducerState {
    fn default() -> Self {
        let mut content = HashMap::new();
        content.insert(ScopeId::Public, ContentState::default());
        Self {
            content,
            users_by_provider: HashMap::new(),
            tokens_by_id: HashMap::new(),
            agent_bindings: HashMap::new(),
            ingests_by_id: HashMap::new(),
            ingests_by_scope_thread: HashMap::new(),
            rooms: HashSet::new(),
            forum_threads: HashMap::new(),
            actor_last_post_ts: HashMap::new(),
            ingests_ordered: Vec::new(),
            posts_by_actor: HashMap::new(),
            redacted_posts: HashSet::new(),
            post_redact_ts: HashMap::new(),
            grants: HashMap::new(),
            room_timeline: HashMap::new(),
            invites: HashMap::new(),
        }
    }
}
