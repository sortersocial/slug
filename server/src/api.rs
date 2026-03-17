use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

/// Verify actor passkey and return the UUID portion if valid.
fn verified_actor_uuid(reduced: &ReducerState, actor: Option<&str>, passkey: Option<&str>) -> Option<String> {
    let actor_str = actor?;
    let pk = passkey?;
    let actor_can = canonicalize_actor(actor_str);
    let stored_hash = reduced.actor_keys.get(&actor_can)?;
    let hash = sha256_hex(pk);
    if hash == *stored_hash {
        Some(crate::events::actor_uuid(&actor_can).to_string())
    } else {
        None
    }
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

    let actor_u = crate::events::actor_uuid(&actor).to_string();

    // Private namespace ownership checks.
    for s in &doc.statements {
        match s {
            dsl::Stmt::Item { title, .. } => {
                let item = canonicalize_item(title);
                if let Some(owner) = crate::events::path_owner_uuid(&item) {
                    if owner != actor_u {
                        return Err((
                            StatusCode::FORBIDDEN,
                            "private namespace: path owner does not match actor".to_string(),
                            Some(format!(
                                "/{} belongs to UUID {}, but your actor UUID is {}",
                                item, owner, actor_u
                            )),
                        ));
                    }
                }
            }
            dsl::Stmt::Vote { item1, item2, .. } => {
                let a = canonicalize_item(item1);
                let b = canonicalize_item(item2);
                let a_owner = crate::events::path_owner_uuid(&a);
                let b_owner = crate::events::path_owner_uuid(&b);
                match (a_owner, b_owner) {
                    (None, None) => {} // both public, fine
                    (Some(oa), Some(ob)) if oa == ob => {
                        // both private, same owner — actor must match
                        if oa != actor_u {
                            return Err((
                                StatusCode::FORBIDDEN,
                                "private namespace: vote owner does not match actor".to_string(),
                                Some(format!("these items belong to UUID {}, not {}", oa, actor_u)),
                            ));
                        }
                    }
                    _ => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "cross-votes between private and public items are not allowed".to_string(),
                            Some("votes must either be between two public items or two private items owned by the same actor".to_string()),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let threads: Vec<String> = threads_seen.into_iter().collect();
    if threads.is_empty() {
        // Thread is optional only when every item and vote is under the actor's private namespace.
        // Pure prose posts and public items still require a #tag.
        let all_private = {
            let mut has_content = false;
            let mut all_under_actor = true;
            for s in &doc.statements {
                match s {
                    dsl::Stmt::Item { title, .. } => {
                        has_content = true;
                        let item = canonicalize_item(title);
                        if crate::events::path_owner_uuid(&item) != Some(actor_u.as_str()) {
                            all_under_actor = false;
                        }
                    }
                    dsl::Stmt::Vote { item1, item2, .. } => {
                        has_content = true;
                        let a = canonicalize_item(item1);
                        let b = canonicalize_item(item2);
                        if crate::events::path_owner_uuid(&a) != Some(actor_u.as_str())
                            || crate::events::path_owner_uuid(&b) != Some(actor_u.as_str())
                        {
                            all_under_actor = false;
                        }
                    }
                    _ => {}
                }
            }
            has_content && all_under_actor
        };
        if !all_private {
            return Err((
                StatusCode::BAD_REQUEST,
                "ingest requires at least one #tag".to_string(),
                Some("declare a thread with #tag, e.g. #sorting-hat\n(#tag is optional only when all items are in your private ~/uuid/ namespace)".to_string()),
            ));
        }
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
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
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

pub async fn get_rank(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<RankQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());
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
                .filter(|r| match crate::events::path_owner_uuid(&r.item) {
                    None => true,
                    Some(owner) => authed_uuid.as_deref() == Some(owner),
                })
                .map(|r| RankRow {
                    item: format!("/{}", r.item),
                    score: r.score,
                })
                .collect(),
        })
        .collect();

    Json(RankResponse {
        components,
        unranked_items: rankings.unranked_items.into_iter()
            .filter(|p| match crate::events::path_owner_uuid(p) {
                None => true,
                Some(owner) => authed_uuid.as_deref() == Some(owner),
            })
            .collect(),
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
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
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

pub async fn get_pair(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<PairQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let force_random = q.random.unwrap_or(false);

    let pool: Vec<String> = {
        let reduced = reduced_arc.read().await;
        let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or_else(|| q.passkey.clone());
        let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());
        let specs = parse_parent_specs(q.parent.as_ref());
        let raw_pool = if specs.is_empty() {
            reduced.ranking_group.idx_to_item.clone()
        } else {
            crate::scope_rank::resolve_scope(&reduced, &specs)
        };
        raw_pool.into_iter().filter(|it| match crate::events::path_owner_uuid(it) {
            None => true,
            Some(owner) => authed_uuid.as_deref() == Some(owner),
        }).collect()
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

#[derive(Debug, Deserialize)]
pub struct PathsQuery {
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

/// List root paths (items with parent "").
pub async fn get_paths(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<PathsQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    let out: Vec<PathSummary> = reduced
        .item_children
        .get("")
        .map(|roots| {
            let mut v: Vec<PathSummary> = roots.iter()
                .filter(|path| {
                    match crate::events::path_owner_uuid(path) {
                        None => true,
                        Some(owner) => authed_uuid.as_deref() == Some(owner),
                    }
                })
                .map(|path| {
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

#[derive(Debug, Deserialize)]
pub struct LeavesQuery {
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

/// List every leaf item (full path). Items that have no children. Does not scale; works for now.
pub async fn get_leaves(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<LeavesQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());
    let parents: HashSet<&String> = reduced.item_children.keys().collect();
    let mut paths: Vec<String> = reduced
        .items
        .iter()
        .filter(|p| !parents.contains(p))
        .filter(|p| {
            match crate::events::path_owner_uuid(p) {
                None => true,
                Some(owner) => authed_uuid.as_deref() == Some(owner),
            }
        })
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
    /// Chronological offset (0 = oldest). Default: 0.
    pub offset: Option<usize>,
    /// Number of posts to return. Default: 10, max: 500.
    pub limit: Option<usize>,
    /// Only posts at or after this Unix ms timestamp.
    pub since: Option<i64>,
    /// Only posts strictly before this Unix ms timestamp.
    pub before: Option<i64>,
    /// Filter to posts whose actor starts with this prefix (UUID prefix or full actor string).
    pub actor: Option<String>,
    /// Return the single post with this ingest ID.
    pub post_id: Option<String>,
}

/// Thread (forum) detail by tag — paginated, filterable posts, full body.
pub async fn get_thread(State(state): State<AppState>, Query(q): Query<ThreadDetailQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let tag = canonicalize_tag(&q.tag);
    let reduced = reduced_arc.read().await;

    // Single post lookup by ingest ID.
    if let Some(ref post_id) = q.post_id {
        let thread_ids = reduced.ingests_by_thread.get(&tag);
        let index = thread_ids.and_then(|ids| {
            ids.iter().rev().enumerate().find(|(_, id)| *id == post_id).map(|(i, _)| i)
        });
        return match index.and_then(|idx| reduced.ingests_by_id.get(post_id).map(|ing| (idx, ing))) {
            None => api_error(StatusCode::NOT_FOUND, "post not found", None),
            Some((idx, ing)) => Json(ThreadDetailResponse {
                thread: format!("#{}", tag),
                posts: vec![PostRow {
                    id: ing.id.clone(),
                    index: idx,
                    ts: ing.ts,
                    actor: ing.actor.clone(),
                    voter_key_id: ing.voter_key_id.clone(),
                    body: ing.raw.clone(),
                }],
                total: 1,
                offset: idx,
            }).into_response(),
        };
    }

    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(10).clamp(1, 500);

    // ingests_by_thread is newest-first; reverse to chronological order.
    let all_ids: Vec<String> = reduced
        .ingests_by_thread
        .get(&tag)
        .map(|q| q.iter().rev().cloned().collect())
        .unwrap_or_default();

    let actor_prefix = q.actor.as_deref().unwrap_or("").to_lowercase();
    let filtered: Vec<(usize, _)> = all_ids
        .into_iter()
        .enumerate()
        .filter_map(|(idx, id)| reduced.ingests_by_id.get(&id).map(|ing| (idx, ing.clone())))
        .filter(|(_, ing)| q.since.map_or(true, |s| ing.ts >= s))
        .filter(|(_, ing)| q.before.map_or(true, |b| ing.ts < b))
        .filter(|(_, ing)| actor_prefix.is_empty() || ing.actor.to_lowercase().starts_with(&actor_prefix))
        .collect();

    let total = filtered.len();

    let posts: Vec<PostRow> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(idx, ing)| PostRow {
            id: ing.id.clone(),
            index: idx,
            ts: ing.ts,
            actor: ing.actor.clone(),
            voter_key_id: ing.voter_key_id.clone(),
            body: ing.raw.clone(),
        })
        .collect();

    Json(ThreadDetailResponse {
        thread: format!("#{}", tag),
        posts,
        total,
        offset,
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ItemQuery {
    pub item: String,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

pub async fn get_item(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<ItemQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let item = canonicalize_item(&q.item);
    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    if let Some(owner) = crate::events::path_owner_uuid(&item) {
        if authed_uuid.as_deref() != Some(owner) {
            return api_error(StatusCode::FORBIDDEN, "private item: authentication required", Some("provide ?actor=@... and x-slug-passkey header".to_string()));
        }
    }

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
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

fn vote_touches_path(a: &str, b: &str, parent_canon: &str) -> bool {
    let under = |item: &str| item == parent_canon || item.starts_with(&format!("{}/", parent_canon));
    under(a) || under(b)
}

pub async fn get_recent_votes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RecentVotesQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let limit = q.limit.unwrap_or(25).clamp(1, 200);

    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());
    let group = &reduced.ranking_group;

    let iter = group.recent_votes.iter();
    let iter: Box<dyn Iterator<Item = _>> = if let Some(parent) = &q.parent {
        let parent_can = canonicalize_item(parent);
        Box::new(iter.filter(move |v| vote_touches_path(&v.a, &v.b, &parent_can)))
    } else {
        Box::new(iter)
    };

    let authed_uuid_clone = authed_uuid.clone();
    let out: Vec<VoteRow> = iter
        .filter(move |v| {
            // Filter out votes involving private items not owned by authed actor
            for it in [&v.a, &v.b] {
                if let Some(owner) = crate::events::path_owner_uuid(it) {
                    if authed_uuid_clone.as_deref() != Some(owner) {
                        return false;
                    }
                }
            }
            true
        })
        .take(limit)
        .map(|v| VoteRow {
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
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

/// Vote history for one item (matchup: wins/losses with thread per vote).
pub async fn get_matchup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MatchupQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let item = canonicalize_item(&q.item);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    if let Some(owner) = crate::events::path_owner_uuid(&item) {
        if authed_uuid.as_deref() != Some(owner) {
            return api_error(StatusCode::FORBIDDEN, "private item: authentication required", Some("provide ?actor=@... and x-slug-passkey header".to_string()));
        }
    }

    let votes: Vec<VoteRow> = reduced
        .item_votes
        .get(&item)
        .map(|q| {
            q.iter()
                .take(limit)
                .filter(|v| {
                    // Filter out votes involving private opponents not owned by authed actor
                    for it in [&v.a, &v.b] {
                        if let Some(owner) = crate::events::path_owner_uuid(it) {
                            if authed_uuid.as_deref() != Some(owner) {
                                return false;
                            }
                        }
                    }
                    true
                })
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
    headers: HeaderMap,
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

    // Passkey: header takes priority over JSON body field.
    let passkey: Option<String> = headers
        .get("x-slug-passkey")
        .and_then(|hv| hv.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| req.passkey.clone());

    // Passkey auth gate.
    // generated_passkey is Some when this is a new actor's first ingest (server-generated key).
    let generated_passkey: Option<String> = {
        let reduced = reduced_arc.read().await;
        match reduced.actor_keys.get(&v.actor) {
            Some(stored_hash) => {
                // Actor IS registered — passkey required.
                match &passkey {
                    None => {
                        return api_error(
                            StatusCode::UNAUTHORIZED,
                            "this actor requires a passkey",
                            Some("pass --passkey <slug_sk_...> or set SLUG_PASSKEY".to_string()),
                        );
                    }
                    Some(pk) => {
                        if sha256_hex(pk) != *stored_hash {
                            return api_error(StatusCode::UNAUTHORIZED, "invalid passkey", None);
                        }
                    }
                }
                None
            }
            None => {
                // Actor NOT registered — server generates passkey on first ingest.
                if passkey.is_some() {
                    return api_error(
                        StatusCode::UNAUTHORIZED,
                        "no passkey registered for this account",
                        Some("do not supply a passkey for a new actor; the server will generate one".to_string()),
                    );
                }
                Some(format!("slug_sk_{}", uuid::Uuid::new_v4().simple()))
            }
        }
    };

    // If registering: append ActorKeyRegistration event before the Ingest event.
    let mut events_appended: usize = 0;
    if let Some(ref pk) = generated_passkey {
        let key_hash = sha256_hex(pk);
        let reg_event = crate::events::Event::ActorKeyRegistration {
            ts: v.ts,
            actor: v.actor.clone(),
            key_hash,
        };
        if let Err(err) = event_log.append(&reg_event).await {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
        }
        {
            let mut reduced = reduced_arc.write().await;
            reduced.apply_event(reg_event);
        }
        events_appended += 1;
    }

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

    // Snapshot rankings before the ingest event is applied.
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

    let ingest_event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        voter_key_id: v.voter_key_id.clone(),
        actor: v.actor.clone(),
    });

    if let Err(err) = event_log.append(&ingest_event).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
    }
    events_appended += 1;

    let actor_for_stream = v.actor.clone();
    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(ingest_event);
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
        events_appended,
        registered: generated_passkey.is_some(),
        passkey: generated_passkey,
        next: NextMoves {
            pair: "npx slugsocial pair".to_string(),
            rank: "npx slugsocial rank".to_string(),
            web: format!("https://slug.social/t/{}", primary_thread),
        },
        ranking_changes,
    }).into_response()
}

pub async fn post_check(
    State(state): State<AppState>,
    headers: HeaderMap,
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

    // Passkey verification for registered actors (read-only — no registration on check).
    let passkey: Option<String> = headers
        .get("x-slug-passkey")
        .and_then(|hv| hv.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| req.passkey.clone());

    if let Some(stored_hash) = reduced.actor_keys.get(&v.actor) {
        match &passkey {
            None => {
                return api_error(
                    StatusCode::UNAUTHORIZED,
                    "this actor requires a passkey",
                    Some("pass --passkey <slug_sk_...> or set SLUG_PASSKEY".to_string()),
                );
            }
            Some(pk) => {
                if sha256_hex(pk) != *stored_hash {
                    return api_error(StatusCode::UNAUTHORIZED, "invalid passkey", None);
                }
            }
        }
    }

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

    // Collect parent scopes touched by votes in this document.
    let voted_parents: Vec<String> = {
        let mut parents: std::collections::HashSet<String> = std::collections::HashSet::new();
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

    // Show the scoped rankings for affected parent paths; fall back to empty if no votes.
    let ranking: Vec<RankRow> = voted_parents.iter().flat_map(|parent| {
        let scoped = crate::scope_rank::build_children_rankings(&simulated, parent);
        scoped.component_rankings.into_iter().flat_map(|comp| {
            comp.ranked.into_iter().map(|r| RankRow {
                item: format!("/{}", r.item),
                score: r.score,
            })
        })
    }).collect();

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
// Digest
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct DigestQuery {
    pub actor: String,
    /// Override the lower bound (ms). Defaults to actor's last ingest timestamp.
    #[serde(default)]
    pub since: Option<i64>,
}

pub async fn get_digest(
    State(state): State<AppState>,
    Query(q): Query<DigestQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let actor = canonicalize_actor(&q.actor);

    let (since, notifications) = {
        let reduced = reduced_arc.read().await;
        let since = q.since.or_else(|| reduced.actor_last_post_ts.get(&actor).copied());
        let cutoff = since.unwrap_or(0);
        let notifications = reduced
            .notifications
            .get(&actor)
            .map(|queue| {
                queue.iter()
                    .filter(|n| n.ts > cutoff)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        (since, notifications)
    };

    Json(DigestResponse {
        ok: true,
        actor: format!("@{}", actor),
        since,
        notifications,
    }).into_response()
}

// ============================================================================
// Search
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SearchApiQuery {
    pub q: Option<String>,
    pub limit: Option<usize>,
}

fn tokenize_query(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn text_contains_all(text: &str, words: &[String]) -> bool {
    let lower = text.to_lowercase();
    words.iter().all(|w| lower.contains(w.as_str()))
}

fn text_contains_any(text: &str, words: &[String]) -> usize {
    let lower = text.to_lowercase();
    words.iter().filter(|w| lower.contains(w.as_str())).count()
}

fn snippet_around(text: &str, words: &[String], max_len: usize) -> String {
    let lower = text.to_lowercase();
    let first_pos = words.iter().filter_map(|w| lower.find(w.as_str())).min().unwrap_or(0);
    let start = first_pos.saturating_sub(max_len / 3);
    let start = if start > 0 {
        let mut i = start;
        while i < text.len() && !text.is_char_boundary(i) { i += 1; }
        text[i..].find(' ').map(|j| i + j + 1).unwrap_or(i)
    } else {
        0
    };
    let mut end = (start + max_len).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) { end += 1; }
    text[start..end].to_string()
}

pub async fn get_search(
    State(state): State<AppState>,
    Query(params): Query<SearchApiQuery>,
) -> impl IntoResponse {
    let q = params.q.unwrap_or_default();
    let limit = params.limit.unwrap_or(50).min(200);

    if q.len() < 2 {
        return Json(slug_types::SearchResponse {
            items: vec![], threads: vec![], posts: vec![],
        }).into_response();
    }

    let words = tokenize_query(&q);
    if words.is_empty() {
        return Json(slug_types::SearchResponse {
            items: vec![], threads: vec![], posts: vec![],
        }).into_response();
    }

    let reduced = state.reduced.read().await;

    let mut scored_items: Vec<(u32, slug_types::SearchItemHit)> = Vec::new();
    for item in &reduced.items {
        let mut score: u32 = 0;
        if text_contains_all(item, &words) { score += 10; }
        else if text_contains_any(item, &words) > 0 { score += 5; }
        if let Some(body) = reduced.item_bodies.get(item) {
            if text_contains_all(body, &words) { score += 6; }
            else {
                let any = text_contains_any(body, &words);
                if any > 0 { score += any as u32; }
            }
        }
        if score > 0 {
            scored_items.push((score, slug_types::SearchItemHit {
                path: format!("/{item}"),
                body: reduced.item_bodies.get(item).map(|b| snippet_around(b, &words, 120)),
            }));
        }
    }

    let mut scored_threads: Vec<(u32, i64, slug_types::SearchThreadHit)> = Vec::new();
    for (tag, ts) in &reduced.threads {
        let mut score: u32 = 0;
        if text_contains_all(tag, &words) { score += 8; }
        else if text_contains_any(tag, &words) > 0 { score += 4; }
        if score > 0 {
            let post_count = reduced.ingests_by_thread.get(tag).map(|q| q.len()).unwrap_or(0);
            scored_threads.push((score, ts.last_activity_ts, slug_types::SearchThreadHit {
                tag: format!("#{tag}"),
                post_count,
                last_activity: ts.last_activity_ts,
            }));
        }
    }

    let mut scored_posts: Vec<(u32, i64, slug_types::SearchPostHit)> = Vec::new();
    for (id, ingest) in &reduced.ingests_by_id {
        let mut score: u32 = 0;
        if text_contains_all(&ingest.raw, &words) { score += 4; }
        else {
            let any = text_contains_any(&ingest.raw, &words);
            if any > 0 { score += any as u32; }
        }
        if score > 0 {
            let thread = reduced.ingests_by_thread.iter()
                .find(|(_, ids)| ids.contains(id))
                .map(|(tag, _)| format!("#{tag}"))
                .unwrap_or_else(|| "#unknown".to_string());
            scored_posts.push((score, ingest.ts, slug_types::SearchPostHit {
                thread,
                actor: format!("@{}", ingest.actor),
                snippet: snippet_around(&ingest.raw, &words, 160),
                ts: ingest.ts,
            }));
        }
    }

    scored_items.sort_by(|a, b| b.0.cmp(&a.0));
    scored_threads.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    scored_posts.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    scored_items.truncate(limit);
    scored_threads.truncate(limit);
    scored_posts.truncate(limit);

    Json(slug_types::SearchResponse {
        items: scored_items.into_iter().map(|(_, h)| h).collect(),
        threads: scored_threads.into_iter().map(|(_, _, h)| h).collect(),
        posts: scored_posts.into_iter().map(|(_, _, h)| h).collect(),
    }).into_response()
}

// ============================================================================
// SSE streams
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SseQuery {
    pub thread: Option<String>,
}

/// Broadcast current presence counts to all SSE subscribers.
async fn broadcast_presence(state: &AppState) {
    let p = state.sse_presence.read().await;
    let global: usize = p.values().sum();
    let threads: HashMap<&str, usize> = p.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let json = serde_json::json!({ "global": global, "threads": threads });
    drop(p);
    let _ = state.presence_tx.send(json.to_string());
}

/// Drop guard: decrements presence count and broadcasts when an SSE connection closes.
struct SsePresenceGuard {
    state: AppState,
    thread: String,
}

impl Drop for SsePresenceGuard {
    fn drop(&mut self) {
        let state = self.state.clone();
        let thread = self.thread.clone();
        tokio::spawn(async move {
            {
                let mut p = state.sse_presence.write().await;
                if let Some(count) = p.get_mut(&thread) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        p.remove(&thread);
                    }
                }
            }
            broadcast_presence(&state).await;
        });
    }
}

/// Wraps a stream with a drop guard so presence is cleaned up when the connection closes.
struct GuardedStream<S> {
    inner: S,
    _guard: SsePresenceGuard,
}

impl<S: futures_util::Stream> futures_util::Stream for GuardedStream<std::pin::Pin<Box<S>>> {
    type Item = S::Item;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

pub async fn get_html_stream(
    State(state): State<AppState>,
    Query(query): Query<SseQuery>,
) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use futures_util::stream;
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

    let thread = query.thread
        .map(|t| canonicalize_tag(&t))
        .unwrap_or_default();

    // Register this connection.
    {
        let mut p = state.sse_presence.write().await;
        *p.entry(thread.clone()).or_default() += 1;
    }
    broadcast_presence(&state).await;

    // Initial thread feed.
    let initial_html = crate::html::thread_feed_html(&state).await;
    let initial = stream::once(async move {
        Ok::<_, std::convert::Infallible>(
            SseEvent::default().data(format!("#thread-feed\n{}", initial_html)),
        )
    });

    // HTML fragment updates (morphing).
    let html_rx = state.html_tx.subscribe();
    let html_updates = BroadcastStream::new(html_rx).filter_map(|msg| match msg {
        Ok(frag) => Some(Ok::<_, std::convert::Infallible>(
            SseEvent::default().data(format!("{}\n{}", frag.selector, frag.html)),
        )),
        Err(_) => None,
    });

    // Presence count updates (typed event).
    let presence_rx = state.presence_tx.subscribe();
    let presence_updates = BroadcastStream::new(presence_rx).filter_map(|msg| match msg {
        Ok(json) => Some(Ok::<_, std::convert::Infallible>(
            SseEvent::default().event("presence").data(json),
        )),
        Err(_) => None,
    });

    let updates = html_updates.merge(presence_updates);
    let guard = SsePresenceGuard { state, thread };
    let stream = GuardedStream {
        inner: Box::pin(initial.chain(updates)),
        _guard: guard,
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
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
