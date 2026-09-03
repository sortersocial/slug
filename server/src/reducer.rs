use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::canonical_path::canonicalize_tag;
use crate::dsl;
use crate::events::{Event, Ingest, ThreadCapability};
use crate::path_types::ItemId;
use slug_types::PostStats;

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
    /// Monotonic in-memory version of the vote graph. Derived caches use this
    /// to detect whether their rankings still describe the current edges.
    pub generation: u64,
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
            generation: 0,
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
        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        let (i, j) = if a_idx < b_idx {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };
        self.voted_pairs.insert((i, j));

        let w_a = vote.ratio_left as f64;
        let w_b = vote.ratio_right as f64;

        self.add_edge_weight(b_idx, a_idx, w_a);
        self.add_edge_weight(a_idx, b_idx, w_b);

        self.recent_votes.push_front(vote);
        while self.recent_votes.len() > 200 {
            self.recent_votes.pop_back();
        }
        self.generation = self
            .generation
            .checked_add(1)
            .expect("vote graph generation overflow");
    }
}

/// Weights on one directed containment pair `(child, parent)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainmentWeights {
    /// Accumulating explicit `<:` claims.
    pub explicit: u32,
    /// At most one from path desugaring (idempotent across ingests).
    pub sugar: bool,
}

impl ContainmentWeights {
    pub fn containment_weight(&self) -> u32 {
        self.explicit.saturating_add(u32::from(self.sugar))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipStatus {
    Active,
    Suspended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorderPairState {
    pub child: ItemId,
    pub parent: ItemId,
    pub containment_weight: u32,
    /// Explicit `<:` claims (accumulate). Sugar (`~/a/b` desugar) adds at most 1 (see `sugar`).
    pub explicit: u32,
    /// Whether idempotent `~/a/b` path sugar contributed (worth exactly 1).
    pub sugar: bool,
    pub border_weight: u32,
    pub status: MembershipStatus,
}

/// Derived fallen-border journal entry (rebuilt on replay; never persisted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallenBorderEntry {
    pub ts: i64,
    pub child: ItemId,
    pub parent: ItemId,
    pub containment_weight: u32,
    pub border_weight: u32,
    pub ingest_id: String,
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
    ThreadGraduated {
        thread_tag: String,
        graduated_by: String,
        posts_copied: u32,
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

#[derive(Debug, Clone, Default)]
pub struct ForumThreadState {
    pub last_activity_ts: i64,
    /// Username of the most recent person who bumped this thread.
    pub last_actor: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RankPositionCache {
    generation: u64,
    /// 1-indexed global rank and component-local Rank Centrality score.
    global: HashMap<ItemId, (usize, f64)>,
    /// Scope item → 1-indexed rank among that scope's active members.
    by_parent: HashMap<ItemId, HashMap<ItemId, usize>>,
    #[cfg(test)]
    recomputations: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ContentState {
    pub ranking_group: GroupState,
    pub items: HashSet<ItemId>,
    pub item_bodies: HashMap<ItemId, String>,
    /// Parent [`ItemId`] -> direct children.
    ///
    /// Tilde-ontology keys are active containment members (synced from
    /// [`Self::members_by_scope`]). URL / external keys keep path-hierarchy
    /// children from [`ReducerState::add_child_edge`], plus any containment members.
    pub item_children: HashMap<ItemId, HashSet<ItemId>>,
    /// `(child, parent)` → containment weights (explicit accumulates; sugar is idempotent).
    pub containment: HashMap<(ItemId, ItemId), ContainmentWeights>,
    /// `(child, parent)` → `!<:` border weight (accumulates).
    pub borders: HashMap<(ItemId, ItemId), u32>,
    /// Parent → currently **active** members (`containment_weight > border_weight`).
    pub members_by_scope: HashMap<ItemId, HashSet<ItemId>>,
    /// Child → scopes in which membership is active.
    pub scopes_by_member: HashMap<ItemId, HashSet<ItemId>>,
    /// Fallen-border journal, chronological. Derived; rebuilt on replay.
    pub fallen_border_journal: Vec<FallenBorderEntry>,
    /// Per-item vote history (most recent first).
    pub item_votes: HashMap<ItemId, VecDeque<VoteData>>,
    /// Per-item ingest references (most recent first).
    pub item_snippets: HashMap<ItemId, VecDeque<String>>,
    /// Item path -> threads that mention or vote on this item.
    pub item_threads: HashMap<ItemId, HashSet<String>>,
    /// Per-item rank history, oldest first.
    pub rank_history: HashMap<ItemId, Vec<RankHistoryEntry>>,
    /// Aspect ranking groups keyed by (scope item, aspect slug).
    /// Canonical votes stay in `ranking_group`. Membership follows the
    /// scope item's active-member electorate.
    pub aspect_groups: HashMap<(ItemId, String), GroupState>,
    /// Prompt text per aspect slug in this room; last non-empty write wins.
    pub aspect_prompts: HashMap<String, String>,
    /// RAM-only memo of rank positions at `ranking_group.generation`.
    ///
    /// The next vote ingest's "before" positions are exactly the previous vote
    /// ingest's "after" positions. Keeping the complete global ordering and
    /// lazily populated parent-scope orderings avoids recomputing both before
    /// every ingest. A vote bumps the generation and forces one fresh "after"
    /// computation; item-only ingests do not invalidate it because isolates do
    /// not affect any ranked component.
    pub(crate) rank_position_cache: Option<RankPositionCache>,
}

impl ContentState {
    pub fn aspect_group(&self, parent: &ItemId, aspect: &str) -> Option<&GroupState> {
        self.aspect_groups
            .get(&(parent.ontology_leaf(), aspect.to_string()))
    }

    pub fn aspect_prompt(&self, aspect: &str) -> Option<&str> {
        self.aspect_prompts.get(aspect).map(String::as_str)
    }

    /// Active members of `item` (the electorate when `item` is used as a scope).
    pub fn members_of(&self, item: &ItemId) -> Vec<ItemId> {
        let key = item.ontology_leaf();
        let mut out: Vec<ItemId> = self
            .members_by_scope
            .get(&key)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        if key.tilde_tail().is_none() {
            if let Some(ch) = self.item_children.get(&key) {
                out.extend(ch.iter().cloned());
            }
            if let Some(ch) = self.item_children.get(item) {
                out.extend(ch.iter().cloned());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Scopes in which `item` is an active member.
    pub fn scopes_of(&self, item: &ItemId) -> Vec<ItemId> {
        let key = item.ontology_leaf();
        let mut out: Vec<ItemId> = self
            .scopes_by_member
            .get(&key)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Shared scopes: items in which both `a` and `b` are active members.
    pub fn shared_scopes(&self, a: &ItemId, b: &ItemId) -> Vec<ItemId> {
        let sa: HashSet<ItemId> = self.scopes_of(a).into_iter().collect();
        let mut out: Vec<ItemId> = self
            .scopes_of(b)
            .into_iter()
            .filter(|s| sa.contains(s))
            .collect();
        out.sort();
        out
    }

    pub fn border_state(&self, child: &ItemId, parent: &ItemId) -> Option<BorderPairState> {
        let child = child.ontology_leaf();
        let parent = parent.ontology_leaf();
        let pair = (child.clone(), parent.clone());
        let weights = self.containment.get(&pair).cloned().unwrap_or_default();
        let c_w = weights.containment_weight();
        let b_w = self.borders.get(&pair).copied().unwrap_or(0);
        if c_w == 0 && b_w == 0 {
            return None;
        }
        let status = if c_w > b_w {
            MembershipStatus::Active
        } else {
            MembershipStatus::Suspended
        };
        Some(BorderPairState {
            child,
            parent,
            containment_weight: c_w,
            explicit: weights.explicit,
            sugar: weights.sugar,
            border_weight: b_w,
            status,
        })
    }

    pub fn fallen_borders(&self) -> &[FallenBorderEntry] {
        &self.fallen_border_journal
    }

    fn apply_containment_claim(
        &mut self,
        child: ItemId,
        parent: ItemId,
        border: bool,
        sugar: bool,
        ts: i64,
        ingest_id: &str,
    ) {
        let child = child.ontology_leaf();
        let parent = parent.ontology_leaf();
        if child == parent {
            return;
        }
        let pair = (child.clone(), parent.clone());
        let old_c = self
            .containment
            .get(&pair)
            .map(ContainmentWeights::containment_weight)
            .unwrap_or(0);
        let old_b = self.borders.get(&pair).copied().unwrap_or(0);
        let prev_holding = old_b > 0 && old_c <= old_b;

        if border {
            *self.borders.entry(pair.clone()).or_insert(0) += 1;
        } else if sugar {
            let w = self.containment.entry(pair.clone()).or_default();
            w.sugar = true;
        } else {
            self.containment.entry(pair.clone()).or_default().explicit += 1;
        }

        let new_c = self
            .containment
            .get(&pair)
            .map(ContainmentWeights::containment_weight)
            .unwrap_or(0);
        let new_b = self.borders.get(&pair).copied().unwrap_or(0);
        let now_active = new_c > new_b;

        if prev_holding && now_active {
            self.fallen_border_journal.push(FallenBorderEntry {
                ts,
                child: child.clone(),
                parent: parent.clone(),
                containment_weight: new_c,
                border_weight: new_b,
                ingest_id: ingest_id.to_string(),
            });
        }

        if now_active {
            self.members_by_scope
                .entry(parent.clone())
                .or_default()
                .insert(child.clone());
            self.scopes_by_member
                .entry(child.clone())
                .or_default()
                .insert(parent.clone());
            self.item_children.entry(parent).or_default().insert(child);
        } else {
            if let Some(set) = self.members_by_scope.get_mut(&parent) {
                set.remove(&child);
                if set.is_empty() {
                    self.members_by_scope.remove(&parent);
                }
            }
            if let Some(set) = self.scopes_by_member.get_mut(&child) {
                set.remove(&parent);
                if set.is_empty() {
                    self.scopes_by_member.remove(&child);
                }
            }
            // Tilde scopes are membership-only; drop the child when suspended.
            // URL scopes keep path-hierarchy children from add_child_edge.
            if parent.tilde_tail().is_some() || parent == ItemId::ontology_root() {
                if let Some(set) = self.item_children.get_mut(&parent) {
                    set.remove(&child);
                    if set.is_empty() {
                        self.item_children.remove(&parent);
                    }
                }
            }
        }
    }
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
    /// Private `(room_id, thread_tag)` pairs that were graduated to public.
    pub graduated_threads: HashSet<(String, String)>,
}

impl ReducerState {
    pub fn content_for_scope(&self, scope: &ScopeId) -> Option<&ContentState> {
        self.content.get(scope)
    }

    /// Most recent forum post bump in `room_id`, or room-created time if the room has no posts.
    pub fn room_last_activity_ts(&self, room_id: &str) -> i64 {
        let scope = ScopeId::Room(room_id.to_string());
        let mut max_ts = 0i64;
        for ((s, _), thread) in &self.forum_threads {
            if s == &scope {
                max_ts = max_ts.max(thread.last_activity_ts);
            }
        }
        if max_ts > 0 {
            return max_ts;
        }
        self.room_timeline
            .get(room_id)
            .and_then(|entries| entries.first().map(|e| e.ts))
            .unwrap_or(0)
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
            .expect("post_id must appear in ingests_by_scope_thread for this scope and thread")
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
                            ScopeId::Room(rid) => viewer
                                .is_some_and(|u| self.user_has_cap(rid, u, ThreadCapability::View)),
                        }
                    })
            })
            .collect()
    }

    /// Public, non-redacted forum posts split into human (no delegate) vs AI (`delegate` set).
    /// System principals (`system:…`) are omitted from both counts.
    pub fn public_post_stats(&self) -> PostStats {
        let mut stats = PostStats::default();
        for id in &self.ingests_ordered {
            if self.redacted_posts.contains(id) {
                continue;
            }
            let Some(ing) = self.ingests_by_id.get(id) else {
                continue;
            };
            if scope_from_room_wire(&ing.room_id) != ScopeId::Public {
                continue;
            }
            if ing.principal.starts_with("system:") {
                continue;
            }
            if ing.delegate.is_some() {
                stats.ai_posts += 1;
            } else {
                stats.human_posts += 1;
            }
        }
        stats
    }

    pub fn user_has_cap(&self, room_id: &str, username: &str, cap: ThreadCapability) -> bool {
        self.grants
            .get(room_id)
            .and_then(|t| t.get(username))
            .map(|caps| caps.contains(&cap))
            .unwrap_or(false)
    }

    pub fn is_thread_graduated(&self, room_id: &str, thread_tag: &str) -> bool {
        let tag = canonicalize_tag(thread_tag);
        self.graduated_threads
            .contains(&(room_id.trim().to_string(), tag))
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
        self.content
            .get(&ScopeId::Public)
            .expect("public scope missing")
    }

    /// Register parent→child edges for the full ancestor chain of **URL / external**
    /// items. Tilde-ontology hierarchy is containment (path sugar), not path identity.
    /// @e2bdefa9-a6fa-4725-b0a2-c0b09d95bb20:claudecode:anthropic/claude-opus-4
    fn add_child_edge(content: &mut ContentState, item: &ItemId) {
        if item.tilde_tail().is_some() {
            return;
        }
        let mut child = item.clone();
        loop {
            let Some(parent) = child.parent() else { break };
            let is_new = content
                .item_children
                .entry(parent.clone())
                .or_default()
                .insert(child);
            if !is_new {
                break;
            }
            child = parent;
        }
    }

    /// Resolve a DSL item ref. Tilde paths collapse to the leaf token.
    fn normalize_item(item: &str) -> Option<ItemId> {
        ItemId::parse(item).map(|id| id.ontology_leaf())
    }

    /// 1-indexed rank within its own connected component, for every active member of
    /// `scope`. Items with no votes connecting them to a sibling are absent.
    fn scope_positions(group: &GroupState, members: &HashSet<ItemId>) -> HashMap<ItemId, usize> {
        if members.is_empty() {
            return HashMap::new();
        }
        let children = members;
        let sibling_idxs: Vec<usize> = children
            .iter()
            .filter_map(|c| group.item_to_idx.get(c).copied())
            .collect();
        let global_to_local: HashMap<usize, usize> = sibling_idxs
            .iter()
            .enumerate()
            .map(|(l, &g)| (g, l))
            .collect();
        let (comps, _) = crate::ranking::connected_components_from_voted_pairs(
            sibling_idxs.len(),
            group.voted_pairs.iter().filter_map(|(a, b)| {
                Some((
                    global_to_local.get(a).copied()?,
                    global_to_local.get(b).copied()?,
                ))
            }),
        );
        let comps_global: Vec<Vec<usize>> = comps
            .iter()
            .map(|c| {
                c.iter()
                    .filter_map(|&l| sibling_idxs.get(l).copied())
                    .collect()
            })
            .collect();

        let mut out = HashMap::new();
        for ranked in crate::ranking::rank_partition(group, &comps_global, 10000, 1e-8) {
            for (i, r) in ranked.into_iter().enumerate() {
                out.insert(r.item, i + 1);
            }
        }
        out
    }

    /// 1-indexed position and component-local score in the component-aware
    /// global flat list. Components largest-first; items ranked within each
    /// component. Isolates / unvoted items are absent.
    ///
    /// The score is Rank Centrality mass within the item's own connected
    /// component — never a whole-graph solve that would mix disconnected
    /// clusters.
    fn global_positions(group: &GroupState) -> HashMap<ItemId, (usize, f64)> {
        let (mut comps, _) = crate::ranking::connected_components_from_voted_pairs(
            group.idx_to_item.len(),
            group.voted_pairs.iter().copied(),
        );
        comps.sort_by_key(|b| std::cmp::Reverse(b.len()));

        let mut out = HashMap::new();
        let mut pos = 1usize;
        for ranked in crate::ranking::rank_partition(group, &comps, 10000, 1e-8) {
            for r in ranked {
                out.insert(r.item, (pos, r.score));
                pos += 1;
            }
        }
        out
    }

    /// `(scope_rank, global_rank, score)` for each of `items`.
    /// Ranks are 0 where unranked; score is 0.0 where unranked.
    ///
    /// Rank history needs these for every item an ingest votes on. The whole
    /// global ordering (with component-local scores) is computed once, and each
    /// distinct parent scope once, no matter how many items the post touches.
    fn rank_positions_for(
        content: &mut ContentState,
        items: &[ItemId],
    ) -> HashMap<ItemId, (usize, usize, f64)> {
        if items.is_empty() {
            return HashMap::new();
        }

        let generation = content.ranking_group.generation;
        let stale = content
            .rank_position_cache
            .as_ref()
            .map_or(true, |cache| cache.generation != generation);
        if stale {
            let global = Self::global_positions(&content.ranking_group);
            #[cfg(test)]
            let recomputations = content
                .rank_position_cache
                .as_ref()
                .map_or(1, |cache| cache.recomputations + 1);
            content.rank_position_cache = Some(RankPositionCache {
                generation,
                global,
                by_parent: HashMap::new(),
                #[cfg(test)]
                recomputations,
            });
        }

        let scopes: HashSet<ItemId> = items
            .iter()
            .flat_map(|item| content.scopes_of(item))
            .collect();
        let missing_scopes: Vec<ItemId> = scopes
            .into_iter()
            .filter(|scope| {
                !content
                    .rank_position_cache
                    .as_ref()
                    .expect("cache initialized")
                    .by_parent
                    .contains_key(scope)
            })
            .collect();
        for scope in missing_scopes {
            let members: HashSet<ItemId> = content.members_of(&scope).into_iter().collect();
            let positions = Self::scope_positions(&content.ranking_group, &members);
            content
                .rank_position_cache
                .as_mut()
                .expect("cache initialized")
                .by_parent
                .insert(scope, positions);
        }

        let item_scopes: Vec<(ItemId, Vec<ItemId>)> = items
            .iter()
            .map(|item| (item.clone(), content.scopes_of(item)))
            .collect();
        let cache = content
            .rank_position_cache
            .as_ref()
            .expect("cache initialized");
        let mut content_positions = HashMap::with_capacity(items.len());
        for (item, scopes) in &item_scopes {
            let scope = scopes
                .iter()
                .find_map(|scope| {
                    cache
                        .by_parent
                        .get(scope)
                        .and_then(|positions| positions.get(item).copied())
                })
                .unwrap_or(0);
            let (global, score) = cache.global.get(item).copied().unwrap_or((0, 0.0));
            content_positions.insert(item.clone(), (scope, global, score));
        }
        content_positions
    }

    /// Apply one ingest's DSL effects to `content` (votes, items, snippets, rank history).
    fn apply_ingest_to_content(content: &mut ContentState, ing: &Ingest) -> Result<(), ()> {
        let doc = dsl::parse_full(&ing.raw).map_err(|_| ())?;
        let canonical_thread = canonicalize_tag(&ing.thread_tag);

        let voted_items: Vec<ItemId> = doc
            .statements
            .iter()
            .filter_map(|s| {
                if let dsl::Stmt::Vote {
                    item1,
                    item2,
                    aspect: None,
                    ..
                } = s
                {
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

        let before: HashMap<ItemId, (usize, usize, f64)> = if !voted_items.is_empty() {
            Self::rank_positions_for(content, &voted_items)
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
                            nav!(
                                content.item_bodies,
                                keypath(item.clone()),
                                setval(body_text)
                            );
                        }
                    }
                }
                dsl::Stmt::Containment {
                    child,
                    parent,
                    border,
                    sugar,
                    ..
                } => {
                    let Some(child_id) = Self::normalize_item(&child) else {
                        continue;
                    };
                    let Some(parent_id) = Self::normalize_item(&parent) else {
                        continue;
                    };
                    ingest_items.insert(child_id.clone());
                    ingest_items.insert(parent_id.clone());
                    content.apply_containment_claim(
                        child_id, parent_id, border, sugar, ing.ts, &ing.id,
                    );
                }
                dsl::Stmt::Aspect { slug, prompt } => {
                    if let (Some(s), Some(p)) = (slug, prompt) {
                        if !p.trim().is_empty() {
                            content.aspect_prompts.insert(s, p);
                        }
                    }
                }
                dsl::Stmt::Vote {
                    item1,
                    item2,
                    ratio_left,
                    ratio_right,
                    explanation,
                    aspect,
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

                    if let Some(asp) = aspect {
                        let shared = content.shared_scopes(&item_a, &item_b);
                        for scope in shared {
                            content
                                .aspect_groups
                                .entry((scope, asp.clone()))
                                .or_default()
                                .apply_vote(vote.clone());
                        }
                    } else {
                        content.ranking_group.apply_vote(vote.clone());

                        for it in [&item_a, &item_b] {
                            nav!(
                                content.item_votes,
                                keypath(it.clone()),
                                push_front(vote.clone())
                            );
                        }
                    }
                }
                dsl::Stmt::Prose { .. } => {}
            }
        }

        for item in ingest_items.iter() {
            nav!(
                content.item_snippets,
                keypath(item.clone()),
                push_front(ing.id.clone())
            );
        }

        for item in ingest_items.iter() {
            nav!(
                content.item_threads,
                keypath(item.clone()),
                set_elem(canonical_thread.clone())
            );
        }

        if !voted_items.is_empty() {
            let thread = canonical_thread.clone();
            let after = Self::rank_positions_for(content, &voted_items);
            for item in &voted_items {
                let (after_scope, after_global, score) =
                    after.get(item).copied().unwrap_or((0, 0, 0.0));
                let (before_scope, before_global, _) =
                    before.get(item).copied().unwrap_or((0, 0, 0.0));
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
                let scope_total = content
                    .scopes_of(item)
                    .first()
                    .map(|s| content.members_of(s).len())
                    .unwrap_or(0);
                let global_total = content.ranking_group.idx_to_item.len();
                content
                    .rank_history
                    .entry(item.clone())
                    .or_default()
                    .push(RankHistoryEntry {
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
                self.posts_by_actor.entry(ing.principal).and_modify(|q| {
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
                    self.room_timeline.entry(room_id.clone()).or_default().push(
                        RoomTimelineEntry {
                            ts: rd.ts,
                            kind: RoomTimelineKind::RoomDeleted {
                                deleted_by: rd.deleted_by.clone(),
                            },
                        },
                    );
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

                let ft = self
                    .forum_threads
                    .entry(scope_thread_key.clone())
                    .or_default();
                let prev_ts = ft.last_activity_ts;
                if ing.ts > prev_ts {
                    ft.last_activity_ts = ing.ts;
                    ft.last_actor = ing.principal.clone();
                }

                nav!(
                    self.ingests_by_scope_thread,
                    keypath(scope_thread_key),
                    push_front(ing.id.clone())
                );

                nav!(self.ingests_ordered, push_back(ing.id.clone()));

                nav!(
                    self.posts_by_actor,
                    keypath(ing.principal.clone()),
                    push_back(ing.id.clone())
                );

                nav!(
                    self.actor_last_post_ts,
                    keypath(ing.principal.clone()),
                    setval(ing.ts)
                );
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
                let caps = self
                    .grants
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
            Event::ThreadGraduated(tg) => {
                let room_id = tg.source_room_id.trim().to_string();
                let tag = canonicalize_tag(&tg.thread_tag);
                self.graduated_threads
                    .insert((room_id.clone(), tag.clone()));
                self.room_timeline
                    .entry(room_id)
                    .or_default()
                    .push(RoomTimelineEntry {
                        ts: tg.ts,
                        kind: RoomTimelineKind::ThreadGraduated {
                            thread_tag: tag,
                            graduated_by: tg.graduated_by.clone(),
                            posts_copied: tg.posts_copied,
                        },
                    });
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
            graduated_threads: HashSet::new(),
        }
    }
}

#[cfg(test)]
mod rank_position_cache_tests {
    use super::*;

    fn ingest(id: &str, raw: &str) -> Ingest {
        Ingest {
            ts: 1,
            id: id.to_string(),
            raw: raw.to_string(),
            principal: "tester".to_string(),
            delegate: None,
            room_id: "public".to_string(),
            thread_tag: "bench".to_string(),
        }
    }

    #[test]
    fn reuses_previous_after_positions_as_next_before_positions() {
        let mut content = ContentState::default();
        ReducerState::apply_ingest_to_content(
            &mut content,
            &ingest(
                "first",
                "~/memo/a { a }\n~/memo/b { b }\n{ a wins }\n~/memo/a 2:1 ~/memo/b",
            ),
        )
        .unwrap();

        let cache = content.rank_position_cache.as_ref().unwrap();
        assert_eq!(cache.generation, content.ranking_group.generation);
        // Initial empty "before", then the first vote's "after".
        assert_eq!(cache.recomputations, 2);

        ReducerState::apply_ingest_to_content(
            &mut content,
            &ingest("items-only", "~/memo/c { c }"),
        )
        .unwrap();
        assert_eq!(
            content.rank_position_cache.as_ref().unwrap().recomputations,
            2,
            "adding an isolate must not invalidate ranked positions"
        );

        ReducerState::apply_ingest_to_content(
            &mut content,
            &ingest("second", "{ b beats c }\n~/memo/b 2:1 ~/memo/c"),
        )
        .unwrap();
        let cache = content.rank_position_cache.as_ref().unwrap();
        assert_eq!(cache.generation, content.ranking_group.generation);
        assert_eq!(
            cache.recomputations, 3,
            "the second ingest must reuse its before positions and recompute only after voting"
        );
        assert_eq!(content.rank_history[&ItemId::parse("~b").unwrap()].len(), 2);
    }
}

#[cfg(test)]
mod aspect_tests {
    use super::*;
    use crate::events::PostRedacted;
    use crate::ranking::ranked_items;

    fn ingest(id: &str, raw: &str) -> Ingest {
        Ingest {
            ts: 1,
            id: id.to_string(),
            raw: raw.to_string(),
            principal: "tester".to_string(),
            delegate: None,
            room_id: "public".to_string(),
            thread_tag: "aspects".to_string(),
        }
    }

    fn parent_songs() -> ItemId {
        ItemId::parse("~/songs").unwrap()
    }

    const SETUP: &str = "\
~/songs/a { a }\n\
~/songs/b { b }\n\
{ canonical }\n\
~/songs/a 3:1 ~/songs/b\n\
:beauty { more beautiful }\n\
{ pretty }\n\
~/songs/a 2:1 ~/songs/b\n\
:speed { faster }\n\
{ zippy }\n\
~/songs/a 4:1 ~/songs/b\n";

    #[test]
    fn aspect_votes_create_separate_groups_and_leave_canonical_unchanged() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest("setup", SETUP)));

        let content = state.public();
        assert_eq!(content.ranking_group.voted_pairs.len(), 1);
        assert_eq!(content.ranking_group.recent_votes.len(), 1);
        assert_eq!(content.ranking_group.recent_votes[0].ratio_left, 3);
        assert_eq!(content.ranking_group.recent_votes[0].ratio_right, 1);

        let beauty = content
            .aspect_group(&parent_songs(), "beauty")
            .expect("beauty group");
        assert_eq!(beauty.voted_pairs.len(), 1);
        assert_eq!(beauty.recent_votes[0].ratio_left, 2);
        assert_eq!(beauty.recent_votes[0].ratio_right, 1);

        let speed = content
            .aspect_group(&parent_songs(), "speed")
            .expect("speed group");
        assert_eq!(speed.voted_pairs.len(), 1);
        assert_eq!(speed.recent_votes[0].ratio_left, 4);
        assert_eq!(speed.recent_votes[0].ratio_right, 1);

        assert_eq!(content.aspect_prompt("beauty"), Some("more beautiful"));
        assert_eq!(content.aspect_prompt("speed"), Some("faster"));
        assert!(content.item_votes.values().all(|votes| {
            votes
                .iter()
                .all(|v| v.ratio_left == 3 && v.ratio_right == 1)
        }));
    }

    #[test]
    fn aspect_prompt_overwrite_matches_item_body_last_write() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest(
            "first",
            ":beauty { first prompt }\n~/songs/a { a }",
        )));
        state.apply_event(Event::Ingest(ingest("second", ":beauty { second prompt }")));
        assert_eq!(
            state.public().aspect_prompt("beauty"),
            Some("second prompt")
        );
        state.apply_event(Event::Ingest(ingest("empty", ":beauty {   }")));
        assert_eq!(
            state.public().aspect_prompt("beauty"),
            Some("second prompt")
        );
    }

    #[test]
    fn redacting_post_with_aspect_votes_removes_them() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest(
            "items",
            "~/songs/a { a }\n~/songs/b { b }\n{ canonical }\n~/songs/a 3:1 ~/songs/b",
        )));
        state.apply_event(Event::Ingest(ingest(
            "aspects",
            ":beauty\n{ pretty }\n~/songs/a 2:1 ~/songs/b",
        )));
        assert!(state
            .public()
            .aspect_group(&parent_songs(), "beauty")
            .is_some());
        assert_eq!(state.public().ranking_group.voted_pairs.len(), 1);

        state.apply_event(Event::PostRedacted(PostRedacted {
            ts: 2,
            post_id: "aspects".to_string(),
            principal: "tester".to_string(),
        }));

        let content = state.public();
        assert!(
            content.aspect_group(&parent_songs(), "beauty").is_none(),
            "redacted aspect votes must leave the group"
        );
        assert!(content.aspect_prompt("beauty").is_none());
        assert_eq!(content.ranking_group.voted_pairs.len(), 1);
        assert_eq!(content.ranking_group.recent_votes[0].ratio_left, 3);
    }

    #[test]
    fn replay_determinism_same_events_same_rankings() {
        let events = [
            Event::Ingest(ingest("setup", SETUP)),
            Event::Ingest(ingest(
                "more",
                ":\n{ more canonical }\n~/songs/b 2:1 ~/songs/a",
            )),
        ];
        let mut a = ReducerState::default();
        let mut b = ReducerState::default();
        for ev in &events {
            a.apply_event(ev.clone());
            b.apply_event(ev.clone());
        }

        let ca = a.public();
        let cb = b.public();
        assert_eq!(ca.ranking_group.idx_to_item, cb.ranking_group.idx_to_item);
        assert_eq!(ca.ranking_group.voted_pairs, cb.ranking_group.voted_pairs);
        assert_eq!(ca.aspect_groups.len(), cb.aspect_groups.len());
        for (key, ga) in &ca.aspect_groups {
            let gb = cb.aspect_groups.get(key).expect("matching aspect group");
            assert_eq!(ga.idx_to_item, gb.idx_to_item);
            assert_eq!(ga.voted_pairs, gb.voted_pairs);
            let mut ra = ga.clone();
            let mut rb = gb.clone();
            let la = ranked_items(&mut ra, 20000, 1e-9);
            let lb = ranked_items(&mut rb, 20000, 1e-9);
            assert_eq!(
                la.iter().map(|r| r.item.as_str()).collect::<Vec<_>>(),
                lb.iter().map(|r| r.item.as_str()).collect::<Vec<_>>()
            );
        }
        assert_eq!(ca.aspect_prompts, cb.aspect_prompts);
    }

    #[test]
    fn thread_graduation_reparse_inherits_aspects() {
        let raw = SETUP;
        let mut private = ingest("priv", raw);
        private.room_id = "aa11bb/studio".to_string();
        let mut public = ingest("pub", raw);
        public.room_id = "public".to_string();

        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(private));
        state.apply_event(Event::Ingest(public));

        let priv_content = state
            .content_for_scope(&ScopeId::Room("aa11bb/studio".into()))
            .expect("private scope");
        let pub_content = state.public();
        assert!(priv_content
            .aspect_group(&parent_songs(), "beauty")
            .is_some());
        assert!(pub_content
            .aspect_group(&parent_songs(), "beauty")
            .is_some());
        assert_eq!(
            priv_content.aspect_prompt("beauty"),
            pub_content.aspect_prompt("beauty")
        );
    }
}

#[cfg(test)]
mod containment_tests {
    use super::*;
    use crate::events::PostRedacted;

    fn ingest_at(id: &str, ts: i64, raw: &str) -> Ingest {
        Ingest {
            ts,
            id: id.to_string(),
            raw: raw.to_string(),
            principal: "tester".to_string(),
            delegate: None,
            room_id: "public".to_string(),
            thread_tag: "contain".to_string(),
        }
    }

    fn item(s: &str) -> ItemId {
        ItemId::parse(s).unwrap().ontology_leaf()
    }

    #[test]
    fn activation_when_containment_beats_border() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at(
            "in",
            1,
            "~luke { l }\n~jedi { j }\n{ in }\n~luke <: ~jedi",
        )));
        let c = state.public();
        assert_eq!(c.members_of(&item("~jedi")), vec![item("~luke")]);
        assert_eq!(c.scopes_of(&item("~luke")), vec![item("~jedi")]);
        let st = c.border_state(&item("~luke"), &item("~jedi")).unwrap();
        assert_eq!(st.status, MembershipStatus::Active);
        assert_eq!(st.containment_weight, 1);
        assert_eq!(st.border_weight, 0);
    }

    #[test]
    fn suspension_at_equality() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at(
            "in",
            1,
            "~luke { l }\n~jedi { j }\n{ in }\n~luke <: ~jedi",
        )));
        state.apply_event(Event::Ingest(ingest_at(
            "out",
            2,
            "{ out }\n~luke !<: ~jedi",
        )));
        let c = state.public();
        assert!(c.members_of(&item("~jedi")).is_empty());
        let st = c.border_state(&item("~luke"), &item("~jedi")).unwrap();
        assert_eq!(st.status, MembershipStatus::Suspended);
        assert_eq!(st.containment_weight, 1);
        assert_eq!(st.border_weight, 1);
        assert!(c.fallen_borders().is_empty());
    }

    #[test]
    fn breach_journals_when_containment_overtakes_holding_border() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at(
            "in",
            1,
            "~luke { l }\n~jedi { j }\n{ in }\n~luke <: ~jedi",
        )));
        state.apply_event(Event::Ingest(ingest_at(
            "out",
            2,
            "{ out }\n~luke !<: ~jedi",
        )));
        state.apply_event(Event::Ingest(ingest_at(
            "in2",
            3,
            "{ still in }\n~luke <: ~jedi",
        )));
        let c = state.public();
        let st = c.border_state(&item("~luke"), &item("~jedi")).unwrap();
        assert_eq!(st.status, MembershipStatus::Active);
        assert_eq!(st.containment_weight, 2);
        assert_eq!(st.border_weight, 1);
        assert_eq!(c.members_of(&item("~jedi")), vec![item("~luke")]);
        assert_eq!(c.fallen_borders().len(), 1);
        let j = &c.fallen_borders()[0];
        assert_eq!(j.ts, 3);
        assert_eq!(j.ingest_id, "in2");
        assert_eq!(j.containment_weight, 2);
        assert_eq!(j.border_weight, 1);
        assert_eq!(j.child, item("~luke"));
        assert_eq!(j.parent, item("~jedi"));
    }

    #[test]
    fn sugar_weights_are_idempotent() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at("one", 1, "~/jedi/luke { l }")));
        state.apply_event(Event::Ingest(ingest_at(
            "two",
            2,
            "~/jedi/luke { l again }",
        )));
        let c = state.public();
        let st = c.border_state(&item("~luke"), &item("~jedi")).unwrap();
        assert_eq!(st.containment_weight, 1);
        assert_eq!(st.status, MembershipStatus::Active);
        assert_eq!(
            c.item_bodies.get(&item("~luke")).map(String::as_str),
            Some("l again")
        );
    }

    #[test]
    fn explicit_weights_accumulate() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at(
            "a",
            1,
            "~luke { l }\n~jedi { j }\n{ in }\n~luke <: ~jedi",
        )));
        state.apply_event(Event::Ingest(ingest_at(
            "b",
            2,
            "{ still }\n~luke <: ~jedi",
        )));
        let st = state
            .public()
            .border_state(&item("~luke"), &item("~jedi"))
            .unwrap();
        assert_eq!(st.containment_weight, 2);
    }

    #[test]
    fn leaf_collision_merges_two_old_paths() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at("x", 1, "~/x/luke { from x }")));
        state.apply_event(Event::Ingest(ingest_at("y", 2, "~/y/luke { from y }")));
        let c = state.public();
        assert!(c.items.contains(&item("~luke")));
        assert!(!c.items.contains(&ItemId::parse("~/x/luke").unwrap()));
        assert_eq!(
            c.item_bodies.get(&item("~luke")).map(String::as_str),
            Some("from y")
        );
        assert_eq!(c.members_of(&item("~x")), vec![item("~luke")]);
        assert_eq!(c.members_of(&item("~y")), vec![item("~luke")]);
        let mut scopes = c.scopes_of(&item("~luke"));
        scopes.sort();
        assert_eq!(scopes, vec![item("~x"), item("~y")]);
    }

    #[test]
    fn replay_determinism_containment_and_journal() {
        let events = [
            Event::Ingest(ingest_at(
                "in",
                1,
                "~luke { l }\n~jedi { j }\n{ in }\n~luke <: ~jedi",
            )),
            Event::Ingest(ingest_at("out", 2, "{ out }\n~luke !<: ~jedi")),
            Event::Ingest(ingest_at("in2", 3, "{ still }\n~luke <: ~jedi")),
            Event::Ingest(ingest_at("path", 4, "~/jedi/obiwan { o }")),
        ];
        let mut a = ReducerState::default();
        let mut b = ReducerState::default();
        for ev in &events {
            a.apply_event(ev.clone());
            b.apply_event(ev.clone());
        }
        let ca = a.public();
        let cb = b.public();
        assert_eq!(ca.containment, cb.containment);
        assert_eq!(ca.borders, cb.borders);
        assert_eq!(ca.members_by_scope, cb.members_by_scope);
        assert_eq!(ca.fallen_border_journal, cb.fallen_border_journal);
        assert_eq!(ca.members_of(&item("~jedi")), cb.members_of(&item("~jedi")));
    }

    #[test]
    fn redaction_rebuilds_containment() {
        let mut state = ReducerState::default();
        state.apply_event(Event::Ingest(ingest_at(
            "keep",
            1,
            "~luke { l }\n~jedi { j }\n{ in }\n~luke <: ~jedi",
        )));
        state.apply_event(Event::Ingest(ingest_at(
            "drop",
            2,
            "{ out }\n~luke !<: ~jedi",
        )));
        assert_eq!(
            state
                .public()
                .border_state(&item("~luke"), &item("~jedi"))
                .unwrap()
                .status,
            MembershipStatus::Suspended
        );
        state.apply_event(Event::PostRedacted(PostRedacted {
            ts: 3,
            post_id: "drop".to_string(),
            principal: "tester".to_string(),
        }));
        let c = state.public();
        assert_eq!(
            c.border_state(&item("~luke"), &item("~jedi"))
                .unwrap()
                .status,
            MembershipStatus::Active
        );
        assert!(c.fallen_borders().is_empty());
    }
}
