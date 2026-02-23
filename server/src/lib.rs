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
        .route("/~/:tag", axum::routing::get(html::thread_page))
        .route("/~/:tag/*item", axum::routing::get(html::item_page))
        .route("/api/v0/tags", axum::routing::get(api::get_tags))
        .route("/api/v0/tag", axum::routing::get(api::get_tag))
        .route("/api/v0/item", axum::routing::get(api::get_item))
        .route("/api/v0/recent_votes", axum::routing::get(api::get_recent_votes))
        // REMOVED: /api/v0/vote - use /api/v0/ingest instead
        .route("/api/v0/ingest", axum::routing::post(api::post_ingest))
        .route("/api/v0/check", axum::routing::post(api::post_check))
        .route("/api/v0/pair", axum::routing::get(api::get_pair))
        .route("/api/v0/rank", axum::routing::get(api::get_rank))
        .route("/api/v0/notifications", axum::routing::get(api::get_notifications))
        .route("/static/:filename", axum::routing::get(html::serve_theme_css))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}


