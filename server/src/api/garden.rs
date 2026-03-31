use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use slug_types::*;
use std::collections::HashSet;

use crate::{
    events::canonicalize_item,
    path_types::CanonicalItemUrl,
    state::AppState,
};

use super::helpers::{api_error, item_path_for_api, vote_touches_path};

// ============================================================================
// Exploration APIs (read-only)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct PathsQuery {}

/// List root paths (items with parent "").
pub async fn get_paths(State(state): State<AppState>, Query(_q): Query<PathsQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let out: Vec<PathSummary> = reduced
        .item_children
        .get("")
        .map(|roots| {
            let mut v: Vec<PathSummary> = roots.iter()
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
pub struct LeavesQuery {}

/// List every leaf item (full path). Items that have no children. Does not scale; works for now.
pub async fn get_leaves(State(state): State<AppState>, Query(_q): Query<LeavesQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let parents: HashSet<&str> = reduced.item_children.keys().map(|s| s.as_str()).collect();
    let mut paths: Vec<String> = reduced
        .items
        .iter()
        .filter(|p| !parents.contains(p.as_str()))
        .map(|p| p.as_str().to_string())
        .collect();
    paths.sort();
    Json(LeavesResponse { paths }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ItemQuery {
    pub item: String,
    /// Return the full body without truncation (default: false, bodies >10k chars are truncated).
    #[serde(default)]
    pub full: Option<bool>,
}

pub async fn get_item(State(state): State<AppState>, Query(q): Query<ItemQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let item_str = canonicalize_item(&q.item);
    let item = CanonicalItemUrl(item_str.clone());
    let reduced = reduced_arc.read().await;

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
        Box::new(iter.filter(move |v| vote_touches_path(v.a.as_str(), v.b.as_str(), &parent_can)))
    } else {
        Box::new(iter)
    };

    let out: Vec<VoteRow> = iter
        .take(limit)
        .map(|v| VoteRow {
            ts: v.ts,
            a: v.a.as_str().to_string(),
            b: v.b.as_str().to_string(),
            ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
            actor: Some(format!("@{}", v.principal)),
            body: v.body.clone(),
            thread: Some(v.thread_id.clone()),
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
    let item_str = canonicalize_item(&q.item);
    let item = CanonicalItemUrl(item_str.clone());
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let reduced = reduced_arc.read().await;

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
                .map(|v| VoteRow {
                    ts: v.ts,
                    a: v.a.as_str().to_string(),
                    b: v.b.as_str().to_string(),
                    ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                    actor: Some(format!("@{}", v.principal)),
                    body: v.body.clone(),
                    thread: Some(v.thread_id.clone()),
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
