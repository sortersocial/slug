pub mod api;
pub mod auth;
pub mod dsl;
pub mod event_log;
pub mod events;
pub mod html;
pub mod ranking;
pub mod reducer;
pub mod state;

use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/", axum::routing::get(html::index))
        .route("/t/:tag", axum::routing::get(html::tag_page))
        .route("/t/:tag/a/:aspect", axum::routing::get(html::tag_aspect_page))
        .route("/api/v0/tags", axum::routing::get(api::get_tags))
        .route("/api/v0/tag", axum::routing::get(api::get_tag))
        .route("/api/v0/item", axum::routing::get(api::get_item))
        .route("/api/v0/recent_votes", axum::routing::get(api::get_recent_votes))
        .route("/api/v0/vote", axum::routing::post(api::post_vote))
        .route("/api/v0/ingest", axum::routing::post(api::post_ingest))
        .route("/api/v0/pair", axum::routing::get(api::get_pair))
        .route("/api/v0/rank", axum::routing::get(api::get_rank))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}


