use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::events::{
    canonicalize_actor, canonicalize_aspect, canonicalize_item, canonicalize_tag, item_parent_path,
    item_thread, Event, Ingest,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub tag: String,
    pub aspect: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemKey {
    pub tag: String,
    pub item: String,
}

/// Parsed vote data (not an event - just internal representation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteData {
    pub ts: i64,
    pub tag: String,
    pub aspect: String,
    pub a: String,
    pub b: String,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub body: String,
    pub actor: String,
}

#[derive(Debug, Clone)]
pub struct GroupState {
    pub key: GroupKey,

    pub item_to_idx: HashMap<String, usize>,
    pub idx_to_item: Vec<String>,

    /// Aggregated directed edge weights: (src_idx, dst_idx) -> weight.
    pub edges: HashMap<(usize, usize), f64>,

    /// Unordered pairs that have at least one vote recorded between them (i<j).
    ///
    /// We cannot reliably infer this from `edges` alone because extreme votes can
    /// produce a zero-weight edge in one direction.
    pub voted_pairs: HashSet<(usize, usize)>,

    pub dirty: bool,
    pub cached_scores: Vec<f64>,

    pub recent_votes: VecDeque<VoteData>,
}

impl GroupState {
    pub fn new(tag: String, aspect: String) -> Self {
        Self {
            key: GroupKey { tag, aspect },
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

    fn add_edge_weight(&mut self, src: usize, dst: usize, w: f64) {
        if w <= 0.0 {
            return;
        }
        *self.edges.entry((src, dst)).or_insert(0.0) += w;
        self.dirty = true;
    }

    pub fn apply_vote(&mut self, mut vote: VoteData) {
        // Canonicalize identifiers.
        vote.tag = canonicalize_tag(&vote.tag);
        vote.aspect = canonicalize_aspect(&vote.aspect);
        vote.a = canonicalize_item(&vote.a);
        vote.b = canonicalize_item(&vote.b);
        vote.actor = canonicalize_actor(&vote.actor);
        if vote.ratio_left < 0 {
            vote.ratio_left = 0;
        }
        if vote.ratio_right < 0 {
            vote.ratio_right = 0;
        }
        // Ratios are the only supported vote representation.

        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        // Track that this unordered pair has been voted on (at least once).
        let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
        self.voted_pairs.insert((i, j));

        // Edge weights represent how much probability mass should flow from loser -> winner.
        // For `/a 3:1 /b`, we add flow b->a by 3 and flow a->b by 1.
        let mut w_a = vote.ratio_left.max(0) as f64;
        let mut w_b = vote.ratio_right.max(0) as f64;
        // Avoid 0:0 degenerate case.
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

/// Notification types for threading/agent coordination.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationType {
    /// Activity in a thread you're subscribed to.
    ThreadActivity {
        thread: String,
        activity: String, // "vote" or "ingest"
        actor: String,
        details: String, // vote body or ingest snippet
    },
}

/// A notification for an actor about activity related to their contributions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Notification {
    /// Unix timestamp in milliseconds when notification was generated.
    pub ts: i64,
    /// ID of the ingest that triggered this notification.
    pub ingest_id: String,
    /// Actor who performed the action.
    pub actor: String,
    /// Type of notification.
    #[serde(flatten)]
    pub notification_type: NotificationType,
}

/// First-class thread state. Tracks bump order and subscriber count.
/// Threads are the forum layer — topic-based sessions, bump-ordered.
/// Items (`~/path/item`) are the ontology layer — persistent, accumulated.
#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    /// Unix timestamp (ms) of the most recent ingest touching this thread.
    pub last_activity_ts: i64,
    /// Number of distinct actors who have participated.
    pub subscriber_count: usize,
}

#[derive(Debug, Default, Clone)]
pub struct ReducerState {
    pub groups: HashMap<GroupKey, GroupState>,
    pub items: HashSet<String>,
    pub tags: HashMap<String, HashSet<String>>, // tag -> set(item)
    pub item_bodies: HashMap<String, String>,
    /// Parent -> direct children index for nested item paths.
    pub item_children: HashMap<String, HashSet<String>>,
    /// Store ingests by id so we can render snippets without duplicating raw text in indexes.
    pub ingests_by_id: HashMap<String, Ingest>,
    /// Recent ingest ids per tag (most recent first).
    pub ingests_by_tag: HashMap<String, VecDeque<String>>,

    /// Per-(tag,item) vote history (most recent first), across all aspects.
    pub item_votes: HashMap<ItemKey, VecDeque<VoteData>>,

    /// Per-(tag,item) ingest references (most recent first).
    /// This is *unbounded* by design; memory usage is dominated by `ingests_by_id`.
    pub item_snippets: HashMap<ItemKey, VecDeque<String>>,

    /// First-class thread state: bump time, subscriber count.
    pub threads: HashMap<String, ThreadState>,

    /// Track thread subscriptions: thread -> set(actor)
    /// Actors are subscribed when they vote or ingest in a thread.
    pub thread_subscriptions: HashMap<String, HashSet<String>>,

    /// Pending notifications per actor (last 100 per actor).
    pub notifications: HashMap<String, VecDeque<Notification>>, // actor -> queue
}

impl ReducerState {
    fn add_child_edge(&mut self, tag: &str, item: &str) {
        let parent = item_parent_path(item).unwrap_or_else(|| tag.to_string());
        self.item_children
            .entry(parent)
            .or_default()
            .insert(item.to_string());
    }

    fn normalize_item_for_context(item: &str, current_tag: &Option<String>) -> Option<(String, String)> {
        let c = canonicalize_item(item);
        if c.is_empty() {
            return None;
        }
        // Only treat the first segment as the tag if the path has >= 2 segments.
        // A single-segment path like "a" (from `/a`) must use the #tag context —
        // otherwise item_thread("a") would return Some("a") and create a spurious tag.
        let has_explicit_thread = c.contains('/');
        if has_explicit_thread {
            if let Some(tag) = item_thread(&c) {
                return Some((tag, c));
            }
        }
        current_tag
            .as_ref()
            .map(|t| (t.clone(), format!("{}/{}", t, c)))
    }

    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Ingest(mut ing) => {
                // Canonicalize actor for indexing.
                ing.actor = canonicalize_actor(&ing.actor);

                // Keep full ingest once (indexed by id). Other indexes should reference this id.
                self.ingests_by_id.insert(ing.id.clone(), ing.clone());

                // Parse DSL to extract all statements.
                let doc = match crate::dsl::parse_full(&ing.raw) {
                    Ok(d) => d,
                    Err(e) => {
                        // Skip malformed events during reduction (shouldn't happen in valid JSONL)
                        eprintln!("WARNING: Skipping malformed ingest event {}: {}", ing.id, e);
                        return;
                    }
                };

                // Track current context for parsing.
                let mut current_tag: Option<String> = None;
                let mut current_aspect: String = "default".to_string();
                let mut current_actor: Option<String> = Some(ing.actor.clone());

                let mut extracted_tags: HashSet<String> = HashSet::new();

                // Process each statement.
                // We only index snippets for items that appear in explicit DSL (`! /item` or votes).
                // (No prose-token matching.)
                let mut ingest_items: HashSet<String> = HashSet::new();
                for stmt in doc.statements {
                    match stmt {
                        crate::dsl::Stmt::Hashtag { name } => {
                            current_tag = Some(canonicalize_tag(&name));
                        }
                        crate::dsl::Stmt::Attribute { name } => {
                            current_aspect = canonicalize_aspect(&name);
                        }
                        crate::dsl::Stmt::Actor { name } => {
                            current_actor = Some(canonicalize_actor(&name));
                        }
                        crate::dsl::Stmt::Item { title, body } => {
                            let Some((tag, item)) =
                                Self::normalize_item_for_context(&title, &current_tag)
                            else {
                                continue;
                            };
                            self.items.insert(item.clone());
                            ingest_items.insert(item.clone());
                            extracted_tags.insert(tag.clone());

                            // Store body if provided.
                            if let Some(body_text) = body {
                                if !body_text.trim().is_empty() {
                                    self.item_bodies.insert(item.clone(), body_text);
                                }
                            }

                            self.tags.entry(tag.clone()).or_default().insert(item.clone());
                            self.add_child_edge(&tag, &item);
                        }
                        crate::dsl::Stmt::Vote {
                            item1,
                            item2,
                            ratio_left,
                            ratio_right,
                            explanation,
                        } => {
                            let Some((tag_a, item_a)) =
                                Self::normalize_item_for_context(&item1, &current_tag)
                            else {
                                continue;
                            };
                            let Some((tag_b, item_b)) =
                                Self::normalize_item_for_context(&item2, &current_tag)
                            else {
                                continue;
                            };
                            if tag_a != tag_b {
                                continue;
                            }
                            let tag = tag_a;
                            extracted_tags.insert(tag.clone());
                            let actor = current_actor.clone().unwrap_or_else(|| ing.actor.clone());
                            let vote = VoteData {
                                ts: ing.ts,
                                tag: tag.clone(),
                                aspect: current_aspect.clone(),
                                a: item_a,
                                b: item_b,
                                ratio_left,
                                ratio_right,
                                body: explanation,
                                actor,
                            };

                            ingest_items.insert(vote.a.clone());
                            ingest_items.insert(vote.b.clone());

                            // Ensure items exist under tag.
                            self.items.insert(vote.a.clone());
                            self.items.insert(vote.b.clone());
                            self.tags
                                .entry(tag.clone())
                                .or_default()
                                .insert(vote.a.clone());
                            self.tags
                                .entry(tag.clone())
                                .or_default()
                                .insert(vote.b.clone());
                            self.add_child_edge(&tag, &vote.a);
                            self.add_child_edge(&tag, &vote.b);

                            // Apply to group.
                            let key = GroupKey {
                                tag: tag.clone(),
                                aspect: current_aspect.clone(),
                            };
                            let group = self.groups.entry(key.clone()).or_insert_with(|| {
                                GroupState::new(key.tag.clone(), key.aspect.clone())
                            });
                            group.apply_vote(vote.clone());

                            // Index vote by (tag,item) for both items.
                            // Also notify actors who have previously voted on the same items.
                            for it in [&vote.a, &vote.b] {
                                let ik = ItemKey {
                                    tag: tag.clone(),
                                    item: it.clone(),
                                };
                                // Collect prior voters before mutating the queue.
                                let prior_voters: Vec<String> = self
                                    .item_votes
                                    .get(&ik)
                                    .map(|q| {
                                        q.iter()
                                            .map(|v| v.actor.clone())
                                            .filter(|a| a != &vote.actor)
                                            .collect::<std::collections::HashSet<_>>()
                                            .into_iter()
                                            .collect()
                                    })
                                    .unwrap_or_default();

                                let q = self.item_votes.entry(ik).or_default();
                                q.push_front(vote.clone());

                                // Notify prior voters about new activity on this item.
                                let detail = format!(
                                    "{} {} {}:{} {} {}",
                                    vote.actor, it,
                                    vote.ratio_left, vote.ratio_right,
                                    vote.a, vote.b
                                );
                                for prior_actor in prior_voters {
                                    let notification = Notification {
                                        ts: ing.ts,
                                        ingest_id: ing.id.clone(),
                                        actor: vote.actor.clone(),
                                        notification_type: NotificationType::ThreadActivity {
                                            thread: format!("#{}", tag),
                                            activity: "vote".to_string(),
                                            actor: vote.actor.clone(),
                                            details: detail.clone(),
                                        },
                                    };
                                    let queue = self.notifications.entry(prior_actor).or_default();
                                    queue.push_front(notification);
                                    while queue.len() > 100 {
                                        queue.pop_back();
                                    }
                                }
                            }
                        }
                        crate::dsl::Stmt::Prose { .. } => {
                            // Ignore prose for indexing purposes.
                        }
                    }
                }

                // Index full snippets by (tag,item) for this ingest.
                if let Some(ref tag) = current_tag {
                    let tag = canonicalize_tag(tag);
                    for item in ingest_items.into_iter() {
                        let ik = ItemKey {
                            tag: tag.clone(),
                            item: item.clone(),
                        };
                        let q = self.item_snippets.entry(ik).or_default();
                        q.push_front(ing.id.clone());
                    }
                }

                // Bump thread state for each extracted tag.
                for tag in &extracted_tags {
                    let thread = self.threads.entry(tag.clone()).or_default();
                    if ing.ts > thread.last_activity_ts {
                        thread.last_activity_ts = ing.ts;
                    }
                }

                // Thread subscription and notification for each extracted tag.
                let snippet = ing.raw.chars().take(200).collect::<String>();
                for tag in &extracted_tags {
                    // Get current subscribers before adding this one.
                    let subscribers = self
                        .thread_subscriptions
                        .get(tag)
                        .cloned()
                        .unwrap_or_default();

                    // Notify all current subscribers (except self).
                    for subscriber in subscribers.iter() {
                        if subscriber != &ing.actor {
                            let notification = Notification {
                                ts: ing.ts,
                                ingest_id: ing.id.clone(),
                                actor: ing.actor.clone(),
                                notification_type: NotificationType::ThreadActivity {
                                    thread: format!("#{}", tag),
                                    activity: "ingest".to_string(),
                                    actor: ing.actor.clone(),
                                    details: snippet.clone(),
                                },
                            };

                            let queue = self.notifications.entry(subscriber.clone()).or_default();
                            queue.push_front(notification);
                            while queue.len() > 100 {
                                queue.pop_back();
                            }
                        }
                    }

                    // Subscribe this actor to the thread.
                    let subs = self.thread_subscriptions
                        .entry(tag.clone())
                        .or_default();
                    subs.insert(ing.actor.clone());
                    // Keep subscriber_count in sync.
                    if let Some(thread) = self.threads.get_mut(tag) {
                        thread.subscriber_count = subs.len();
                    }
                }

                // Index ingest by extracted tags.
                for tag in extracted_tags {
                    let q = self.ingests_by_tag.entry(tag).or_default();
                    q.push_front(ing.id.clone());
                    while q.len() > 25 {
                        q.pop_back();
                    }
                }
            }
        }
    }
}


