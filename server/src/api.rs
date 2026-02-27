use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use slug_types::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    dsl,
    events::{canonicalize_actor, canonicalize_aspect, canonicalize_item, canonicalize_tag, item_thread, Event},
    ranking::{ranked_items, ranked_items_subset},
    reducer::GroupKey,
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

/// Resolve an item path, optionally using a current tag context for single-segment items.
/// `#tag` context + `/item` is treated as `~/tag/item`.
fn require_path_item_ctx(item: &str, current_tag: Option<&str>) -> Result<(String, String), String> {
    let canonical = canonicalize_item(item);
    if canonical.is_empty() {
        return Err(format!("empty item path: `{}`", item));
    }
    if canonical.contains('/') {
        // Multi-segment: explicit thread root present.
        let tag = item_thread(&canonical).unwrap();
        return Ok((tag, canonical));
    }
    // Single-segment: needs #tag context.
    if let Some(tag) = current_tag {
        let tag = canonicalize_tag(tag);
        let full = format!("{}/{}", tag, canonical);
        return Ok((tag, full));
    }
    Err(format!(
        "item `{}` has no thread root. use `~/thread/{}` or set `#thread` context first.",
        item, canonical
    ))
}

fn require_path_item(item: &str) -> Result<(String, String), String> {
    require_path_item_ctx(item, None)
}

/// Validate actor format: @<uuid>:<rig>:<model>
/// Returns Err with helpful message if format is invalid.
fn validate_actor_format(actor: &str) -> Result<(), String> {
    let actor = actor.strip_prefix('@').unwrap_or(actor);

    // Full format must be: <uuid>:<rig>:<model>
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

    // Validate UUID format using uuid crate
    if uuid::Uuid::parse_str(uuid_part).is_err() {
        return Err(format!(
            "Invalid UUID in actor format: '{}' is not a valid UUID.\n\
             Expected format: @<uuid>:<rig>:<model>\n\
             The UUID must be a full UUID v4, not a prefix (e.g., 7a3b9c2d-1234-5678-90ab-cdef12345678).\n\
             \n\
             You provided: @{}:{}:{}\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>",
            uuid_part, uuid_part, rig_part, model_part
        ));
    }

    // Validate rig is non-empty
    if rig_part.is_empty() {
        return Err(format!(
            "Missing rig in actor format.\n\
             Expected format: @<uuid>:<rig>:<model>\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>"
        ));
    }

    // Validate model format (should contain a slash for provider/model)
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

// REMOVED: post_vote endpoint - use ingest instead.
// All votes should be submitted via npx slugsocial ingest with .sorter documents.
/*
pub async fn post_vote(
    State(state): State<AppState>,
    _headers: axum::http::HeaderMap,
    payload: Result<Json<VoteRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(req) = match payload {
        Ok(v) => v,
        Err(rej) => {
            let msg = rej.to_string();
            let hint = if msg.contains("missing field `body`") || msg.contains("missing field 'body'") {
                Some(
                    "your client is missing the required vote explanation.\n\
upgrade `slugsocial` and pass a justification string.\n\
\n\
example:\n\
  npx slugsocial vote '#tag' /a 2:1 /b :default @you \"because ...\"\n\
\n\
or via stdin:\n\
  printf '%s\\n' 'because ...' | npx slugsocial vote '#tag' /a 2:1 /b :default @you"
                        .to_string(),
                )
            } else {
                Some(format!("invalid JSON: {msg}"))
            };
            return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid request body", hint);
        }
    };
    let actor_c = req.actor.as_ref().map(|a| canonicalize_actor(a));

    // Validate actor format if provided
    if let Some(ref actor) = req.actor {
        if let Err(msg) = validate_actor_format(actor) {
            return api_error(StatusCode::BAD_REQUEST, "invalid actor format", Some(msg));
        }
    }

    let voter_key_id = actor_c.clone().unwrap_or_else(|| "anon".to_string());
    let tag = canonicalize_tag(&req.tag);
    let aspect = canonicalize_aspect(&req.aspect);
    let a = canonicalize_item(&req.a);
    let b = canonicalize_item(&req.b);

    fn parse_ratio(s: &str) -> Option<(i32, i32)> {
        let t = s.trim();
        let (l, r) = t.split_once(':')?;
        let left: i32 = l.trim().parse().ok()?;
        let right: i32 = r.trim().parse().ok()?;
        Some((left, right))
    }

    let (ratio_left, ratio_right) = match req.ratio.as_deref() {
        Some(r) => {
            let Some((l, rr)) = parse_ratio(r) else {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid ratio",
                    Some("expected like `3:1` (e.g. `/a 3:1 /b`)".to_string()),
                );
            };
            (l.max(0), rr.max(0))
        }
        None => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "missing ratio",
                Some("pass `ratio: \"3:1\"`".to_string()),
            )
        }
    };

    let body = req.body.trim().to_string();
    if body.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing vote explanation",
            Some("provide a non-empty `body` explaining why you voted that way".to_string()),
        );
    }

    // Enforce: items must already exist in the tag AND have bodies.
    {
        let reduced = state.reduced.read().await;
        let Some(items) = reduced.tags.get(&tag) else {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!("unknown tag: #{tag}"),
                Some("define items first via `npx slugsocial ingest <file>` containing `#tag` and `/item { body }`".to_string()),
            );
        };
        let missing: Vec<String> = [a.clone(), b.clone()]
            .into_iter()
            .filter(|it| !items.contains(it))
            .map(|it| format!("/{it}"))
            .collect();
        if !missing.is_empty() {
            return api_error(
                StatusCode::BAD_REQUEST,
                "vote references undefined item(s) for this tag",
                Some(format!(
                    "missing under #{tag}: {}. Define them first like:\n#{}\n/{} {{ ... }}\n/{} {{ ... }}",
                    missing.join(", "),
                    tag,
                    a,
                    b
                )),
            );
        }
        let missing_body: Vec<String> = [a.clone(), b.clone()]
            .into_iter()
            .filter(|it| !reduced.item_bodies.contains_key(it))
            .map(|it| format!("/{it}"))
            .collect();
        if !missing_body.is_empty() {
            return api_error(
                StatusCode::BAD_REQUEST,
                "vote references item(s) without bodies",
                Some(format!(
                    "missing bodies: {}. Define each item with a body block: `/item {{ ... }}`",
                    missing_body.join(", ")
                )),
            );
        }
    }

    let vote = VoteCast {
        ts: now_ms(),
        tag: tag.clone(),
        aspect: aspect.clone(),
        a: a.clone(),
        b: b.clone(),
        ratio_left,
        ratio_right,
        body,
        voter_key_id,
        actor: actor_c.clone(),
    };

    let event = Event::VoteCast(vote.clone());
    if let Err(err) = state.event_log.append(&event).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
    }

    {
        let mut reduced = state.reduced.write().await;
        reduced.apply_event(event);
    }

    // Return ranking for this group.
    let ranking = {
        let mut reduced = state.reduced.write().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let group = reduced.groups.get_mut(&key);
        if let Some(g) = group {
            ranked_items(g, 10000, 1e-8)
                .into_iter()
                .take(25)
                .map(|r| RankRow {
                    item: format!("/{}", r.item),
                    score: r.score,
                })
                .collect()
        } else {
            vec![]
        }
    };

    let resp = VoteResponse {
        ok: true,
        tag: format!("#{tag}"),
        aspect: format!(":{aspect}"),
        ranking,
        next: NextMoves {
            pair: format!(
                "npx slugsocial pair --thread {} --aspect {}",
                tag,
                aspect
            ),
            rank: format!(
                "npx slugsocial rank --thread {} --aspect {}",
                tag,
                aspect
            ),
            web: format!("https://slug.social/~/{}?aspect={}", tag, aspect),
        },
    };

    Json(resp).into_response()
}
*/

#[derive(Debug, Deserialize)]
pub struct RankQuery {
    pub tag: String,
    pub aspect: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct RankResponse {
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

pub async fn get_rank(State(state): State<AppState>, Query(q): Query<RankQuery>) -> impl IntoResponse {
    let tag = canonicalize_tag(&q.tag);
    let aspect = canonicalize_aspect(&q.aspect);
    let parent = q
        .parent
        .as_deref()
        .map(canonicalize_item)
        .unwrap_or_else(|| tag.clone());
    if item_thread(&parent).as_deref() != Some(tag.as_str()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "parent path must be in requested thread",
            Some(format!("expected parent under `~/{}`", tag)),
        );
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let ranking = {
        let reduced = state.reduced.read().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let group = reduced.groups.get(&key);
        if let Some(g) = group {
            let children = reduced
                .item_children
                .get(&parent)
                .map(|s| s.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let idxs: Vec<usize> = children
                .iter()
                .filter_map(|it| g.item_to_idx.get(it).copied())
                .collect();
            ranked_items_subset(g, &idxs, 10000, 1e-8)
                .into_iter()
                .take(limit)
                .map(|r| RankRow {
                    item: format!("/{}", r.item),
                    score: r.score,
                })
                .collect()
        } else {
            vec![]
        }
    };

    Json(RankResponse {
        tag: format!("#{tag}"),
        aspect: format!(":{aspect}"),
        ranking,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PairQuery {
    pub tag: String,
    pub aspect: String,
    #[serde(default)]
    pub parent: Option<String>,
    /// If true, ignore ranking and select a random pair (useful for “skip”).
    #[serde(default)]
    pub random: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PairResponse {
    pub tag: String,
    pub aspect: String,
    pub left: String,
    pub right: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_body: Option<String>,
}

fn pick_random_distinct(items: &[String]) -> Option<(String, String)> {
    if items.len() < 2 {
        return None;
    }
    let mut rng = rand::thread_rng();
    let left = items.choose(&mut rng)?.clone();
    // Retry a few times to avoid pathological “all same” (shouldn’t happen, but cheap).
    for _ in 0..8 {
        let right = items.choose(&mut rng)?.clone();
        if right != left {
            return Some((left, right));
        }
    }
    // Deterministic fallback.
    let mut right = items[0].clone();
    if right == left {
        right = items[1].clone();
    }
    Some((left, right))
}

fn is_pair_voted(group: &crate::reducer::GroupState, a: &str, b: &str) -> bool {
    let Some(&a_idx) = group.item_to_idx.get(a) else {
        return false;
    };
    let Some(&b_idx) = group.item_to_idx.get(b) else {
        return false;
    };
    let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
    group.voted_pairs.contains(&(i, j))
}

pub async fn get_pair(State(state): State<AppState>, Query(q): Query<PairQuery>) -> impl IntoResponse {
    let tag = canonicalize_tag(&q.tag);
    let aspect = canonicalize_aspect(&q.aspect);
    let parent = q
        .parent
        .as_deref()
        .map(canonicalize_item)
        .unwrap_or_else(|| tag.clone());
    if item_thread(&parent).as_deref() != Some(tag.as_str()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "parent path must be in requested thread",
            Some(format!("expected parent under `~/{}`", tag)),
        );
    }
    let force_random = q.random.unwrap_or(false);

    // Pool is direct children under parent scope.
    let pool: Vec<String> = {
        let reduced = state.reduced.read().await;
        if let Some(s) = reduced.item_children.get(&parent) {
            s.iter().cloned().collect()
        } else {
            vec![]
        }
    };

    if pool.len() < 2 {
        return api_error(
            StatusCode::BAD_REQUEST,
            format!("need at least 2 sibling items under parent /{}", parent),
            Some("add sibling items via ingest using `~/thread/parent/child` paths".to_string()),
        );
    }

    // Random mode: ignore ranking and pick any distinct pair from pool.
    if force_random {
        let Some((left, right)) = pick_random_distinct(&pool) else {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!("need at least 2 sibling items under parent /{}", parent),
                Some("add sibling items via ingest using `~/thread/parent/child` paths".to_string()),
            );
        };
        let (left_body, right_body) = {
            let reduced = state.reduced.read().await;
            (
                reduced.item_bodies.get(&left).cloned(),
                reduced.item_bodies.get(&right).cloned(),
            )
        };
        return Json(PairResponse {
            tag: format!("#{tag}"),
            aspect: format!(":{aspect}"),
            left: format!("/{}", left),
            right: format!("/{}", right),
            left_body,
            right_body,
        })
        .into_response();
    }

    // Rank-aware selection (when a group exists for this tag+aspect).
    let selected: Option<(String, String)> = {
        let mut reduced = state.reduced.write().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };

        match reduced.groups.get_mut(&key) {
            // If no votes yet for this aspect, just pick random from the tag pool.
            None => pick_random_distinct(&pool),
            Some(group) => {
                let mut rng = rand::thread_rng();

                let idxs: Vec<usize> = pool
                    .iter()
                    .filter_map(|it| group.item_to_idx.get(it).copied())
                    .collect();
                let ranked = ranked_items_subset(group, &idxs, 10000, 1e-8);
                let ranked_set: std::collections::HashSet<String> =
                    ranked.iter().map(|r| r.item.clone()).collect();

                let unsorted: Vec<String> = pool
                    .iter()
                    .filter(|it| !ranked_set.contains(*it))
                    .cloned()
                    .collect();

                // Strategy 1: introduce unsorted items by pairing with a sorted item (or any other item).
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
                    // Strategy 2: all items are “sorted”; prefer adjacent pairs not yet voted on.
                    for i in 0..(ranked.len().saturating_sub(1)) {
                        let a = &ranked[i].item;
                        let b = &ranked[i + 1].item;
                        if a != b && !is_pair_voted(group, a, b) {
                            pick = Some((a.clone(), b.clone()));
                            break;
                        }
                    }

                    // Otherwise pick any unvoted pair (bounded attempt).
                    if pick.is_none() {
                        for _ in 0..64 {
                            let (Some(a), Some(b)) =
                                (pool.choose(&mut rng).cloned(), pool.choose(&mut rng).cloned())
                            else {
                                break;
                            };
                            if a != b && !is_pair_voted(group, &a, &b) {
                                pick = Some((a, b));
                                break;
                            }
                        }
                    }
                }

                // Saturated/degenerate or selection failure: allow re-voting.
                pick.or_else(|| pick_random_distinct(&pool))
            }
        }
    };

    let Some((left, right)) = selected else {
        return api_error(
            StatusCode::BAD_REQUEST,
            format!("need at least 2 items under tag #{tag}"),
            Some(format!("add items via ingest:\n#{}\n/item {{ ... }}", tag)),
        );
    };

    let (left_body, right_body) = {
        let reduced = state.reduced.read().await;
        (
            reduced.item_bodies.get(&left).cloned(),
            reduced.item_bodies.get(&right).cloned(),
        )
    };
    Json(PairResponse {
        tag: format!("#{tag}"),
        aspect: format!(":{aspect}"),
        left: format!("/{}", left),
        right: format!("/{}", right),
        left_body,
        right_body,
    })
    .into_response()
}

// ============================================================================
// Exploration APIs (read-only)
// ============================================================================

#[derive(Debug, Serialize)]
pub struct TagSummary {
    pub tag: String,
    pub items: usize,
    pub aspects: usize,
    pub web: String,
}

#[derive(Debug, Serialize)]
pub struct TagsResponse {
    pub tags: Vec<TagSummary>,
}

pub async fn get_tags(State(state): State<AppState>) -> impl IntoResponse {
    let reduced = state.reduced.read().await;

    let mut aspects_by_tag: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for k in reduced.groups.keys() {
        aspects_by_tag
            .entry(k.tag.clone())
            .or_default()
            .insert(k.aspect.clone());
    }

    let mut tags: BTreeSet<String> = BTreeSet::new();
    tags.extend(reduced.tags.keys().cloned());
    tags.extend(aspects_by_tag.keys().cloned());

    let mut out: Vec<TagSummary> = Vec::new();
    for t in tags.into_iter() {
        let items = reduced.tags.get(&t).map(|s| s.len()).unwrap_or(0);
        let aspects = aspects_by_tag.get(&t).map(|s| s.len()).unwrap_or(0);
        out.push(TagSummary {
            tag: format!("#{t}"),
            items,
            aspects,
            web: format!("https://slug.social/~/{}", t),
        });
    }

    Json(TagsResponse { tags: out }).into_response()
}

// ThreadSummary and ThreadsResponse live in slug_types (shared with CLI).

pub async fn get_threads(State(state): State<AppState>) -> impl IntoResponse {
    let reduced = state.reduced.read().await;

    let mut aspects_by_tag: BTreeMap<String, usize> = BTreeMap::new();
    for k in reduced.groups.keys() {
        *aspects_by_tag.entry(k.tag.clone()).or_default() += 1;
    }

    // Collect all known threads from the ThreadState map.
    let mut out: Vec<slug_types::ThreadSummary> = reduced
        .threads
        .iter()
        .map(|(tag, thread)| {
            let items = reduced.tags.get(tag).map(|s| s.len()).unwrap_or(0);
            let aspects = aspects_by_tag.get(tag).copied().unwrap_or(0);
            slug_types::ThreadSummary {
                thread: format!("#{tag}"),
                last_activity_ts: thread.last_activity_ts,
                subscriber_count: thread.subscriber_count,
                items,
                aspects,
                web: format!("https://slug.social/~/{}", tag),
            }
        })
        .collect();

    // Bump order: most recently active first.
    out.sort_by(|a, b| b.last_activity_ts.cmp(&a.last_activity_ts));

    Json(slug_types::ThreadsResponse { threads: out }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct TagDetailQuery {
    pub tag: String,
}

#[derive(Debug, Serialize)]
pub struct TagDetailResponse {
    pub tag: String,
    pub items: Vec<String>,
    pub aspects: Vec<String>,
    pub recent_ingests: Vec<IngestRow>,
}

#[derive(Debug, Serialize)]
pub struct IngestRow {
    pub ts: i64,
    pub actor: Option<String>,
    pub voter_key_id: String,
    pub snippet: String,
}

pub async fn get_tag(State(state): State<AppState>, Query(q): Query<TagDetailQuery>) -> impl IntoResponse {
    let tag = canonicalize_tag(&q.tag);
    let reduced = state.reduced.read().await;

    let mut items: Vec<String> = reduced
        .tags
        .get(&tag)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_else(Vec::new);
    items.sort();

    let mut aspects: BTreeSet<String> = BTreeSet::new();
    for k in reduced.groups.keys() {
        if k.tag == tag {
            aspects.insert(k.aspect.clone());
        }
    }

    let recent_ingests: Vec<IngestRow> = reduced
        .ingests_by_tag
        .get(&tag)
        .map(|q| {
            q.iter()
                .take(20)
                .filter_map(|ing_id| reduced.ingests_by_id.get(ing_id))
                .map(|ing: &Ingest| IngestRow {
                    ts: ing.ts,
                    actor: Some(format!("@{}", ing.actor)),
                    voter_key_id: ing.voter_key_id.clone(),
                    snippet: ing.raw.chars().take(800).collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    Json(TagDetailResponse {
        tag: format!("#{tag}"),
        items: items.into_iter().map(|it| format!("/{}", it)).collect(),
        aspects: aspects.into_iter().map(|a| format!(":{a}")).collect(),
        recent_ingests,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ItemQuery {
    pub item: String,
}

#[derive(Debug, Serialize)]
pub struct ItemResponse {
    pub item: String,
    pub body: Option<String>,
    pub tags: Vec<String>,
}

pub async fn get_item(State(state): State<AppState>, Query(q): Query<ItemQuery>) -> impl IntoResponse {
    let item = canonicalize_item(&q.item);
    let reduced = state.reduced.read().await;

    let body = reduced.item_bodies.get(&item).cloned();
    let mut tags: Vec<String> = Vec::new();
    for (t, set) in reduced.tags.iter() {
        if set.contains(&item) {
            tags.push(format!("#{t}"));
        }
    }
    tags.sort();

    Json(ItemResponse {
        item: format!("/{}", item),
        body,
        tags,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RecentVotesQuery {
    pub tag: String,
    pub aspect: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct VoteRow {
    pub ts: i64,
    pub tag: String,
    pub aspect: String,
    pub a: String,
    pub b: String,
    pub ratio: String,
    pub actor: Option<String>,
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct RecentVotesResponse {
    pub votes: Vec<VoteRow>,
}

pub async fn get_recent_votes(
    State(state): State<AppState>,
    Query(q): Query<RecentVotesQuery>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&q.tag);
    let aspect = canonicalize_aspect(&q.aspect);
    let limit = q.limit.unwrap_or(25).clamp(1, 200);

    let reduced = state.reduced.read().await;
    let key = GroupKey { tag, aspect };
    let Some(group) = reduced.groups.get(&key) else {
        return Json(RecentVotesResponse { votes: vec![] }).into_response();
    };

    let mut out: Vec<VoteRow> = Vec::new();
    for v in group.recent_votes.iter().take(limit) {
        out.push(VoteRow {
            ts: v.ts,
            tag: format!("#{}", v.tag),
            aspect: format!(":{}", v.aspect),
            a: format!("/{}", v.a),
            b: format!("/{}", v.b),
            ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
            actor: Some(format!("@{}", v.actor)),
            body: v.body.clone(),
        });
    }

    Json(RecentVotesResponse { votes: out }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    /// Raw text containing DSL (and optionally prose).
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub tags: Vec<String>,
    pub events_appended: usize,
    pub next: NextMoves,
}

#[derive(Debug, Serialize)]
pub struct CheckGroup {
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub tags: Vec<String>,
    pub groups: Vec<CheckGroup>,
    pub next: Vec<String>,
}

/// Ingest a DSL/prose document, emitting events into the JSONL log.
///
/// Interpretation model:
/// - `:aspect` sets the active aspect context (default: "default")
/// - `~/thread/item {body}` declares/updates an item body
/// - `~/thread/a 2:1 ~/thread/b {explanation}` records a vote
pub async fn post_ingest(
    State(state): State<AppState>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    // Open-write mode: attribution comes from `@actor` in the document, else "anon".
    let mut current_actor: Option<String> = None;
    let mut voter_key_id: String = "anon".to_string();

    // Single parsing mode: always `parse_full` (prose-compatible).
    let doc = match dsl::parse_full(&req.text) {
        Ok(d) => d,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "parse error",
                Some(format!("{}", e)),
            );
        }
    };

    let mut current_aspect: String = "default".to_string();
    let mut current_tag: Option<String> = None;
    let ts = now_ms();

    let mut tags_seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut defined_in_doc: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Parse once for validation and metadata extraction.
    for s in &doc.statements {
        match s {
            dsl::Stmt::Actor { name } => {
                // Validate actor format.
                if let Err(msg) = validate_actor_format(&name) {
                    return api_error(StatusCode::BAD_REQUEST, "invalid actor format", Some(msg));
                }
                let a = canonicalize_actor(&name);
                current_actor = Some(a.clone());
                voter_key_id = a;
            }
            dsl::Stmt::Hashtag { name } => {
                // #thread sets tag context for subsequent items/votes.
                current_tag = Some(canonicalize_tag(name));
            }
            dsl::Stmt::Attribute { name } => {
                current_aspect = canonicalize_aspect(&name);
            }
            dsl::Stmt::Item { title, body } => {
                let (tag, item) = match require_path_item_ctx(&title, current_tag.as_deref()) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid item path", Some(msg)),
                };
                let Some(body_text) = body else {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item missing body: /{item}"),
                        Some("items must be declared with bodies, e.g. `~/thread/item { ... }`".to_string()),
                    );
                };
                if body_text.trim().is_empty() {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: /{item}"),
                        Some("write at least one sentence inside `{ ... }`".to_string()),
                    );
                }
                defined_in_doc.insert(item.clone());
                tags_seen.insert(tag);
            }
            dsl::Stmt::Vote {
                item1,
                item2,
                ..
            } => {
                let (tag_a, a) = match require_path_item_ctx(&item1, current_tag.as_deref()) {
                    Ok(v) => v,
                    Err(msg) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path",
                            Some(msg),
                        )
                    }
                };
                let (tag_b, b) = match require_path_item_ctx(&item2, current_tag.as_deref()) {
                    Ok(v) => v,
                    Err(msg) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path",
                            Some(msg),
                        )
                    }
                };
                if tag_a != tag_b {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "vote items must be in the same thread root",
                        Some(format!(
                            "left is in `#{}` and right is in `#{}`",
                            tag_a, tag_b
                        )),
                    );
                }
                let tag = tag_a;
                tags_seen.insert(tag.clone());

                // Validate items exist and have bodies.
                {
                    let reduced = state.reduced.read().await;
                    let items_in_tag = reduced.tags.get(&tag);
                    let mut missing: Vec<String> = Vec::new();
                    for it in [&a, &b] {
                        let ok_in_doc = defined_in_doc.contains(it);
                        let ok_in_tag = items_in_tag.map(|s| s.contains(it)).unwrap_or(false);
                        if !(ok_in_doc || ok_in_tag) {
                            missing.push(format!("/{it}"));
                        }
                    }
                    if !missing.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references undefined item(s)",
                            Some(format!(
                                "define items with bodies before voting. missing: {}",
                                missing.join(", ")
                            )),
                        );
                    }
                    let mut missing_body: Vec<String> = Vec::new();
                    for it in [&a, &b] {
                        let ok_body_in_doc = defined_in_doc.contains(it);
                        let ok_body_in_state = reduced.item_bodies.contains_key(it);
                        if !(ok_body_in_doc || ok_body_in_state) {
                            missing_body.push(format!("/{it}"));
                        }
                    }
                    if !missing_body.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references item(s) without bodies",
                            Some(format!(
                                "missing bodies: {}. Declare each item as `/item {{ ... }}`",
                                missing_body.join(", ")
                            )),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    // Require actor for threading/notifications.
    let Some(actor) = current_actor.clone() else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "ingest requires @actor declaration",
            Some("add `@yourname` at the start of your document".to_string()),
        );
    };

    let tags_vec: Vec<String> = tags_seen.into_iter().collect();

    // Create single Ingest event - all parsing happens in reducer.
    let event = Event::Ingest(crate::events::Ingest {
        ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: req.text.clone(),
        voter_key_id: voter_key_id.clone(),
        actor,
    });

    // Persist then reduce.
    if let Err(err) = state.event_log.append(&event).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
    }
    let actor_for_stream = match &event {
        Event::Ingest(ing) => ing.actor.clone(),
    };
    {
        let mut reduced = state.reduced.write().await;
        reduced.apply_event(event);
    }

    // Broadcast to SSE subscribers (best-effort; ignore send errors if no subscribers).
    let _ = state.stream_tx.send(crate::state::StreamEvent {
        ts,
        actor: actor_for_stream,
        tags: tags_vec.iter().map(|t| format!("#{t}")).collect(),
        snippet: req.text.chars().take(200).collect(),
    });

    // Broadcast updated HTML fragment for web SSE (poem pattern).
    {
        let html = crate::html::thread_feed_html(&state).await;
        let _ = state.html_tx.send(crate::state::HtmlFragment {
            selector: "#thread-feed".to_string(),
            html,
        });
    }

    let primary_tag = tags_vec
        .get(0)
        .cloned()
        .unwrap_or_else(|| "untagged".to_string());
    Json(IngestResponse {
        ok: true,
        tags: tags_vec.iter().map(|t| format!("#{t}")).collect(),
        events_appended: 1, // Single Ingest event
        next: NextMoves {
            pair: format!(
                "npx slugsocial pair --thread {} --aspect {}",
                primary_tag,
                current_aspect
            ),
            rank: format!(
                "npx slugsocial rank --thread {} --aspect {}",
                primary_tag,
                current_aspect
            ),
            web: format!("https://slug.social/~/{}", primary_tag),
        },
    })
    .into_response()
}

/// Web form ingest — accepts application/x-www-form-urlencoded, no API key required.
/// Returns 204 on success; DOM update arrives via SSE.
pub async fn post_web_ingest(
    State(state): State<AppState>,
    axum::extract::Form(req): axum::extract::Form<IngestRequest>,
) -> impl IntoResponse {
    // Delegate to the same logic as post_ingest but accept form data.
    // Re-use the JSON body path by constructing IngestRequest directly.
    let json_req = Json(req);
    let headers = axum::http::HeaderMap::new();
    let resp = post_ingest(State(state), headers, json_req).await.into_response();
    if resp.status().is_success() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        resp
    }
}

/// Check a DSL/prose document without committing it:
/// parse + validate + show simulated rankings for the groups it touches.
pub async fn post_check(
    State(state): State<AppState>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    // Single parsing mode: always `parse_full` (prose-compatible).
    let doc = match dsl::parse_full(&req.text) {
        Ok(d) => d,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "parse error",
                Some(format!("{}", e)),
            );
        }
    };

    // Open-write semantics: actor comes from `@actor` in the document.
    let mut current_actor: Option<String> = None;
    let mut voter_key_id: String = "anon".to_string();
    let mut current_aspect: String = "default".to_string();
    let mut current_tag: Option<String> = None;
    let ts = now_ms();

    let mut tags_seen: BTreeSet<String> = BTreeSet::new();
    let mut defined_in_doc: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut groups_touched: BTreeSet<(String, String)> = BTreeSet::new(); // (tag, aspect)

    // Parse once for validation and metadata.
    for s in &doc.statements {
        match s {
            dsl::Stmt::Actor { name } => {
                // Validate actor format.
                if let Err(msg) = validate_actor_format(&name) {
                    return api_error(StatusCode::BAD_REQUEST, "invalid actor format", Some(msg));
                }
                let a = canonicalize_actor(&name);
                current_actor = Some(a.clone());
                voter_key_id = a;
            }
            dsl::Stmt::Hashtag { name } => {
                // #thread sets tag context for subsequent items/votes.
                current_tag = Some(canonicalize_tag(name));
            }
            dsl::Stmt::Attribute { name } => {
                current_aspect = canonicalize_aspect(&name);
            }
            dsl::Stmt::Item { title, body } => {
                let (tag, item) = match require_path_item_ctx(&title, current_tag.as_deref()) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid item path", Some(msg)),
                };
                let Some(body_text) = body else {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item missing body: /{item}"),
                        Some("items must be declared with bodies, e.g. `~/thread/item { ... }`".to_string()),
                    );
                };
                if body_text.trim().is_empty() {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: /{item}"),
                        Some("write at least one sentence inside `{ ... }`".to_string()),
                    );
                }
                defined_in_doc.insert(item.clone());
                tags_seen.insert(tag);
            }
            dsl::Stmt::Vote {
                item1,
                item2,
                ..
            } => {
                let (tag_a, a) = match require_path_item_ctx(&item1, current_tag.as_deref()) {
                    Ok(v) => v,
                    Err(msg) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path",
                            Some(msg),
                        )
                    }
                };
                let aspect = current_aspect.clone();
                let (tag_b, b) = match require_path_item_ctx(&item2, current_tag.as_deref()) {
                    Ok(v) => v,
                    Err(msg) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path",
                            Some(msg),
                        )
                    }
                };
                if tag_a != tag_b {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "vote items must be in the same thread root",
                        Some(format!(
                            "left is in `#{}` and right is in `#{}`",
                            tag_a, tag_b
                        )),
                    );
                }
                let tag = tag_a;
                tags_seen.insert(tag.clone());

                // Validate items exist and have bodies.
                {
                    let reduced = state.reduced.read().await;
                    let items_in_tag = reduced.tags.get(&tag);
                    let mut missing: Vec<String> = Vec::new();
                    for it in [&a, &b] {
                        let ok_in_doc = defined_in_doc.contains(it);
                        let ok_in_tag = items_in_tag.map(|s| s.contains(it)).unwrap_or(false);
                        if !(ok_in_doc || ok_in_tag) {
                            missing.push(format!("/{it}"));
                        }
                    }
                    if !missing.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references undefined item(s)",
                            Some(format!(
                                "define items with bodies before voting. missing: {}",
                                missing.join(", ")
                            )),
                        );
                    }
                    let mut missing_body: Vec<String> = Vec::new();
                    for it in [&a, &b] {
                        let ok_body_in_doc = defined_in_doc.contains(it);
                        let ok_body_in_state = reduced.item_bodies.contains_key(it);
                        if !(ok_body_in_doc || ok_body_in_state) {
                            missing_body.push(format!("/{it}"));
                        }
                    }
                    if !missing_body.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references item(s) without bodies",
                            Some(format!(
                                "missing bodies: {}. Declare each item as `/item {{ ... }}`",
                                missing_body.join(", ")
                            )),
                        );
                    }
                }

                groups_touched.insert((tag, aspect));
            }
            _ => {}
        }
    }

    // Require actor for threading/notifications.
    let Some(actor) = current_actor.clone() else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "check requires @actor declaration",
            Some("add `@yourname` at the start of your document".to_string()),
        );
    };

    // Create Ingest event for dry-run.
    let event = Event::Ingest(crate::events::Ingest {
        ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: req.text.clone(),
        voter_key_id: voter_key_id.clone(),
        actor,
    });

    // Apply to a clone (dry-run).
    let mut simulated = { state.reduced.read().await.clone() };
    simulated.apply_event(event);

    let mut groups: Vec<CheckGroup> = Vec::new();
    for (tag, aspect) in groups_touched.into_iter() {
        let key = GroupKey {
            tag: canonicalize_tag(&tag),
            aspect: canonicalize_aspect(&aspect),
        };
        let ranking = simulated
            .groups
            .get_mut(&key)
            .map(|g| ranked_items(g, 10000, 1e-8))
            .unwrap_or_default()
            .into_iter()
            .take(25)
            .map(|r| RankRow {
                item: format!("/{}", r.item),
                score: r.score,
            })
            .collect();
        groups.push(CheckGroup {
            tag: format!("#{}", key.tag),
            aspect: format!(":{}", key.aspect),
            ranking,
        });
    }
    groups.sort_by(|a, b| (a.tag.clone(), a.aspect.clone()).cmp(&(b.tag.clone(), b.aspect.clone())));

    let tags_vec: Vec<String> = tags_seen.into_iter().collect();
    let primary_tag = tags_vec
        .get(0)
        .cloned()
        .unwrap_or_else(|| "untagged".to_string());

    Json(CheckResponse {
        ok: true,
        tags: tags_vec.iter().map(|t| format!("#{t}")).collect(),
        groups,
        next: vec![
            "npx slugsocial ingest <file.sorter>".to_string(),
            "npx slugsocial threads".to_string(),
            format!("https://slug.social/~/{}", primary_tag),
        ],
    })
    .into_response()
}

/// Query parameters for notifications endpoint.
#[derive(Debug, Deserialize)]
pub struct NotificationsQuery {
    /// Actor to get notifications for (e.g. "@aec").
    pub actor: String,
    /// Unix timestamp in milliseconds - return notifications since this time.
    #[serde(default)]
    pub since: Option<i64>,
}

/// Response for notifications endpoint.
#[derive(Debug, Serialize)]
pub struct NotificationsResponse {
    pub ok: bool,
    pub actor: String,
    pub notifications: Vec<crate::reducer::Notification>,
}

pub async fn get_notifications(
    State(state): State<AppState>,
    Query(q): Query<NotificationsQuery>,
) -> impl IntoResponse {
    let actor = canonicalize_actor(&q.actor);
    let since = q.since.unwrap_or(0);

    let notifications = {
        let reduced = state.reduced.read().await;
        reduced
            .notifications
            .get(&actor)
            .map(|queue| {
                queue
                    .iter()
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
    })
    .into_response()
}

/// SSE web stream (poem pattern) — broadcasts HTML fragments for DOM morphing.
/// Connect: GET /sse
/// On connect: immediately sends current thread feed.
/// On ingest: sends updated HTML fragments.
/// Message format: first line = CSS selector, remaining lines = HTML.
pub async fn get_html_stream(State(state): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use futures_util::stream;
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

    // Send current thread feed immediately on connect.
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

/// SSE live stream — broadcasts an event for every ingest.
/// Connect: GET /api/v0/stream
/// Each event is a JSON-encoded StreamEvent.
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
            // Lagged — skip the missed events rather than killing the stream.
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}


