use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use slug_types::*;
use std::collections::HashSet;

use crate::{
    events::{canonicalize_item, path_owner_uuid},
    path_types::CanonicalItemUrl,
    state::AppState,
};

use super::auth::verified_actor_uuid;
use super::helpers::{api_error, item_path_for_api, vote_touches_path};

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
                    match path_owner_uuid(path.as_str()) {
                        None => true,
                        Some(owner) => authed_uuid.as_deref() == Some(owner),
                    }
                })
                .map(|path| {
                let children = reduced.item_children.get(path.as_str()).map(|s| s.len()).unwrap_or(0);
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
    let parents: HashSet<&str> = reduced.item_children.keys().map(|s| s.as_str()).collect();
    let mut paths: Vec<String> = reduced
        .items
        .iter()
        .filter(|p| !parents.contains(p.as_str()))
        .filter(|p| {
            match path_owner_uuid(p.as_str()) {
                None => true,
                Some(owner) => authed_uuid.as_deref() == Some(owner),
            }
        })
        .map(|p| p.as_str().to_string())
        .collect();
    paths.sort();
    Json(LeavesResponse { paths }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ItemQuery {
    pub item: String,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
    /// Return the full body without truncation (default: false, bodies >10k chars are truncated).
    #[serde(default)]
    pub full: Option<bool>,
}

pub async fn get_item(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<ItemQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let item_str = canonicalize_item(&q.item);
    let item = CanonicalItemUrl(item_str.clone());
    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    if let Some(owner) = path_owner_uuid(&item_str) {
        if authed_uuid.as_deref() != Some(owner) {
            return api_error(StatusCode::FORBIDDEN, "private item: authentication required", Some("provide ?actor=@... and x-slug-passkey header".to_string()));
        }
    }

    if !reduced.items.contains(&item) {
        return api_error(
            StatusCode::NOT_FOUND,
            "item not found",
            Some(format!("{} does not exist", item_path_for_api(&item_str))),
        );
    }

    const MAX_ITEM_BODY: usize = 10_000;
    let want_full = q.full.unwrap_or(false);

    let (body, truncated, body_len) = match reduced.item_bodies.get(&item) {
        None => (None, false, 0),
        Some(raw) => {
            let char_len = raw.chars().count();
            if !want_full && char_len > MAX_ITEM_BODY {
                let byte_end = raw.char_indices().nth(MAX_ITEM_BODY).map(|(i, _)| i).unwrap_or(raw.len());
                (Some(raw[..byte_end].to_string()), true, char_len)
            } else {
                (Some(raw.clone()), false, 0)
            }
        }
    };

    let threads: Vec<String> = reduced
        .item_threads
        .get(&item)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();

    Json(ItemResponse {
        item: item_str.clone(),
        body,
        truncated,
        body_len,
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
        Box::new(iter.filter(move |v| vote_touches_path(v.a.as_str(), v.b.as_str(), &parent_can)))
    } else {
        Box::new(iter)
    };

    let authed_uuid_clone = authed_uuid.clone();
    let out: Vec<VoteRow> = iter
        .filter(move |v| {
            // Filter out votes involving private items not owned by authed actor
            for it in [&v.a, &v.b] {
                if let Some(owner) = path_owner_uuid(it.as_str()) {
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
            a: v.a.as_str().to_string(),
            b: v.b.as_str().to_string(),
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
    let item_str = canonicalize_item(&q.item);
    let item = CanonicalItemUrl(item_str.clone());
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let reduced = reduced_arc.read().await;
    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    if let Some(owner) = path_owner_uuid(&item_str) {
        if authed_uuid.as_deref() != Some(owner) {
            return api_error(StatusCode::FORBIDDEN, "private item: authentication required", Some("provide ?actor=@... and x-slug-passkey header".to_string()));
        }
    }

    if !reduced.items.contains(&item) {
        return api_error(
            StatusCode::NOT_FOUND,
            "item not found",
            Some(format!("{} does not exist", item_path_for_api(&item_str))),
        );
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
                        if let Some(owner) = path_owner_uuid(it.as_str()) {
                            if authed_uuid.as_deref() != Some(owner) {
                                return false;
                            }
                        }
                    }
                    true
                })
                .map(|v| VoteRow {
                    ts: v.ts,
                    a: v.a.as_str().to_string(),
                    b: v.b.as_str().to_string(),
                    ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                    actor: Some(format!("@{}", v.actor)),
                    body: v.body.clone(),
                    thread: Some(format!("#{}", v.thread)),
                })
                .collect()
        })
        .unwrap_or_default();

    Json(MatchupResponse {
        item: item_path_for_api(&item_str),
        votes,
    })
    .into_response()
}
