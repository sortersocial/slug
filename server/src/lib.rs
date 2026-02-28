pub mod api;
pub mod auth;
pub mod dsl;
pub mod event_log;
pub mod events;
pub mod html;
pub mod ranking;
pub mod reducer;
pub mod state;
pub mod timeago;

use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/", axum::routing::get(html::index))
        .route("/~", axum::routing::get(html::garden_index))
        .route("/t/:tag", axum::routing::get(html::thread_view))
        .route("/~/*path", axum::routing::get(html::ontology_path))
        .route("/api/v0/paths", axum::routing::get(api::get_paths))
        .route("/api/v0/threads", axum::routing::get(api::get_threads))
        .route("/api/v0/path", axum::routing::get(api::get_path))
        .route("/api/v0/item", axum::routing::get(api::get_item))
        .route("/api/v0/recent_votes", axum::routing::get(api::get_recent_votes))
        // REMOVED: /api/v0/vote - use /api/v0/ingest instead
        .route("/api/v0/ingest", axum::routing::post(api::post_ingest))
        .route("/api/v0/check", axum::routing::post(api::post_check))
        .route("/api/v0/pair", axum::routing::get(api::get_pair))
        .route("/api/v0/rank", axum::routing::get(api::get_rank))
        .route("/api/v0/notifications", axum::routing::get(api::get_notifications))
        .route("/api/v0/stream", axum::routing::get(api::get_stream))
        .route("/sse", axum::routing::get(api::get_html_stream))
        .route("/web/ingest", axum::routing::post(api::post_web_ingest))
        .route("/static/:filename", axum::routing::get(html::serve_theme_css))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}


