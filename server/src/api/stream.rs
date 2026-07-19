use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use slug_types::room_id_from_route_segment;

use crate::{
    api::optional_principal,
    html::user_can_view_room,
    state::{AppState, JsSnippetAudience},
};

// ============================================================================
// SSE streams
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SseQuery {
    pub path: Option<String>,
}

/// Room wire id for the subscriber's page URL, if it is a private `/r/{seg}/…` route.
fn sse_context_room_id(subscribed_path: &str) -> Option<String> {
    let path = subscribed_path.split('?').next().unwrap_or(subscribed_path);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 && segs[0] == "r" {
        room_id_from_route_segment(segs[1])
    } else {
        None
    }
}

fn sse_snippet_allowed_for_subscriber(
    snippet: &crate::state::JsSnippet,
    subscribed_path: &str,
    username: Option<&str>,
    reduced: &crate::reducer::ReducerState,
) -> bool {
    let path_matches = snippet.path_prefixes.is_empty()
        || snippet.path_prefixes.iter().any(|prefix| {
            subscribed_path == *prefix || subscribed_path.starts_with(&format!("{prefix}?"))
        });
    if !path_matches {
        return false;
    }

    match &snippet.audience {
        JsSnippetAudience::Public => true,
        JsSnippetAudience::RoomViewers(room_id) => user_can_view_room(reduced, room_id, username),
    }
}

/// Fixed response for unauthorized or invalid `/sse` subscription (no JS execution).
fn sse_forbidden_or_gone() -> impl IntoResponse {
    (
        StatusCode::FORBIDDEN,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/event-stream; charset=utf-8",
        )],
        ": forbidden\n\n",
    )
}

pub async fn get_html_stream(
    Query(q): Query<SseQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use tokio_stream::wrappers::BroadcastStream;

    let subscribed_path = q.path.unwrap_or_default();
    let path_only = subscribed_path
        .split('?')
        .next()
        .unwrap_or(&subscribed_path)
        .to_string();

    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    let path_room_id = sse_context_room_id(&subscribed_path);

    // Private room routes require a signed-in member (same as HTML).
    if path_only.starts_with("/r/") && path_room_id.is_none() {
        drop(reduced);
        return sse_forbidden_or_gone().into_response();
    }
    if let Some(ref rid) = path_room_id {
        if !user_can_view_room(&reduced, rid, user.as_deref()) {
            drop(reduced);
            return sse_forbidden_or_gone().into_response();
        }
    }
    drop(reduced);

    let user_for_filter = user;
    let subscribed_path_owned = subscribed_path;
    let state_clone = state.clone();
    let js_rx = state.js_tx.subscribe();
    use futures_util::StreamExt;
    let js_updates = BroadcastStream::new(js_rx).filter_map(move |msg| {
        let state = state_clone.clone();
        let subscribed_path = subscribed_path_owned.clone();
        let user_for_filter = user_for_filter.clone();
        async move {
            match msg {
                Ok(snippet) => {
                    let reduced = state.reduced.read().await;
                    let allowed = sse_snippet_allowed_for_subscriber(
                        &snippet,
                        &subscribed_path,
                        user_for_filter.as_deref(),
                        &reduced,
                    );
                    drop(reduced);
                    if allowed {
                        Some(Ok::<_, std::convert::Infallible>(
                            SseEvent::default().data(snippet.code),
                        ))
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        }
    });

    Sse::new(js_updates)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub async fn get_stream(State(state): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

    let rx = state.stream_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(ev) => {
            let data = serde_json::to_string(&ev).unwrap_or_default();
            Some(Ok::<_, std::convert::Infallible>(
                SseEvent::default().event("ingest").data(data),
            ))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
