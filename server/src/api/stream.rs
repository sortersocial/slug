use axum::{
    extract::State,
    response::IntoResponse,
};

use crate::state::AppState;

// ============================================================================
// SSE streams
// ============================================================================

pub async fn get_html_stream(State(state): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use futures_util::stream;
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt as _;

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

    let stream = initial.chain(html_updates);

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
