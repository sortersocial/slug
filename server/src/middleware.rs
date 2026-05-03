use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

pub fn canonical_view_url(uri: &axum::http::Uri) -> String {
    let path = uri.path();
    if let Some(query) = uri.query() {
        let mut pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
        if pairs.is_empty() {
            return path.to_string();
        }
        pairs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let new_query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        format!("{path}?{new_query}")
    } else {
        path.to_string()
    }
}

pub async fn view_count_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() == axum::http::Method::GET {
        let path = req.uri().path();

        if !path.starts_with("/static")
            && !path.starts_with("/api")
            && !path.starts_with("/sse")
            && !path.starts_with("/auth")
            && path != "/healthz"
            && path != "/ui"
        {
            let url_key = canonical_view_url(req.uri());
            state.views.increment(url_key);
        }
    }
    next.run(req).await
}
