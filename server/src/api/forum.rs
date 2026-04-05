use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use slug_types::*;

use crate::{
    canonical_path::canonicalize_tag,
    identity::parse_username,
    state::AppState,
};

use super::helpers::api_error;

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
    #[serde(default)]
    /// Chronological offset (0 = oldest). Default: 0.
    pub offset: Option<usize>,
    /// Number of posts to return. Default: 10, max: 500.
    pub limit: Option<usize>,
    /// Only posts at or after this Unix ms timestamp.
    pub since: Option<i64>,
    /// Only posts strictly before this Unix ms timestamp.
    pub before: Option<i64>,
    /// Filter to posts whose principal username starts with this prefix (stored form, no `@`).
    pub actor: Option<String>,
    /// Return the single post with this ingest ID.
    pub post_id: Option<String>,
}

/// Thread (forum) detail by tag -- paginated, filterable posts, full body.
pub async fn get_thread(State(state): State<AppState>, Query(q): Query<ThreadDetailQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let tag = canonicalize_tag(&q.tag);
    let actor_prefix = match q.actor.as_deref().map(str::trim) {
        None | Some("") => String::new(),
        Some(s) => match parse_username(s) {
            Ok(u) => u,
            Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid actor filter", Some(msg)).into_response(),
        },
    };
    let reduced = reduced_arc.read().await;

    // Single post lookup by ingest ID -- return full body untruncated.
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
                    actor: ing.principal.clone(),
                    body: ing.raw.clone(),
                    truncated: false,
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

    let filtered: Vec<(usize, _)> = all_ids
        .into_iter()
        .enumerate()
        .filter_map(|(idx, id)| reduced.ingests_by_id.get(&id).map(|ing| (idx, ing.clone())))
        .filter(|(_, ing)| q.since.map_or(true, |s| ing.ts >= s))
        .filter(|(_, ing)| q.before.map_or(true, |b| ing.ts < b))
        .filter(|(_, ing)| actor_prefix.is_empty() || ing.principal.to_lowercase().starts_with(&actor_prefix))
        .collect();

    let total = filtered.len();

    const MAX_BODY: usize = 2000;
    let posts: Vec<PostRow> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(idx, ing)| {
            let (body, truncated) = if ing.raw.len() > MAX_BODY {
                (ing.raw[..MAX_BODY].to_string(), true)
            } else {
                (ing.raw.clone(), false)
            };
            PostRow {
                id: ing.id.clone(),
                index: idx,
                ts: ing.ts,
                actor: ing.principal.clone(),
                body,
                truncated,
            }
        })
        .collect();

    Json(ThreadDetailResponse {
        thread: format!("#{}", tag),
        posts,
        total,
        offset,
    }).into_response()
}
