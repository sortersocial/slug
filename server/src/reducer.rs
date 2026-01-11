use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::events::{
    canonicalize_actor, canonicalize_aspect, canonicalize_item, canonicalize_tag, DslIngested, Event,
    VoteCast,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub tag: String,
    pub aspect: String,
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

    pub recent_votes: VecDeque<VoteCast>,
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

    pub fn apply_vote(&mut self, mut vote: VoteCast) {
        // Canonicalize identifiers.
        vote.tag = canonicalize_tag(&vote.tag);
        vote.aspect = canonicalize_aspect(&vote.aspect);
        vote.a = canonicalize_item(&vote.a);
        vote.b = canonicalize_item(&vote.b);
        vote.actor = vote.actor.as_ref().map(|a| canonicalize_actor(a));
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
    /// Someone voted against your position on an item (THE HOOK).
    ItemCountered {
        item: String,
        opponent: String,
        body: String,
        ratio: String,
    },
    /// Someone referenced your ingest in their document.
    IngestQuoted { ingest_id: String },
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

#[derive(Debug, Default, Clone)]
pub struct ReducerState {
    pub groups: HashMap<GroupKey, GroupState>,
    pub items: HashSet<String>,
    pub tags: HashMap<String, HashSet<String>>, // tag -> set(item)
    pub item_bodies: HashMap<String, String>,
    pub ingests_by_tag: HashMap<String, VecDeque<DslIngested>>,

    /// Track edge stances for counter-detection: (item_a, item_b) -> (actor -> direction)
    /// where item_a < item_b lexicographically (normalized).
    /// direction: 1 = prefers item_a, -1 = prefers item_b, 0 = tie
    pub edge_stances: HashMap<(String, String), HashMap<String, i8>>,

    /// Pending notifications per actor (last 100 per actor).
    pub notifications: HashMap<String, VecDeque<Notification>>, // actor -> queue
}

impl ReducerState {
    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::VoteCast(v) => {
                let tag = canonicalize_tag(&v.tag);
                let aspect = canonicalize_aspect(&v.aspect);
                let a_canon = canonicalize_item(&v.a);
                let b_canon = canonicalize_item(&v.b);

                // Counter-detection: normalize edge and determine direction
                if let Some(actor) = &v.actor {
                    let actor_canon = canonicalize_actor(actor);

                    // Normalize: ensure u < w lexicographically
                    let (u, w, sign) = if a_canon < b_canon {
                        (a_canon.clone(), b_canon.clone(), 1i8) // prefers a
                    } else {
                        (b_canon.clone(), a_canon.clone(), -1i8) // prefers b
                    };

                    // Check for existing stances on this edge
                    let stances = self.edge_stances.entry((u.clone(), w.clone())).or_default();

                    for (other_actor, other_sign) in stances.iter() {
                        // If they voted opposite to us, notify them (THE HOOK)
                        if *other_sign == -sign && other_actor != &actor_canon {
                            let countered_item = if *other_sign == 1 {
                                u.clone()
                            } else {
                                w.clone()
                            };

                            let ratio_str = format!("{}:{}", v.ratio_left, v.ratio_right);

                            let notification = Notification {
                                ts: v.ts,
                                ingest_id: String::new(),
                                actor: actor_canon.clone(),
                                notification_type: NotificationType::ItemCountered {
                                    item: countered_item,
                                    opponent: actor_canon.clone(),
                                    body: v.body.clone(),
                                    ratio: ratio_str,
                                },
                            };

                            let queue = self.notifications.entry(other_actor.clone()).or_default();
                            queue.push_front(notification);
                            while queue.len() > 100 {
                                queue.pop_back();
                            }
                        }
                    }

                    // Record this actor's stance
                    stances.insert(actor_canon, sign);
                }

                let key = GroupKey { tag, aspect };
                let group = self
                    .groups
                    .entry(key.clone())
                    .or_insert_with(|| GroupState::new(key.tag.clone(), key.aspect.clone()));
                group.apply_vote(v);
            }
            Event::ItemUpsert(item) => {
                let item_id = canonicalize_item(&item.item);
                self.items.insert(item_id.clone());
                if let Some(body) = item.body {
                    self.item_bodies.insert(item_id, body);
                }
            }
            Event::TagAdd(t) => {
                let tag = canonicalize_tag(&t.tag);
                let item = canonicalize_item(&t.item);
                self.items.insert(item.clone());
                self.tags.entry(tag).or_default().insert(item);
            }
            Event::DslIngested(mut ing) => {
                // Canonicalize actor for indexing.
                ing.actor = canonicalize_actor(&ing.actor);

                // Extract tags by parsing raw content (single source of truth).
                let doc = crate::dsl::parse_full(&ing.raw);
                let extracted_tags: Vec<String> = doc
                    .statements
                    .iter()
                    .filter_map(|s| match s {
                        crate::dsl::Stmt::Hashtag { name } => Some(canonicalize_tag(name)),
                        _ => None,
                    })
                    .collect();

                // Index ingest by extracted tags.
                for tag in extracted_tags {
                    let q = self.ingests_by_tag.entry(tag).or_default();
                    q.push_front(ing.clone());
                    while q.len() > 25 {
                        q.pop_back();
                    }
                }
            }
        }
    }
}


