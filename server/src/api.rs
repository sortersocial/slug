use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use slug_types::*;
use std::collections::{BTreeSet, HashSet};

use std::collections::HashMap;

use crate::{
    dsl,
    events::{canonicalize_actor, canonicalize_item, canonicalize_tag, item_parent_path, Event},
    ranking::{ranked_items, ranked_items_subset},
    reducer::ReducerState,
    state::AppState,
};
use crate::events::Ingest;

fn api_error(status: StatusCode, error: impl Into<String>, hint: Option<String>) -> axum::response::Response {
    (status, Json(ApiError { ok: false, error: error.into(), hint })).into_response()
}

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

/// Resolve an item path as a first-class canonical path.
fn resolve_item(item: &str) -> Result<String, String> {
    let canonical = canonicalize_item(item);
    if canonical.is_empty() {
        return Err(format!("empty item path: `{}`", item));
    }
    Ok(canonical)
}

/// Validate actor format: @<uuid>:<rig>:<model>
fn validate_actor_format(actor: &str) -> Result<(), String> {
    let actor = actor.strip_prefix('@').unwrap_or(actor);

    let parts: Vec<&str> = actor.split(':').collect();
    if parts.len() != 3 {
        return Err(format!(
            "Invalid actor format. Expected @<uuid>:<rig>:<model>.\n\
             Got {} parts (expected 3).\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>",
            parts.len()
        ));
    }

    let (uuid_part, rig_part, model_part) = (parts[0], parts[1], parts[2]);

    if uuid::Uuid::parse_str(uuid_part).is_err() {
        return Err(format!(
            "Invalid UUID in actor format: '{}' is not a valid UUID.\n\
             Expected format: @<uuid>:<rig>:<model>\n\
             The UUID must be a full UUID v4.\n\
             \n\
             You provided: @{}:{}:{}\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>",
            uuid_part, uuid_part, rig_part, model_part
        ));
    }

    if rig_part.is_empty() {
        return Err(
            "Missing rig in actor format. Expected @<uuid>:<rig>:<model>.".to_string()
        );
    }

    if !model_part.contains('/') {
        return Err(format!(
            "Invalid model format in actor.\n\
             Expected format: @<uuid>:<rig>:<provider/model>\n\
             Example: @7a3b9c2d...:claudecode:anthropic/claude-sonnet-4.5\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>"
        ));
    }

    Ok(())
}

/// Result of validating an ingest document. Shared by post_ingest and post_check.
#[derive(Debug)]
pub struct ValidatedIngest {
    pub doc: dsl::Document,
    pub ts: i64,
    pub voter_key_id: String,
    pub actor: String,
    pub threads: Vec<String>,
    pub raw_text: String,
}

/// Parse and validate an ingest document against current reduced state.
/// Returns a validated struct for commit (ingest) or dry-run (check).
/// Error is (StatusCode, error message, optional hint).
pub fn validate_ingest_document(
    reduced: &ReducerState,
    text: &str,
    require_actor_error: &str,
) -> Result<ValidatedIngest, (StatusCode, String, Option<String>)> {
    let doc = match dsl::parse_full(text) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "parse error".to_string(),
                Some(format!("{}", e)),
            ));
        }
    };

    let ts = now_ms();
    let mut current_actor: Option<String> = None;
    let mut voter_key_id = "anon".to_string();
    let mut threads_seen: BTreeSet<String> = BTreeSet::new();
    let mut defined_in_doc: HashSet<String> = HashSet::new();

    for s in &doc.statements {
        match s {
            dsl::Stmt::Actor { name } => {
                if let Err(msg) = validate_actor_format(name) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "invalid actor format".to_string(),
                        Some(msg),
                    ));
                }
                let a = canonicalize_actor(name);
                current_actor = Some(a.clone());
                voter_key_id = a;
            }
            dsl::Stmt::Hashtag { name } => {
                let t = canonicalize_tag(name);
                threads_seen.insert(t);
            }
            dsl::Stmt::Item { title, body } => {
                let item = match resolve_item(title) {
                    Ok(v) => v,
                    Err(msg) => {
                        return Err((StatusCode::BAD_REQUEST, "invalid item path".to_string(), Some(msg)));
                    }
                };
                let Some(body_text) = body else {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("item missing body: /{item}"),
                        Some("items must be declared with bodies, e.g. `~/path/item { ... }`".to_string()),
                    ));
                };
                if body_text.trim().is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: /{item}"),
                        Some("write at least one sentence inside `{ ... }`".to_string()),
                    ));
                }
                defined_in_doc.insert(item);
            }
            dsl::Stmt::Vote { item1, item2, .. } => {
                let a = match resolve_item(item1) {
                    Ok(v) => v,
                    Err(msg) => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path".to_string(),
                            Some(msg),
                        ));
                    }
                };
                let b = match resolve_item(item2) {
                    Ok(v) => v,
                    Err(msg) => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path".to_string(),
                            Some(msg),
                        ));
                    }
                };
                let missing: Vec<String> = [&a, &b]
                    .into_iter()
                    .filter(|it| !defined_in_doc.contains(*it) && !reduced.items.contains(*it))
                    .map(|it| format!("/{it}"))
                    .collect();
                if !missing.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "vote references undefined item(s)".to_string(),
                        Some(format!(
                            "define items with bodies before voting. missing: {}",
                            missing.join(", ")
                        )),
                    ));
                }
                let missing_body: Vec<String> = [&a, &b]
                    .into_iter()
                    .filter(|it| !defined_in_doc.contains(*it) && !reduced.item_bodies.contains_key(*it))
                    .map(|it| format!("/{it}"))
                    .collect();
                if !missing_body.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "vote references item(s) without bodies".to_string(),
                        Some(format!(
                            "missing bodies: {}. Declare each item as `/item {{ ... }}`",
                            missing_body.join(", ")
                        )),
                    ));
                }
            }
            _ => {}
        }
    }

    let actor = current_actor.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            require_actor_error.to_string(),
            Some("add `@yourname` at the start of your document".to_string()),
        )
    })?;

    let threads: Vec<String> = threads_seen.into_iter().collect();
    if threads.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "ingest requires at least one #tag".to_string(),
            Some("declare a thread with #tag, e.g. #sorting-hat".to_string()),
        ));
    }

    Ok(ValidatedIngest {
        doc,
        ts,
        voter_key_id,
        actor,
        threads,
        raw_text: text.to_string(),
    })
}

// ============================================================================
// Rank / Pair — one-ranking (parent-path-filtered)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RankQuery {
    /// Parent path(s). Comma-separated to merge scopes: ~/a,~/b (e.g. rank ~/models ~/ai-models).
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn parse_parent_specs(parent: Option<&String>) -> Vec<String> {
    let s = match parent {
        Some(p) => p.trim(),
        None => return vec![],
    };
    if s.is_empty() {
        return vec![];
    }
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

pub async fn get_rank(State(state): State<AppState>, Query(q): Query<RankQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let specs = parse_parent_specs(q.parent.as_ref());
    let rankings = if specs.is_empty() {
        crate::scope_rank::build_children_rankings(&reduced, "")
    } else {
        let items = crate::scope_rank::resolve_scope(&reduced, &specs);
        crate::scope_rank::build_rankings_for_item_set(&reduced, &items)
    };

    let components: Vec<RankComponent> = rankings
        .component_rankings
        .into_iter()
        .map(|c| RankComponent {
            pairs: c.pairs,
            ranking: c
                .ranked
                .into_iter()
                .map(|r| RankRow {
                    item: format!("/{}", r.item),
                    score: r.score,
                })
                .collect(),
        })
        .collect();

    Json(RankResponse {
        components,
        unranked_items: rankings.unranked_items,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PairQuery {
    /// Parent path(s). Comma-separated to merge scopes: ~/a,~/b (e.g. rank ~/models ~/ai-models).
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub random: Option<bool>,
}


fn pick_random_distinct(items: &[String]) -> Option<(String, String)> {
    if items.len() < 2 {
        return None;
    }
    let mut rng = rand::thread_rng();
    let left = items.choose(&mut rng)?.clone();
    for _ in 0..8 {
        let right = items.choose(&mut rng)?.clone();
        if right != left {
            return Some((left, right));
        }
    }
    let mut right = items[0].clone();
    if right == left {
        right = items[1].clone();
    }
    Some((left, right))
}

fn is_pair_voted(group: &crate::reducer::GroupState, a: &str, b: &str) -> bool {
    let Some(&a_idx) = group.item_to_idx.get(a) else { return false; };
    let Some(&b_idx) = group.item_to_idx.get(b) else { return false; };
    let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
    group.voted_pairs.contains(&(i, j))
}

pub async fn get_pair(State(state): State<AppState>, Query(q): Query<PairQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let force_random = q.random.unwrap_or(false);

    let pool: Vec<String> = {
        let reduced = reduced_arc.read().await;
        let specs = parse_parent_specs(q.parent.as_ref());
        if specs.is_empty() {
            reduced.ranking_group.idx_to_item.clone()
        } else {
            crate::scope_rank::resolve_scope(&reduced, &specs)
        }
    };

    if pool.len() < 2 {
        let scope = q.parent.as_deref().unwrap_or("(all)");
        return api_error(
            StatusCode::BAD_REQUEST,
            format!("need at least 2 items under parent /{}", scope),
            Some("add items via ingest".to_string()),
        );
    }

    if force_random {
        let Some((left, right)) = pick_random_distinct(&pool) else {
            return api_error(StatusCode::BAD_REQUEST, "need at least 2 items", None);
        };
        let (left_body, right_body, threads) = {
            let reduced = reduced_arc.read().await;
            let lb = reduced.item_bodies.get(&left).cloned();
            let rb = reduced.item_bodies.get(&right).cloned();
            let th: Vec<String> = reduced
                .item_threads
                .get(&left)
                .into_iter()
                .chain(reduced.item_threads.get(&right))
                .flat_map(|s| s.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            (lb, rb, th)
        };
        return Json(PairResponse {
            left: format!("/{}", left),
            right: format!("/{}", right),
            left_body: left_body,
            right_body: right_body,
            threads,
        })
        .into_response();
    }

    let selected: Option<(String, String)> = {
        let mut reduced = reduced_arc.write().await;
        let group = &mut reduced.ranking_group;
        if group.idx_to_item.is_empty() {
            pick_random_distinct(&pool)
        } else {
                let mut rng = rand::thread_rng();
                let idxs: Vec<usize> = pool.iter()
                    .filter_map(|it| group.item_to_idx.get(it).copied())
                    .collect();
                let ranked = ranked_items_subset(group, &idxs, 10000, 1e-8);
                let ranked_set: std::collections::HashSet<String> =
                    ranked.iter().map(|r| r.item.clone()).collect();
                let unsorted: Vec<String> = pool.iter()
                    .filter(|it| !ranked_set.contains(*it))
                    .cloned()
                    .collect();

                let mut pick: Option<(String, String)> = None;
                if !unsorted.is_empty() {
                    if let Some(left) = unsorted.choose(&mut rng).cloned() {
                        let mut candidates: Vec<String> = if !ranked.is_empty() {
                            ranked.iter().map(|r| r.item.clone()).collect()
                        } else {
                            pool.clone()
                        };
                        candidates.retain(|c| c != &left);
                        if let Some(right) = candidates.choose(&mut rng).cloned() {
                            pick = Some((left, right));
                        }
                    }
                } else if ranked.len() >= 2 {
                    for i in 0..(ranked.len().saturating_sub(1)) {
                        let a = &ranked[i].item;
                        let b = &ranked[i + 1].item;
                        if a != b && !is_pair_voted(group, a, b) {
                            pick = Some((a.clone(), b.clone()));
                            break;
                        }
                    }
                    if pick.is_none() {
                        for _ in 0..64 {
                            let (Some(a), Some(b)) = (pool.choose(&mut rng).cloned(), pool.choose(&mut rng).cloned()) else { break; };
                            if a != b && !is_pair_voted(group, &a, &b) {
                                pick = Some((a, b));
                                break;
                            }
                        }
                    }
                }
                pick.or_else(|| pick_random_distinct(&pool))
        }
    };

    let Some((left, right)) = selected else {
        return api_error(StatusCode::BAD_REQUEST, "need at least 2 items", None);
    };

    let (left_body, right_body, threads) = {
        let reduced = reduced_arc.read().await;
        let lb = reduced.item_bodies.get(&left).cloned();
        let rb = reduced.item_bodies.get(&right).cloned();
        let th: Vec<String> = reduced
            .item_threads
            .get(&left)
            .into_iter()
            .chain(reduced.item_threads.get(&right))
            .flat_map(|s| s.iter().cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        (lb, rb, th)
    };
    Json(PairResponse {
        left: format!("/{}", left),
        right: format!("/{}", right),
        left_body,
        right_body,
        threads,
    })
    .into_response()
}

// ============================================================================
// Exploration APIs (read-only)
// ============================================================================

/// List root paths (items with parent "").
pub async fn get_paths(State(state): State<AppState>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let out: Vec<PathSummary> = reduced
        .item_children
        .get("")
        .map(|roots| {
            let mut v: Vec<PathSummary> = roots.iter().map(|path| {
                let children = reduced.item_children.get(path).map(|s| s.len()).unwrap_or(0);
                PathSummary {
                    path: format!("~/{}", path),
                    children,
                    web: format!("https://slug.social/~/{}", path),
                }
            }).collect();
            v.sort_by(|a, b| a.path.cmp(&b.path));
            v
        })
        .unwrap_or_default();

    Json(PathsResponse { paths: out }).into_response()
}

/// List every leaf item (full path). Items that have no children. Does not scale; works for now.
pub async fn get_leaves(State(state): State<AppState>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let parents: HashSet<&String> = reduced.item_children.keys().collect();
    let mut paths: Vec<String> = reduced
        .items
        .iter()
        .filter(|p| !parents.contains(p))
        .cloned()
        .collect();
    paths.sort();
    Json(LeavesResponse { paths }).into_response()
}

pub async fn get_threads(State(state): State<AppState>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let mut out: Vec<ThreadSummary> = reduced
        .threads
        .iter()
        .map(|(thread, ts)| {
            ThreadSummary {
                thread: format!("#{thread}"),
                last_activity_ts: ts.last_activity_ts,
                subscriber_count: ts.subscriber_count,
                web: format!("https://slug.social/t/{}", thread),
            }
        })
        .collect();

    out.sort_by(|a, b| b.last_activity_ts.cmp(&a.last_activity_ts));
    Json(ThreadsResponse { threads: out }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ThreadDetailQuery {
    pub tag: String,
}

/// Thread (forum) detail by tag — all posts, full body. Not the same as get_path (garden).
pub async fn get_thread(State(state): State<AppState>, Query(q): Query<ThreadDetailQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let tag = canonicalize_tag(&q.tag);
    let reduced = reduced_arc.read().await;

    let ingest_ids = reduced
        .ingests_by_thread
        .get(&tag)
        .map(|q| q.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    let posts: Vec<PostRow> = {
        let mut ids = ingest_ids;
        ids.sort_by_key(|id| {
            reduced
                .ingests_by_id
                .get(id)
                .map(|ing| ing.ts)
                .unwrap_or(0)
        });
        ids
            .into_iter()
            .filter_map(|ing_id| reduced.ingests_by_id.get(&ing_id))
            .map(|ing: &Ingest| PostRow {
                ts: ing.ts,
                voter_key_id: ing.voter_key_id.clone(),
                body: ing.raw.clone(),
            })
            .collect()
    };

    Json(ThreadDetailResponse {
        thread: format!("#{}", tag),
        posts,
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ItemQuery {
    pub item: String,
}

pub async fn get_item(State(state): State<AppState>, Query(q): Query<ItemQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let item = canonicalize_item(&q.item);
    let reduced = reduced_arc.read().await;

    let body = reduced.item_bodies.get(&item).cloned();
    let threads: Vec<String> = reduced
        .item_threads
        .get(&item)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();

    Json(ItemResponse {
        item: format!("/{}", item),
        body,
        threads,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RecentVotesQuery {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn vote_touches_path(a: &str, b: &str, parent_canon: &str) -> bool {
    let under = |item: &str| item == parent_canon || item.starts_with(&format!("{}/", parent_canon));
    under(a) || under(b)
}

pub async fn get_recent_votes(
    State(state): State<AppState>,
    Query(q): Query<RecentVotesQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let limit = q.limit.unwrap_or(25).clamp(1, 200);

    let reduced = reduced_arc.read().await;
    let group = &reduced.ranking_group;

    let iter = group.recent_votes.iter();
    let iter: Box<dyn Iterator<Item = _>> = if let Some(parent) = &q.parent {
        let parent_can = canonicalize_item(parent);
        Box::new(iter.filter(move |v| vote_touches_path(&v.a, &v.b, &parent_can)))
    } else {
        Box::new(iter)
    };

    let out: Vec<VoteRow> = iter.take(limit).map(|v| VoteRow {
        ts: v.ts,
        a: format!("/{}", v.a),
        b: format!("/{}", v.b),
        ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
        actor: Some(format!("@{}", v.actor)),
        body: v.body.clone(),
        thread: Some(format!("#{}", v.thread)),
    }).collect();

    Json(RecentVotesResponse { votes: out }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct MatchupQuery {
    pub item: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Vote history for one item (matchup: wins/losses with thread per vote).
pub async fn get_matchup(
    State(state): State<AppState>,
    Query(q): Query<MatchupQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let item = canonicalize_item(&q.item);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let reduced = reduced_arc.read().await;
    let votes: Vec<VoteRow> = reduced
        .item_votes
        .get(&item)
        .map(|q| {
            q.iter()
                .take(limit)
                .map(|v| VoteRow {
                    ts: v.ts,
                    a: format!("/{}", v.a),
                    b: format!("/{}", v.b),
                    ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                    actor: Some(format!("@{}", v.actor)),
                    body: v.body.clone(),
                    thread: Some(format!("#{}", v.thread)),
                })
                .collect()
        })
        .unwrap_or_default();

    Json(MatchupResponse {
        item: format!("/{}", item),
        votes,
    })
    .into_response()
}

// ============================================================================
// Ingest
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub events_appended: usize,
    pub next: NextMoves,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ranking_changes: Vec<ScopeRankChanges>,
}

#[derive(Debug, Serialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub ranking: Vec<RankRow>,
    pub next: Vec<String>,
}

/// Compute ranking changes between two snapshots of a parent scope's rankings.
/// Returns None if nothing changed.
fn compute_scope_rank_changes(
    parent: &str,
    before: &crate::scope_rank::ChildrenRankings,
    after: &crate::scope_rank::ChildrenRankings,
) -> Option<ScopeRankChanges> {
    fn build_positions(rankings: &crate::scope_rank::ChildrenRankings) -> HashMap<String, Option<RankPosition>> {
        let mut map = HashMap::new();
        for comp in &rankings.component_rankings {
            let total = comp.ranked.len();
            for (i, item) in comp.ranked.iter().enumerate() {
                map.insert(item.item.clone(), Some(RankPosition { rank: i + 1, of: total }));
            }
        }
        for item in &rankings.unranked_items {
            map.insert(item.clone(), None);
        }
        map
    }

    let before_pos = build_positions(before);
    let after_pos = build_positions(after);

    let all_items: std::collections::BTreeSet<String> = before_pos.keys().cloned()
        .chain(after_pos.keys().cloned())
        .collect();

    let mut changes: Vec<RankChange> = Vec::new();
    for item in all_items {
        let b = before_pos.get(&item).cloned().flatten();
        let a = after_pos.get(&item).cloned().flatten();
        let changed = match (&b, &a) {
            (Some(bp), Some(ap)) => bp.rank != ap.rank || bp.of != ap.of,
            (None, Some(_)) => true,
            (Some(_), None) => true,
            (None, None) => false,
        };
        if changed {
            changes.push(RankChange {
                item: format!("/{}", item),
                before: b,
                after: a,
            });
        }
    }

    if changes.is_empty() {
        return None;
    }

    changes.sort_by(|a, b| match (&a.after, &b.after) {
        (Some(ap), Some(bp)) => ap.rank.cmp(&bp.rank),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.item.cmp(&b.item),
    });

    Some(ScopeRankChanges {
        parent: if parent.is_empty() { "/".to_string() } else { format!("/{}", parent) },
        changes,
    })
}

pub async fn post_ingest(
    State(state): State<AppState>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let event_log = state.event_log.clone();
    let reduced = reduced_arc.read().await;
    let v = match validate_ingest_document(
        &reduced,
        &req.text,
        "ingest requires @actor declaration",
    ) {
        Ok(x) => x,
        Err((status, msg, hint)) => return api_error(status, msg, hint).into_response(),
    };
    drop(reduced);

    // Collect parent scopes for all voted items so we can compute ranking deltas.
    let voted_parent_scopes: Vec<String> = {
        let mut parents: HashSet<String> = HashSet::new();
        for s in &v.doc.statements {
            if let dsl::Stmt::Vote { item1, item2, .. } = s {
                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                    parents.insert(item_parent_path(&a).unwrap_or_default());
                    parents.insert(item_parent_path(&b).unwrap_or_default());
                }
            }
        }
        let mut out: Vec<String> = parents.into_iter().collect();
        out.sort();
        out
    };

    // Snapshot rankings before the event is applied.
    let pre_rankings: HashMap<String, crate::scope_rank::ChildrenRankings> =
        if !voted_parent_scopes.is_empty() {
            let reduced = reduced_arc.read().await;
            voted_parent_scopes
                .iter()
                .map(|p| (p.clone(), crate::scope_rank::build_children_rankings(&reduced, p)))
                .collect()
        } else {
            HashMap::new()
        };

    let event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        voter_key_id: v.voter_key_id.clone(),
        actor: v.actor.clone(),
    });

    if let Err(err) = event_log.append(&event).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
    }
    let actor_for_stream = v.actor.clone();
    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(event);
    }

    // Snapshot rankings after the event and compute per-scope deltas.
    let ranking_changes: Vec<ScopeRankChanges> = if !voted_parent_scopes.is_empty() {
        let reduced = reduced_arc.read().await;
        voted_parent_scopes
            .iter()
            .filter_map(|p| {
                let before = pre_rankings.get(p)?;
                let after = crate::scope_rank::build_children_rankings(&reduced, p);
                compute_scope_rank_changes(p, before, &after)
            })
            .collect()
    } else {
        vec![]
    };

    let _ = state.stream_tx.send(crate::state::StreamEvent {
        ts: v.ts,
        actor: actor_for_stream,
        tags: v.threads.iter().map(|t| format!("#{t}")).collect(),
        snippet: v.raw_text.chars().take(200).collect(),
    });
    let html = crate::html::thread_feed_html(&state).await;
    let _ = state.html_tx.send(crate::state::HtmlFragment {
        selector: "#thread-feed".to_string(),
        html,
    });

    let primary_thread = v.threads.first().cloned().unwrap_or_else(|| "untagged".to_string());
    Json(IngestResponse {
        ok: true,
        threads: v.threads.iter().map(|t| format!("#{t}")).collect(),
        events_appended: 1,
        next: NextMoves {
            pair: "npx slugsocial pair".to_string(),
            rank: "npx slugsocial rank".to_string(),
            web: format!("https://slug.social/t/{}", primary_thread),
        },
        ranking_changes,
    }).into_response()
}

pub async fn post_web_ingest(
    State(state): State<AppState>,
    axum::extract::Form(req): axum::extract::Form<IngestRequest>,
) -> impl IntoResponse {
    let json_req = Json(req);
    let resp = post_ingest(State(state), json_req).await.into_response();
    if resp.status().is_success() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        resp
    }
}

pub async fn post_check(
    State(state): State<AppState>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let v = match validate_ingest_document(
        &reduced,
        &req.text,
        "check requires @actor declaration",
    ) {
        Ok(x) => x,
        Err((status, msg, hint)) => return api_error(status, msg, hint).into_response(),
    };
    drop(reduced);

    let event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        voter_key_id: v.voter_key_id.clone(),
        actor: v.actor.clone(),
    });

    let mut simulated = { reduced_arc.read().await.clone() };
    simulated.apply_event(event);

    let ranking = ranked_items(&mut simulated.ranking_group, 10000, 1e-8)
        .into_iter()
        .take(25)
        .map(|r| RankRow {
            item: format!("/{}", r.item),
            score: r.score,
        })
        .collect();

    let primary_thread = v.threads.first().cloned().unwrap_or_else(|| "untagged".to_string());

    Json(CheckResponse {
        ok: true,
        threads: v.threads.iter().map(|t| format!("#{t}")).collect(),
        ranking,
        next: vec![
            "npx slugsocial ingest <file.sorter>".to_string(),
            "npx slugsocial threads".to_string(),
            format!("https://slug.social/t/{}", primary_thread),
        ],
    }).into_response()
}

// ============================================================================
// Notifications
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct NotificationsQuery {
    pub actor: String,
    #[serde(default)]
    pub since: Option<i64>,
}

pub async fn get_notifications(
    State(state): State<AppState>,
    Query(q): Query<NotificationsQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let actor = canonicalize_actor(&q.actor);
    let since = q.since.unwrap_or(0);

    let notifications = {
        let reduced = reduced_arc.read().await;
        reduced
            .notifications
            .get(&actor)
            .map(|queue| {
                queue.iter()
                    .filter(|n| n.ts > since)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    Json(NotificationsResponse {
        ok: true,
        actor: format!("@{}", actor),
        notifications,
    }).into_response()
}

// ============================================================================
// Presence (thread pages)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct PresencePingRequest {
    pub session_id: String,
    pub thread_tag: String,
    #[serde(default)]
    pub cursor_anchor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PresencePingResponse {
    pub ok: bool,
    pub global_viewers: usize,
    pub local_viewers: usize,
}

pub async fn post_presence_ping(
    State(state): State<AppState>,
    Json(req): Json<PresencePingRequest>,
) -> impl IntoResponse {
    let session_id = req.session_id.trim().to_string();
    if session_id.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing session_id",
            Some("provide a stable client session id".to_string()),
        );
    }

    let thread_tag = canonicalize_tag(&req.thread_tag);
    if thread_tag.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing thread_tag",
            Some("presence is only valid for /t/:tag pages".to_string()),
        );
    }

    let counts = state
        .upsert_presence(session_id, thread_tag, req.cursor_anchor)
        .await;

    Json(PresencePingResponse {
        ok: true,
        global_viewers: counts.global_viewers,
        local_viewers: counts.local_viewers,
    })
    .into_response()
}

// ============================================================================
// SSE streams
// ============================================================================

pub async fn get_html_stream(State(state): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use futures_util::stream;
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

    let initial_html = crate::html::thread_feed_html(&state).await;
    let initial = stream::once(async move {
        Ok::<_, std::convert::Infallible>(
            SseEvent::default().data(format!("#thread-feed\n{}", initial_html)),
        )
    });

    let rx = state.html_tx.subscribe();
    let updates = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(frag) => Some(Ok::<_, std::convert::Infallible>(
            SseEvent::default().data(format!("{}\n{}", frag.selector, frag.html)),
        )),
        Err(_) => None,
    });

    Sse::new(initial.chain(updates)).keep_alive(KeepAlive::default())
}

pub async fn get_stream(State(state): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

    let rx = state.stream_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| {
        match msg {
            Ok(ev) => {
                let data = serde_json::to_string(&ev).unwrap_or_default();
                Some(Ok::<_, std::convert::Infallible>(
                    SseEvent::default().event("ingest").data(data),
                ))
            }
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Ingest;

    fn apply_ingest(reduced: &mut ReducerState, ts: i64, raw: &str) {
        reduced.apply_event(Event::Ingest(Ingest {
            ts,
            id: format!("test-{ts}"),
            raw: raw.to_string(),
            voter_key_id: "test".to_string(),
            actor: "test".to_string(),
        }));
    }

    #[test]
    fn validate_ingest_document_requires_actor() {
        let reduced = ReducerState::default();
        let text = "~/t/a {a}\n~/t/b {b}\n";
        let err = validate_ingest_document(&reduced, text, "need actor").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "need actor");
    }

    #[test]
    fn validate_ingest_document_parse_error() {
        let reduced = ReducerState::default();
        let text = "~/t/a { unclosed ";
        let err = validate_ingest_document(&reduced, text, "need actor").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "parse error");
    }

    #[test]
    fn validate_ingest_document_accepts_valid_doc_with_existing_items() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n~/t/a {a}\n~/t/b {b}\n~/t/a 2:1 ~/t/b {because}\n",
        );
        let text = "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n~/t/a 1:1 ~/t/b {equal}\n";
        let v = validate_ingest_document(&reduced, text, "need actor").unwrap();
        assert_eq!(v.actor, "00000000-0000-0000-0000-000000000000:test:local/test");
        assert_eq!(v.threads, vec!["t"]);
    }

    #[test]
    fn validate_ingest_document_rejects_vote_on_undefined_item() {
        let reduced = ReducerState::default();
        let text = "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n~/t/a {a}\n~/t/b 1:1 ~/t/missing {why}\n";
        let err = validate_ingest_document(&reduced, text, "need actor").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("undefined item"));
    }

    #[test]
    fn validate_ingest_document_requires_tag() {
        let reduced = ReducerState::default();
        let text = "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/a {a}\n~/t/b {b}\n";
        let err = validate_ingest_document(&reduced, text, "need actor").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "ingest requires at least one #tag");
    }

    #[test]
    fn validate_ingest_document_rejects_item_without_body() {
        let reduced = ReducerState::default();
        let text = "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n~/t/a\n";
        let err = validate_ingest_document(&reduced, text, "need actor").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("missing body"));
    }
}
