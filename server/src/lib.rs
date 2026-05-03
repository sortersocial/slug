#[macro_use]
pub mod paths;
pub mod api;
pub mod canonical_path;
pub mod dsl;
pub mod external_resolver;
pub mod form_template;
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
pub mod write_cmd;

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::state::{AppConfig, AppState};
use crate::write_cmd::WriteCmd;
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

pub use reducer::ReducerState;

/// Build application state and start the serialized writer task (required for RPC/auth writes).
pub fn create_app_state(cfg: AppConfig) -> AppState {
    let event_log = crate::event_log::EventLog::new(cfg.event_log_path.clone());
    let (stream_tx, _) = broadcast::channel(64);
    let (js_tx, _) = broadcast::channel(64);
    let (write_tx, write_rx) = mpsc::channel::<WriteCmd>(256);
    let state = AppState {
        cfg: Arc::new(cfg),
        event_log: Arc::new(event_log),
        reduced: Arc::new(RwLock::new(crate::reducer::ReducerState::default())),
        pending_sessions: Arc::new(RwLock::new(HashMap::new())),
        invites: Arc::new(RwLock::new(HashMap::new())),
        stream_tx,
        js_tx,
        write_tx,
    };
    tokio::spawn(crate::api::write_actor::writer_actor(write_rx, state.clone()));
    state
}

/// Spawn the serialized writer. Used by integration tests after seeding reducer state in-process.
pub fn spawn_writer_actor_for_test(
    state: AppState,
    rx: tokio::sync::mpsc::Receiver<WriteCmd>,
) {
    tokio::spawn(crate::api::write_actor::writer_actor(rx, state));
}

pub fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/static/:filename", get(crate::html::serve_static))
        .route("/", get(crate::html::home))
        .route("/login", get(api::get_web_login))
        .route("/logout", get(api::get_logout))
        .route("/ui", post(api::post_ui_html))
        .route("/theme", post(crate::html::post_theme))
        .route("/sse", get(api::get_html_stream))
        .route("/stream", get(api::get_stream))
        .route("/search", get(crate::html::search_page))
        .route("/search/results", get(crate::html::search_results_fragment))
        .route("/u/:username", get(crate::html::user_profile_page))
        .route("/try", get(crate::html::editor_page))
        .route("/try/check", post(crate::html::editor_check))
        .route("/vote/compare", get(crate::html::vote_compare_page))
        .route(
            "/r/:room_key/vote/compare",
            get(crate::html::room_vote_compare_page),
        )
        .route("/~", get(crate::html::garden_index))
        .route("/~/*path", get(crate::html::ontology_path))
        .route("/-", get(crate::html::external_garden_index))
        .route("/-/*path", get(crate::html::external_ontology_path))
        .route("/r/:room_key/~", get(crate::html::room_garden_index))
        .route(
            "/r/:room_key/~/*path",
            get(crate::html::room_ontology_path),
        )
        .route(
            "/r/:room_key/-",
            get(crate::html::room_external_garden_index),
        )
        .route(
            "/r/:room_key/-/*path",
            get(crate::html::room_external_ontology_path),
        )
        .route("/t/:tag/:index", get(crate::html::thread_post_view))
        .route("/t/:tag", get(crate::html::thread_view))
        .route(
            "/r/:room_key/t/:thread_tag/:index",
            get(crate::html::room_thread_post_view),
        )
        .route(
            "/r/:room_key/t/:thread_tag",
            get(crate::html::room_thread_view),
        )
        .route("/r/:room_key", get(crate::html::room_page))
        .route("/join/:token", get(api::get_join_invite))
        .route("/auth/login", get(api::get_auth_login))
        .route("/auth/callback", get(api::get_auth_callback))
        .route("/auth/complete", get(api::get_auth_complete))
        .route("/auth/choose-username", get(api::get_choose_username))
        .route("/auth/choose-username", post(api::post_choose_username))
        .route("/api/v0/pending-session", post(api::post_pending_session))
        .route("/api/v0/pending-session/:id", get(api::get_pending_session))
        .route("/api/v0/whoami", get(api::get_whoami))
        .route("/api/v0/rpc", post(api::handle_rpc_batch))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
