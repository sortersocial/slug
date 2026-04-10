use axum::{
    extract::{Query, State},
    response::IntoResponse,
};
use serde::Deserialize;

use crate::state::AppState;

// ============================================================================
// SSE streams
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SseQuery {
    pub path: Option<String>,
}

pub async fn get_html_stream(Query(q): Query<SseQuery>, State(state): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

    let path = q.path.unwrap_or_default();
    let js_rx = state.js_tx.subscribe();
    let js_updates = BroadcastStream::new(js_rx).filter_map(move |msg| match msg {
        Ok(snippet)
            if snippet.path_prefixes.is_empty()
                || snippet
                    .path_prefixes
                    .iter()
                    .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}?"))) =>
        {
            Some(Ok::<_, std::convert::Infallible>(
                SseEvent::default().data(snippet.code),
            ))
        }
        Err(_) => None,
        _ => None,
    });

    Sse::new(js_updates).keep_alive(KeepAlive::default())
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
