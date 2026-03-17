use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

/// Middleware that increments view counts for HTML view routes.
/// Uses fire-and-forget via MPSC so it never blocks the handler.
pub async fn view_count_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let is_html_view = req.method() == axum::http::Method::GET
        && (path == "/"
            || path == "/~"
            || path.starts_with("/~/")
            || path.starts_with("/t/"));

    if is_html_view {
        state.views.increment(path);
    }

    next.run(req).await
}
