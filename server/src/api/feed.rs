use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::{
    events::canonicalize_actor,
    state::AppState,
};

// ============================================================================
// Feed -- global reverse-chronological ingest stream since a cutoff
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    pub actor: String,
    /// Override the lower bound (ms). Defaults to actor's last ingest timestamp.
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

pub async fn get_feed(
    State(state): State<AppState>,
    Query(q): Query<FeedQuery>,
) -> impl IntoResponse {
    const DEFAULT_LIMIT: usize = 50;
    const MAX_LIMIT: usize = 200;

    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let actor = canonicalize_actor(&q.actor);
    let since = q.since.or_else(|| reduced.actor_last_post_ts.get(&actor).copied());
    let cutoff = since.unwrap_or(0);
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = q.offset.unwrap_or(0);

    // Scan ingests_ordered backwards (newest first), stop once ts <= cutoff.
    let matching: Vec<&str> = reduced.ingests_ordered.iter().rev()
        .map(|id| id.as_str())
        .take_while(|id| reduced.ingests_by_id.get(*id).map_or(false, |ing| ing.ts > cutoff))
        .collect();

    let total = matching.len();
    let posts: Vec<slug_types::FeedPost> = matching.into_iter()
        .skip(offset)
        .take(limit)
        .filter_map(|id| reduced.ingests_by_id.get(id))
        .map(|ing| {
            // Extract primary thread tag from raw doc.
            let thread = crate::dsl::parse_full(&ing.raw).ok().and_then(|doc| {
                doc.statements.into_iter().find_map(|s| {
                    if let crate::dsl::Stmt::Hashtag { name } = s {
                        Some(crate::events::canonicalize_tag(&name))
                    } else { None }
                })
            });
            let thread_post_index = thread.as_deref().and_then(|t| {
                reduced.ingests_by_thread.get(t).and_then(|q| {
                    q.iter().rev().position(|id| id == &ing.id).map(|i| i + 1)
                })
            });
            slug_types::FeedPost {
                ts: ing.ts,
                id: ing.id.clone(),
                thread,
                thread_post_index,
                body: ing.raw.clone(),
            }
        })
        .collect();

    Json(slug_types::FeedResponse {
        actor: format!("@{}", actor),
        since,
        posts,
        total,
    }).into_response()
}
