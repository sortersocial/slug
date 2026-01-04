use std::collections::{HashMap, HashSet, VecDeque};

use crate::events::{canonicalize_aspect, canonicalize_item, canonicalize_tag, Event, VoteCast};

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
        vote.score = vote.score.clamp(-50, 50);

        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        // Track that this unordered pair has been voted on (at least once).
        let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
        self.voted_pairs.insert((i, j));

        // We follow the pagerank.rs convention where a negative magnitude means
        // left/a is preferred. Our VoteCast has positive score => prefer a, so
        // we negate it.
        let magnitude = (-(vote.score) as f64).clamp(-50.0, 50.0);
        let weight_left_wins = magnitude + 50.0; // in [0,100]
        let weight_right_wins = 100.0 - weight_left_wins;

        // Edge weights represent how much probability mass should flow from loser -> winner.
        // With magnitude=-50 (prefer a completely), weight_left_wins=0 and weight_right_wins=100,
        // so flow b->a is maximal.
        self.add_edge_weight(a_idx, b_idx, weight_left_wins);
        self.add_edge_weight(b_idx, a_idx, weight_right_wins);

        self.recent_votes.push_front(vote);
        while self.recent_votes.len() > 200 {
            self.recent_votes.pop_back();
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReducerState {
    pub groups: HashMap<GroupKey, GroupState>,
    pub items: HashSet<String>,
    pub tags: HashMap<String, HashSet<String>>, // tag -> set(item)
    pub item_bodies: HashMap<String, String>,
}

impl ReducerState {
    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::VoteCast(v) => {
                let tag = canonicalize_tag(&v.tag);
                let aspect = canonicalize_aspect(&v.aspect);
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
        }
    }
}


