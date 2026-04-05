#[macro_use]
pub mod paths;
pub mod api;
pub mod dsl;
pub mod event_log;
pub mod events;
pub mod middleware;
pub mod path_types;
pub mod ranking;
pub mod reducer;
pub mod scope_rank;
pub mod state;
pub mod timeago;

use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub use reducer::ReducerState;

pub fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/auth/login", axum::routing::get(api::get_auth_login))
        .route("/auth/callback", axum::routing::get(api::get_auth_callback))
        .route("/auth/complete", axum::routing::get(api::get_auth_complete))
        .route("/auth/choose-username", axum::routing::get(api::get_choose_username))
        .route("/auth/choose-username", axum::routing::post(api::post_choose_username))
        .route("/api/v0/pending-session", axum::routing::post(api::post_pending_session))
        .route("/api/v0/pending-session/:id", axum::routing::get(api::get_pending_session))
        .route("/api/v0/whoami", axum::routing::get(api::get_whoami))
        .route("/debug/query", axum::routing::get(api::get_debug_query))
        .route("/api/v0/paths", axum::routing::get(api::get_paths))
        .route("/api/v0/leaves", axum::routing::get(api::get_leaves))
        .route("/api/v0/threads", axum::routing::get(api::get_threads))
        .route("/api/v0/thread", axum::routing::get(api::get_thread))
        .route("/api/v0/item", axum::routing::get(api::get_item))
        .route("/api/v0/matchup", axum::routing::get(api::get_matchup))
        .route("/api/v0/recent_votes", axum::routing::get(api::get_recent_votes))
        // REMOVED: /api/v0/vote - use /api/v0/ingest instead
        .route("/api/v0/thread", axum::routing::post(api::post_create_thread))
        .route("/api/v0/thread/:id/grants", axum::routing::post(api::post_thread_grants))
        .route("/api/v0/ingest", axum::routing::post(api::post_ingest))
        .route("/api/v0/check", axum::routing::post(api::post_check))
        .route("/api/v0/pair", axum::routing::get(api::get_pair))
        .route("/api/v0/rank", axum::routing::get(api::get_rank))
        .route("/api/v0/global-rank", axum::routing::get(api::get_global_rank))
        .route("/api/v0/rank-history", axum::routing::get(api::get_rank_history))
        .route("/api/v0/feed", axum::routing::get(api::get_feed))
        .route("/api/v0/search", axum::routing::get(api::get_search))
        // .route("/api/v0/stream", axum::routing::get(api::get_stream))
        .with_state(state)
        // view_count_middleware currently disabled while HTML views are offline
        .layer(TraceLayer::new_for_http())
}


