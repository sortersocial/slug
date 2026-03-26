#[macro_use]
pub mod paths;
pub mod api;
pub mod auth;
pub mod bot;
pub mod dsl;
pub mod signal_bot;
pub mod event_log;
pub mod events;
pub mod html;
pub mod middleware;
pub mod ranking;
pub mod reducer;
pub mod scope_rank;
pub mod state;
pub mod timeago;
pub mod views;

use axum::{middleware as axum_middleware, Router};
use tower_http::trace::TraceLayer;

use crate::{middleware::view_count_middleware, state::AppState};

pub use reducer::ReducerState;

pub fn create_app(state: AppState) -> Router {
    let state_for_middleware = state.clone();
    Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/", axum::routing::get(html::index))
        .route("/~", axum::routing::get(html::garden_index))
        .route("/t/:tag", axum::routing::get(html::thread_view))
        .route("/t/:tag/:id", axum::routing::get(html::thread_post_view))
        .route("/t/:tag/:id/expand", axum::routing::get(html::thread_post_expand))
        .route("/~/*path", axum::routing::get(html::ontology_path))
        .route("/api/v0/paths", axum::routing::get(api::get_paths))
        .route("/api/v0/leaves", axum::routing::get(api::get_leaves))
        .route("/api/v0/threads", axum::routing::get(api::get_threads))
        .route("/api/v0/thread", axum::routing::get(api::get_thread))
        .route("/api/v0/item", axum::routing::get(api::get_item))
        .route("/api/v0/matchup", axum::routing::get(api::get_matchup))
        .route("/api/v0/recent_votes", axum::routing::get(api::get_recent_votes))
        // REMOVED: /api/v0/vote - use /api/v0/ingest instead
        .route("/api/v0/ingest", axum::routing::post(api::post_ingest))
        .route("/api/v0/check", axum::routing::post(api::post_check))
        .route("/api/v0/pair", axum::routing::get(api::get_pair))
        .route("/api/v0/rank", axum::routing::get(api::get_rank))
        .route("/api/v0/global-rank", axum::routing::get(api::get_global_rank))
        .route("/api/v0/rank-history", axum::routing::get(api::get_rank_history))
        .route("/api/v0/feed", axum::routing::get(api::get_feed))
        .route("/api/v0/search", axum::routing::get(api::get_search))
        .route("/api/v0/stream", axum::routing::get(api::get_stream))
        .route("/sse", axum::routing::get(api::get_html_stream))
        .route("/search", axum::routing::get(html::search_page))
        .route("/search/results", axum::routing::get(html::search_results_fragment))
        .route("/try", axum::routing::get(html::editor_page))
        .route("/try/check", axum::routing::post(html::editor_check))
        .route("/static/:filename", axum::routing::get(html::serve_theme_css))
        .with_state(state)
        .layer(axum_middleware::from_fn_with_state(
            state_for_middleware,
            view_count_middleware,
        ))
        .layer(TraceLayer::new_for_http())
}


