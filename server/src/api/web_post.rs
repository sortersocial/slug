use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use slug_types::RpcResult;

use crate::{
    api::{
        auth::{optional_principal, SLUG_SESSION_COOKIE},
        rpc::rpc_post_with_bearer,
    },
    canonical_path::canonicalize_tag,
    html::layout,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct WebPostForm {
    pub room: String,
    pub thread_tag: String,
    pub text: String,
}

fn post_redirect_location(room: &str, thread_tag: &str) -> String {
    let tag = canonicalize_tag(thread_tag);
    if room.trim() == "public" {
        format!("/t/{tag}")
    } else {
        let room = room.trim();
        let Some((a, b)) = room.split_once('/') else {
            return "/".to_string();
        };
        format!("/r/{a}/{b}/t/{tag}")
    }
}

pub async fn post_web_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<WebPostForm>,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let Some(username) = optional_principal(&headers, &jar, &reduced) else {
        drop(reduced);
        return Redirect::temporary("/login").into_response();
    };

    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
        .or_else(|| jar.get(SLUG_SESSION_COOKIE).map(|c| c.value().to_string()));

    drop(reduced);

    let Some(bearer) = bearer else {
        return Redirect::temporary("/login").into_response();
    };

    let room = form.room.trim().to_string();
    let thread_tag = form.thread_tag.trim().to_string();
    let text = form.text.clone();

    if text.trim().is_empty() {
        return error_page(
            "empty post",
            "Write something in the text area (DSL / prose).",
            &username,
        )
        .into_response();
    }

    match rpc_post_with_bearer(&state, &bearer, room.clone(), thread_tag.clone(), text).await {
        Ok(RpcResult::PostOk { .. }) => {
            Redirect::to(&post_redirect_location(&room, &thread_tag)).into_response()
        }
        Ok(_) => error_page("unexpected response", "Post did not return PostOk.", &username).into_response(),
        Err((msg, hint)) => error_page(
            &msg,
            hint.as_deref().unwrap_or(""),
            &username,
        )
        .into_response(),
    }
}

fn error_page(title: &str, detail: &str, user: &str) -> impl IntoResponse {
    use maud::html;
    let body = html! {
        nav class="breadcrumb" {
            a href="/" { "slug.social" }
        }
        h1 { "could not post" }
        p { (title) }
        @if !detail.is_empty() {
            pre class="muted" { (detail) }
        }
        p class="muted" { "signed in as @" (user) " · " a href="/" { "home" } }
    };
    let page = layout("post error — slug.social", "view-thread", body, None);
    (StatusCode::BAD_REQUEST, Html(page.into_string()))
}
