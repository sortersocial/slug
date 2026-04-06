#[macro_use]
pub mod paths;
pub mod api;
pub mod canonical_path;
pub mod dsl;
pub mod html;
pub mod event_log;
pub mod events;
pub mod identity;
pub mod middleware;
pub mod path_types;
pub mod ranking;
pub mod reducer;
pub mod scope_rank;
pub mod state;
pub mod timeago;

use axum::Router;
use axum::routing::post;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub use reducer::ReducerState;

pub fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/static/:filename", axum::routing::get(crate::html::serve_theme_css))
        .route("/join/:token", axum::routing::get(api::get_join_invite))
        .route("/auth/login", axum::routing::get(api::get_auth_login))
        .route("/auth/callback", axum::routing::get(api::get_auth_callback))
        .route("/auth/complete", axum::routing::get(api::get_auth_complete))
        .route("/auth/choose-username", axum::routing::get(api::get_choose_username))
        .route("/auth/choose-username", axum::routing::post(api::post_choose_username))
        .route("/api/v0/pending-session", axum::routing::post(api::post_pending_session))
        .route("/api/v0/pending-session/:id", axum::routing::get(api::get_pending_session))
        .route("/api/v0/whoami", axum::routing::get(api::get_whoami))
        .route("/api/v0/rpc", post(api::handle_rpc_batch))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
