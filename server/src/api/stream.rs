use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::{
    html::{live_sse_multi_topic, topics_from_legacy_sse_path},
    state::AppState,
};

// ============================================================================
// SSE streams
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SseQuery {
    pub path: Option<String>,
}

pub async fn get_html_stream(
    Query(q): Query<SseQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let subscribed_path = q.path.unwrap_or_default();
    let Some((topics, gate)) = topics_from_legacy_sse_path(&subscribed_path) else {
        use axum::http::StatusCode;
        return (
            StatusCode::FORBIDDEN,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/event-stream; charset=utf-8",
            )],
            ": forbidden\n\n",
        )
            .into_response();
    };
    live_sse_multi_topic(State(state), headers, jar, topics, gate).await
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
