use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
pub use slug_types::{Notification, NotificationType};

use crate::events::{
    canonicalize_actor, canonicalize_aspect, canonicalize_item, canonicalize_tag,
    item_parent_path, Event, Ingest,
};

/// Key identifying a ranking group: one aspect across all item paths.
/// Items that have been compared in this aspect form connected components;
/// the rank-centrality algorithm ranks within each component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub aspect: String,
}

/// Parsed vote data (internal representation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteData {
    pub ts: i64,
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
    pub voted_pairs: HashSet<(usize, usize)>,

    pub dirty: bool,
    pub cached_scores: Vec<f64>,

    pub recent_votes: VecDeque<VoteData>,
}

impl GroupState {
    pub fn new(aspect: String) -> Self {
        Self {
            key: GroupKey { aspect },
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

/// First-class thread state. Tracks bump order and subscriber count.
#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    pub last_activity_ts: i64,
    pub subscriber_count: usize,
}

#[derive(Debug, Default, Clone)]
pub struct ReducerState {
    /// Per-aspect ranking groups. Items across all paths are globally rankable
    /// within an aspect; connected components naturally separate unrelated items.
    pub groups: HashMap<GroupKey, GroupState>,

    pub items: HashSet<String>,
    pub item_bodies: HashMap<String, String>,
    /// Parent path -> direct children. Root items have parent "".
    pub item_children: HashMap<String, HashSet<String>>,

    pub ingests_by_id: HashMap<String, Ingest>,
    /// Thread -> recent ingest ids (most recent first).
    pub ingests_by_thread: HashMap<String, VecDeque<String>>,

    /// Per-item vote history (most recent first), across all aspects.
    pub item_votes: HashMap<String, VecDeque<VoteData>>,

    /// Per-item ingest references (most recent first).
    pub item_snippets: HashMap<String, VecDeque<String>>,

    /// First-class thread state: bump time, subscriber count.
    pub threads: HashMap<String, ThreadState>,

    /// Track thread subscriptions: thread -> set(actor)
    pub thread_subscriptions: HashMap<String, HashSet<String>>,

    /// Pending notifications per actor (last 100 per actor).
    pub notifications: HashMap<String, VecDeque<Notification>>,
}

impl ReducerState {
    /// Register parent→child edge. Root-level items (single segment) get parent "".
    fn add_child_edge(&mut self, item: &str) {
        let parent = item_parent_path(item).unwrap_or_default();
        self.item_children
            .entry(parent)
            .or_default()
            .insert(item.to_string());
    }

    /// Resolve an item path as a first-class canonical path.
    fn normalize_item(item: &str) -> Option<String> {
        let c = canonicalize_item(item);
        if c.is_empty() {
            return None;
        }
        Some(c)
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

                let mut current_thread: Option<String> = None;
                let mut current_aspect: String = "default".to_string();
                let mut current_actor: Option<String> = Some(ing.actor.clone());

                // Threads explicitly declared with #tag in this ingest.
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
                        crate::dsl::Stmt::Attribute { name } => {
                            current_aspect = canonicalize_aspect(&name);
                        }
                        crate::dsl::Stmt::Actor { name } => {
                            current_actor = Some(canonicalize_actor(&name));
                        }
                        crate::dsl::Stmt::Item { title, body } => {
                            let Some(item) = Self::normalize_item(&title) else {
                                continue;
                            };
                            self.items.insert(item.clone());
                            ingest_items.insert(item.clone());
                            self.add_child_edge(&item);

                            if let Some(body_text) = body {
                                if !body_text.trim().is_empty() {
                                    self.item_bodies.insert(item.clone(), body_text);
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
                                aspect: current_aspect.clone(),
                                a: item_a.clone(),
                                b: item_b.clone(),
                                ratio_left,
                                ratio_right,
                                body: explanation,
                                actor,
                            };

                            ingest_items.insert(item_a.clone());
                            ingest_items.insert(item_b.clone());

                            self.items.insert(item_a.clone());
                            self.items.insert(item_b.clone());
                            self.add_child_edge(&item_a);
                            self.add_child_edge(&item_b);

                            let key = GroupKey {
                                aspect: current_aspect.clone(),
                            };
                            let group = self.groups.entry(key.clone()).or_insert_with(|| {
                                GroupState::new(key.aspect.clone())
                            });
                            group.apply_vote(vote.clone());

                            // Index vote by item and notify prior voters.
                            for it in [&item_a, &item_b] {
                                let prior_voters: Vec<String> = self
                                    .item_votes
                                    .get(it)
                                    .map(|q| {
                                        q.iter()
                                            .map(|v| v.actor.clone())
                                            .filter(|a| a != &vote.actor)
                                            .collect::<HashSet<_>>()
                                            .into_iter()
                                            .collect()
                                    })
                                    .unwrap_or_default();

                                let q = self.item_votes.entry(it.clone()).or_default();
                                q.push_front(vote.clone());

                                let detail = format!(
                                    "{} {} {}:{} {} {}",
                                    vote.actor, it,
                                    vote.ratio_left, vote.ratio_right,
                                    vote.a, vote.b
                                );
                                for prior_actor in prior_voters {
                                    // Notify about vote on items they've previously voted on.
                                    // Thread attribution uses explicit ingest thread context when present.
                                    let thread_label = current_thread
                                        .as_deref()
                                        .unwrap_or("unknown");
                                    let notification = Notification {
                                        ts: ing.ts,
                                        ingest_id: ing.id.clone(),
                                        actor: vote.actor.clone(),
                                        notification_type: NotificationType::ThreadActivity {
                                            thread: format!("#{}", thread_label),
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
                        crate::dsl::Stmt::Prose { .. } => {}
                    }
                }

                // Index snippets by item for this ingest.
                for item in ingest_items.iter() {
                    let q = self.item_snippets.entry(item.clone()).or_default();
                    q.push_front(ing.id.clone());
                }

                // Bump thread state and subscriptions for explicitly declared threads.
                let snippet = ing.raw.chars().take(200).collect::<String>();
                for thread in &touched_threads {
                    let state = self.threads.entry(thread.clone()).or_default();
                    if ing.ts > state.last_activity_ts {
                        state.last_activity_ts = ing.ts;
                    }
                }

                for thread in &touched_threads {
                    let subscribers = self
                        .thread_subscriptions
                        .get(thread)
                        .cloned()
                        .unwrap_or_default();

                    for subscriber in subscribers.iter() {
                        if subscriber != &ing.actor {
                            let notification = Notification {
                                ts: ing.ts,
                                ingest_id: ing.id.clone(),
                                actor: ing.actor.clone(),
                                notification_type: NotificationType::ThreadActivity {
                                    thread: format!("#{}", thread),
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

                    let subs = self.thread_subscriptions
                        .entry(thread.clone())
                        .or_default();
                    subs.insert(ing.actor.clone());
                    if let Some(ts) = self.threads.get_mut(thread) {
                        ts.subscriber_count = subs.len();
                    }
                }

                // Index ingest by touched threads.
                for thread in touched_threads {
                    let q = self.ingests_by_thread.entry(thread).or_default();
                    q.push_front(ing.id.clone());
                    while q.len() > 25 {
                        q.pop_back();
                    }
                }
            }
        }
    }
}
