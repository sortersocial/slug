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

use crate::{
    dsl,
    events::{canonicalize_actor, canonicalize_item, canonicalize_tag, Event},
    ranking::{ranked_items, ranked_items_subset},
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

// ============================================================================
// Rank / Pair — one-ranking (parent-path-filtered)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RankQuery {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn get_rank(State(state): State<AppState>, Query(q): Query<RankQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let ranking = {
        let reduced = state.reduced.read().await;
        let group = &reduced.ranking_group;
        if !group.idx_to_item.is_empty() {
            let idxs: Vec<usize> = match &q.parent {
                Some(parent) => {
                    let parent = canonicalize_item(parent);
                    reduced
                        .item_children
                        .get(&parent)
                        .map(|s| s.iter().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|it| group.item_to_idx.get(it).copied())
                        .collect()
                }
                None => (0..group.idx_to_item.len()).collect(),
            };
            ranked_items_subset(group, &idxs, 10000, 1e-8)
                .into_iter()
                .take(limit)
                .map(|r| RankRow { item: format!("/{}", r.item), score: r.score })
                .collect()
        } else {
            vec![]
        }
    };

    Json(RankResponse { ranking }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct PairQuery {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub random: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PairResponseLocal {
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
    let force_random = q.random.unwrap_or(false);

    // Pool: direct children of parent scope, or all items in the group.
    let pool: Vec<String> = {
        let reduced = state.reduced.read().await;
        match &q.parent {
            Some(parent) => {
                let parent = canonicalize_item(parent);
                reduced.item_children.get(&parent)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default()
            }
            None => reduced.ranking_group.idx_to_item.clone(),
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
        let (left_body, right_body) = {
            let reduced = state.reduced.read().await;
            (reduced.item_bodies.get(&left).cloned(), reduced.item_bodies.get(&right).cloned())
        };
        return Json(PairResponseLocal {
            left: format!("/{}", left),
            right: format!("/{}", right),
            left_body,
            right_body,
        }).into_response();
    }

    let selected: Option<(String, String)> = {
        let mut reduced = state.reduced.write().await;
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

    let (left_body, right_body) = {
        let reduced = state.reduced.read().await;
        (reduced.item_bodies.get(&left).cloned(), reduced.item_bodies.get(&right).cloned())
    };
    Json(PairResponseLocal {
        left: format!("/{}", left),
        right: format!("/{}", right),
        left_body,
        right_body,
    }).into_response()
}

// ============================================================================
// Exploration APIs (read-only)
// ============================================================================

/// List root paths (items with parent "").
pub async fn get_paths(State(state): State<AppState>) -> impl IntoResponse {
    let reduced = state.reduced.read().await;

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

pub async fn get_threads(State(state): State<AppState>) -> impl IntoResponse {
    let reduced = state.reduced.read().await;

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
pub struct PathDetailQuery {
    pub path: String,
}

pub async fn get_path(State(state): State<AppState>, Query(q): Query<PathDetailQuery>) -> impl IntoResponse {
    let path = canonicalize_item(&q.path);
    let reduced = state.reduced.read().await;

    let mut children: Vec<String> = reduced
        .item_children
        .get(&path)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    children.sort();

    let recent_ingests: Vec<IngestRow> = {
        // Path-first: gather ingests that touched this path or its direct children.
        let mut ingest_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for item in std::iter::once(path.clone()).chain(children.iter().cloned()) {
            if let Some(q) = reduced.item_snippets.get(&item) {
                for ing_id in q.iter() {
                    if seen.insert(ing_id.clone()) {
                        ingest_ids.push(ing_id.clone());
                    }
                }
            }
        }

        ingest_ids.sort_by_key(|id| {
            reduced
                .ingests_by_id
                .get(id)
                .map(|ing| std::cmp::Reverse(ing.ts))
                .unwrap_or(std::cmp::Reverse(0))
        });

        ingest_ids
            .into_iter()
            .take(20)
            .filter_map(|ing_id| reduced.ingests_by_id.get(&ing_id))
            .map(|ing: &Ingest| IngestRow {
                ts: ing.ts,
                actor: Some(format!("@{}", ing.actor)),
                voter_key_id: ing.voter_key_id.clone(),
                snippet: ing.raw.chars().take(800).collect(),
            })
            .collect()
    };

    Json(PathDetailResponse {
        path: format!("~/{}", path),
        children: children.into_iter().map(|it| format!("/{}", it)).collect(),
        recent_ingests,
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ItemQuery {
    pub item: String,
}

pub async fn get_item(State(state): State<AppState>, Query(q): Query<ItemQuery>) -> impl IntoResponse {
    let item = canonicalize_item(&q.item);
    let reduced = state.reduced.read().await;

    let body = reduced.item_bodies.get(&item).cloned();

    Json(ItemResponse {
        item: format!("/{}", item),
        body,
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct RecentVotesQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn get_recent_votes(
    State(state): State<AppState>,
    Query(q): Query<RecentVotesQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(25).clamp(1, 200);

    let reduced = state.reduced.read().await;
    let group = &reduced.ranking_group;

    let out: Vec<VoteRow> = group.recent_votes.iter().take(limit).map(|v| VoteRow {
        ts: v.ts,
        a: format!("/{}", v.a),
        b: format!("/{}", v.b),
        ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
        actor: Some(format!("@{}", v.actor)),
        body: v.body.clone(),
    }).collect();

    Json(RecentVotesResponse { votes: out }).into_response()
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
}

#[derive(Debug, Serialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub ranking: Vec<RankRow>,
    pub next: Vec<String>,
}

pub async fn post_ingest(
    State(state): State<AppState>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let mut current_actor: Option<String> = None;
    let mut voter_key_id: String = "anon".to_string();

    let doc = match dsl::parse_full(&req.text) {
        Ok(d) => d,
        Err(e) => {
            return api_error(StatusCode::BAD_REQUEST, "parse error", Some(format!("{}", e)));
        }
    };

    let ts = now_ms();

    let mut threads_seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut defined_in_doc: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Validate pass.
    for s in &doc.statements {
        match s {
            dsl::Stmt::Actor { name } => {
                if let Err(msg) = validate_actor_format(name) {
                    return api_error(StatusCode::BAD_REQUEST, "invalid actor format", Some(msg));
                }
                let a = canonicalize_actor(name);
                current_actor = Some(a.clone());
                voter_key_id = a;
            }
            dsl::Stmt::Hashtag { name } => {
                let t = canonicalize_tag(name);
                threads_seen.insert(t.clone());
            }
            dsl::Stmt::Attribute { .. } => {}
            dsl::Stmt::Item { title, body } => {
                let item = match resolve_item(title) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid item path", Some(msg)),
                };
                let Some(body_text) = body else {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item missing body: /{item}"),
                        Some("items must be declared with bodies, e.g. `~/path/item { ... }`".to_string()),
                    );
                };
                if body_text.trim().is_empty() {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: /{item}"),
                        Some("write at least one sentence inside `{ ... }`".to_string()),
                    );
                }
                defined_in_doc.insert(item);
            }
            dsl::Stmt::Vote { item1, item2, .. } => {
                let a = match resolve_item(item1) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid vote item path", Some(msg)),
                };
                let b = match resolve_item(item2) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid vote item path", Some(msg)),
                };
                // Validate items exist and have bodies.
                {
                    let reduced = state.reduced.read().await;
                    let missing: Vec<String> = [&a, &b]
                        .into_iter()
                        .filter(|it| !defined_in_doc.contains(*it) && !reduced.items.contains(*it))
                        .map(|it| format!("/{it}"))
                        .collect();
                    if !missing.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references undefined item(s)",
                            Some(format!("define items with bodies before voting. missing: {}", missing.join(", "))),
                        );
                    }
                    let missing_body: Vec<String> = [&a, &b]
                        .into_iter()
                        .filter(|it| !defined_in_doc.contains(*it) && !reduced.item_bodies.contains_key(*it))
                        .map(|it| format!("/{it}"))
                        .collect();
                    if !missing_body.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references item(s) without bodies",
                            Some(format!("missing bodies: {}. Declare each item as `/item {{ ... }}`", missing_body.join(", "))),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let Some(actor) = current_actor.clone() else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "ingest requires @actor declaration",
            Some("add `@yourname` at the start of your document".to_string()),
        );
    };

    let threads_vec: Vec<String> = threads_seen.into_iter().collect();

    let event = Event::Ingest(Ingest {
        ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: req.text.clone(),
        voter_key_id: voter_key_id.clone(),
        actor,
    });

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

    let _ = state.stream_tx.send(crate::state::StreamEvent {
        ts,
        actor: actor_for_stream,
        tags: threads_vec.iter().map(|t| format!("#{t}")).collect(),
        snippet: req.text.chars().take(200).collect(),
    });

    {
        let html = crate::html::thread_feed_html(&state).await;
        let _ = state.html_tx.send(crate::state::HtmlFragment {
            selector: "#thread-feed".to_string(),
            html,
        });
    }

    let primary_thread = threads_vec.first().cloned().unwrap_or_else(|| "untagged".to_string());
    Json(IngestResponse {
        ok: true,
        threads: threads_vec.iter().map(|t| format!("#{t}")).collect(),
        events_appended: 1,
        next: NextMoves {
            pair: "npx slugsocial pair".to_string(),
            rank: "npx slugsocial rank".to_string(),
            web: format!("https://slug.social/t/{}", primary_thread),
        },
    }).into_response()
}

pub async fn post_web_ingest(
    State(state): State<AppState>,
    axum::extract::Form(req): axum::extract::Form<IngestRequest>,
) -> impl IntoResponse {
    let json_req = Json(req);
    let headers = axum::http::HeaderMap::new();
    let resp = post_ingest(State(state), headers, json_req).await.into_response();
    if resp.status().is_success() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        resp
    }
}

pub async fn post_check(
    State(state): State<AppState>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let doc = match dsl::parse_full(&req.text) {
        Ok(d) => d,
        Err(e) => {
            return api_error(StatusCode::BAD_REQUEST, "parse error", Some(format!("{}", e)));
        }
    };

    let mut current_actor: Option<String> = None;
    let mut voter_key_id: String = "anon".to_string();
    let ts = now_ms();

    let mut threads_seen: BTreeSet<String> = BTreeSet::new();
    let mut defined_in_doc: std::collections::HashSet<String> = std::collections::HashSet::new();

    for s in &doc.statements {
        match s {
            dsl::Stmt::Actor { name } => {
                if let Err(msg) = validate_actor_format(name) {
                    return api_error(StatusCode::BAD_REQUEST, "invalid actor format", Some(msg));
                }
                let a = canonicalize_actor(name);
                current_actor = Some(a.clone());
                voter_key_id = a;
            }
            dsl::Stmt::Hashtag { name } => {
                let t = canonicalize_tag(name);
                threads_seen.insert(t.clone());
            }
            dsl::Stmt::Attribute { .. } => {}
            dsl::Stmt::Item { title, body } => {
                let item = match resolve_item(title) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid item path", Some(msg)),
                };
                let Some(body_text) = body else {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item missing body: /{item}"),
                        Some("items must be declared with bodies, e.g. `~/path/item { ... }`".to_string()),
                    );
                };
                if body_text.trim().is_empty() {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: /{item}"),
                        Some("write at least one sentence inside `{ ... }`".to_string()),
                    );
                }
                defined_in_doc.insert(item);
            }
            dsl::Stmt::Vote { item1, item2, .. } => {
                let a = match resolve_item(item1) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid vote item path", Some(msg)),
                };
                let b = match resolve_item(item2) {
                    Ok(v) => v,
                    Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid vote item path", Some(msg)),
                };
                {
                    let reduced = state.reduced.read().await;
                    let missing: Vec<String> = [&a, &b]
                        .into_iter()
                        .filter(|it| !defined_in_doc.contains(*it) && !reduced.items.contains(*it))
                        .map(|it| format!("/{it}"))
                        .collect();
                    if !missing.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references undefined item(s)",
                            Some(format!("define items with bodies before voting. missing: {}", missing.join(", "))),
                        );
                    }
                    let missing_body: Vec<String> = [&a, &b]
                        .into_iter()
                        .filter(|it| !defined_in_doc.contains(*it) && !reduced.item_bodies.contains_key(*it))
                        .map(|it| format!("/{it}"))
                        .collect();
                    if !missing_body.is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "vote references item(s) without bodies",
                            Some(format!("missing bodies: {}. Declare each item as `/item {{ ... }}`", missing_body.join(", "))),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let Some(actor) = current_actor.clone() else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "check requires @actor declaration",
            Some("add `@yourname` at the start of your document".to_string()),
        );
    };

    let event = Event::Ingest(Ingest {
        ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: req.text.clone(),
        voter_key_id: voter_key_id.clone(),
        actor,
    });

    let mut simulated = { state.reduced.read().await.clone() };
    simulated.apply_event(event);

    let ranking = ranked_items(&mut simulated.ranking_group, 10000, 1e-8)
        .into_iter()
        .take(25)
        .map(|r| RankRow { item: format!("/{}", r.item), score: r.score })
        .collect();

    let threads_vec: Vec<String> = threads_seen.into_iter().collect();
    let primary_thread = threads_vec.first().cloned().unwrap_or_else(|| "untagged".to_string());

    Json(CheckResponse {
        ok: true,
        threads: threads_vec.iter().map(|t| format!("#{t}")).collect(),
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
    let actor = canonicalize_actor(&q.actor);
    let since = q.since.unwrap_or(0);

    let notifications = {
        let reduced = state.reduced.read().await;
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
