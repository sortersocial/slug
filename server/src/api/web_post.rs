use axum::{
    body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use slug_types::{RpcBatch, RpcBatchResponse, RpcCommand, RpcResult};

use crate::{
    api::{
        auth::{optional_principal, SLUG_SESSION_COOKIE},
        handle_rpc_batch,
        rpc::{rpc_post_redact, rpc_post_with_bearer},
    },
    canonical_path::canonicalize_tag,
    html::{thread_feed_html, thread_feed_html_for_room, thread_feed_region_markup, JsBuilder},
    reducer::{scope_from_room_wire, ScopeId},
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct WebPostForm {
    pub room: String,
    pub thread_tag: String,
    pub text: String,
    #[serde(default)]
    pub error_target: Option<String>,
    #[serde(default)]
    pub form_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WebRedactForm {
    pub post_id: String,
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

fn js_quote(s: &str) -> String {
    serde_json::to_string(s).expect("js string escaping must succeed")
}

fn js_redirect(to: &str) -> Response {
    let js = format!("window.location = {};", js_quote(to));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        .body(axum::body::Body::from(js))
        .unwrap()
}

fn js_error(error_target: &str, title: &str, detail: &str) -> Response {
    let markup = maud::html! {
        div id=(error_target.trim_start_matches('#')) {
            p class="auth-error" { (title) }
            @if !detail.is_empty() {
                pre class="muted" { (detail) }
            }
        }
    };
    JsBuilder::new()
        .morph_selector(error_target, markup)
        .into_response()
}

fn form_error_target(form: &WebPostForm) -> String {
    form.error_target
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            if s.starts_with('#') {
                s.to_string()
            } else {
                format!("#{s}")
            }
        })
        .unwrap_or_else(|| "#errors".to_string())
}

fn form_js_error(form: &WebPostForm, title: &str, detail: &str) -> Response {
    js_error(&form_error_target(form), title, detail)
}

fn js_clear_errors(error_target: &str) -> Response {
    let markup = maud::html! {
        div id=(error_target.trim_start_matches('#')) {}
    };
    JsBuilder::new()
        .morph_selector(error_target, markup)
        .into_response()
}

fn empty_error_markup(error_target: &str) -> maud::Markup {
    maud::html! {
        div id=(error_target.trim_start_matches('#')) {}
    }
}

async fn rpc_check_with_bearer(
    state: &AppState,
    bearer_token: &str,
    room: String,
    text: String,
) -> Result<RpcResult, (String, Option<String>)> {
    let mut headers = HeaderMap::new();
    let hv = HeaderValue::from_str(&format!("Bearer {bearer_token}"))
        .map_err(|_| ("invalid session token".into(), None))?;
    headers.insert(header::AUTHORIZATION, hv);

    let response = handle_rpc_batch(
        State(state.clone()),
        headers,
        axum::Json(RpcBatch(vec![RpcCommand::Check { room, text }])),
    )
    .await
    .into_response();

    let status = response.status();
    if !status.is_success() {
        return Err((format!("rpc check http {}", status), None));
    }

    let body = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|e| (e.to_string(), None))?;
    let parsed: RpcBatchResponse =
        serde_json::from_slice(&body).map_err(|e| (e.to_string(), None))?;
    let line = parsed
        .results
        .into_iter()
        .next()
        .ok_or_else(|| ("empty rpc check response".to_string(), None))?;
    if line.ok {
        line.result
            .ok_or_else(|| ("missing rpc check result".to_string(), None))
    } else {
        Err((
            line.error.unwrap_or_else(|| "check failed".to_string()),
            line.hint,
        ))
    }
}

fn post_success_response<'a>(
    state: &'a AppState,
    form: &'a WebPostForm,
    headers: &'a HeaderMap,
    jar: &'a CookieJar,
) -> impl std::future::Future<Output = Response> + 'a {
    async move {
        let error_target = form_error_target(form);
        let room = form.room.trim().to_string();
        let thread_tag = canonicalize_tag(&form.thread_tag);
        let thread_location = post_redirect_location(&room, &thread_tag);
        let form_id = form.form_id.as_deref().unwrap_or_default();
        let scope = scope_from_room_wire(&room);
        let viewer = {
            let reduced = state.reduced.read().await;
            optional_principal(headers, jar, &reduced)
        };
        let feed_markup = match &scope {
            ScopeId::Public => thread_feed_html(state).await,
            ScopeId::Room(_) => thread_feed_html_for_room(state, &room).await,
        };
        let thread_markup = thread_feed_region_markup(
            state,
            match &scope {
                ScopeId::Public => None,
                ScopeId::Room(_) => Some(room.as_str()),
            },
            &thread_tag,
            viewer.as_deref(),
        )
        .await;
        let feed_selector = match &scope {
            ScopeId::Public => "#thread-feed",
            ScopeId::Room(_) => "#room-thread-feed",
        };

        let builder = JsBuilder::new()
            .morph_selector(&error_target, empty_error_markup(&error_target))
            .morph_selector(feed_selector, feed_markup)
            .if_current_path_matches(&thread_location, |builder| {
                let builder = builder.morph_selector("#thread-feed-region", thread_markup);
                let builder = if !form_id.trim().is_empty() {
                    builder.qs(&format!("#{form_id}")).reset()
                } else {
                    builder
                };
                builder
            })
            .if_current_path_not_matches(&thread_location, |builder| builder.redirect(&thread_location));

        builder.into_response()
    }
}

async fn redact_success_response(state: &AppState) -> Response {
    let feed_markup = thread_feed_html(state).await;
    JsBuilder::new()
        .morph_selector("#thread-feed", feed_markup)
        .into_response()
}

pub async fn post_web_redact(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<WebRedactForm>,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let Some(_username) = optional_principal(&headers, &jar, &reduced) else {
        drop(reduced);
        return js_redirect("/login").into_response();
    };
    drop(reduced);

    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
        .or_else(|| jar.get(SLUG_SESSION_COOKIE).map(|c| c.value().to_string()));

    let Some(_bearer) = bearer else {
        return js_redirect("/login").into_response();
    };

    match rpc_post_redact(&state, &headers, form.post_id).await {
        Ok(RpcResult::RedactPostOk {}) => redact_success_response(&state).await.into_response(),
        Ok(_) => (StatusCode::BAD_REQUEST, "unexpected response").into_response(),
        Err((msg, hint)) => {
            let detail = hint.as_deref().unwrap_or("");
            js_error("#errors", &msg, detail).into_response()
        }
    }
}

pub async fn post_web_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<WebPostForm>,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let Some(_username) = optional_principal(&headers, &jar, &reduced) else {
        drop(reduced);
        return js_redirect("/login").into_response();
    };

    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
        .or_else(|| jar.get(SLUG_SESSION_COOKIE).map(|c| c.value().to_string()));

    drop(reduced);

    let Some(bearer) = bearer else {
        return js_redirect("/login").into_response();
    };

    let room = form.room.trim().to_string();
    let thread_tag = form.thread_tag.trim().to_string();
    let text = form.text.clone();

    if text.trim().is_empty() {
        return form_js_error(&form, "empty post", "Write something in the text area (DSL / prose).")
            .into_response();
    }

    match rpc_post_with_bearer(&state, &bearer, room.clone(), thread_tag.clone(), text).await {
        Ok(RpcResult::PostOk { .. }) => post_success_response(&state, &form, &headers, &jar)
            .await
            .into_response(),
        Ok(_) => form_js_error(&form, "unexpected response", "Post did not return PostOk.").into_response(),
        Err((msg, hint)) => form_js_error(&form, &msg, hint.as_deref().unwrap_or("")).into_response(),
    }
}

pub async fn check_web_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<WebPostForm>,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let Some(_username) = optional_principal(&headers, &jar, &reduced) else {
        drop(reduced);
        return js_redirect("/login").into_response();
    };

    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
        .or_else(|| jar.get(SLUG_SESSION_COOKIE).map(|c| c.value().to_string()));
    drop(reduced);

    let Some(bearer) = bearer else {
        return js_redirect("/login").into_response();
    };

    let room = form.room.trim().to_string();
    let thread_tag = canonicalize_tag(&form.thread_tag);
    if thread_tag.is_empty() {
        return form_js_error(&form, "missing thread tag", "Set a thread tag before posting.").into_response();
    }

    if form.text.trim().is_empty() {
        return js_clear_errors(&form_error_target(&form)).into_response();
    }

    match rpc_check_with_bearer(&state, &bearer, room, form.text.clone()).await {
        Ok(RpcResult::CheckOk { .. }) => js_clear_errors(&form_error_target(&form)).into_response(),
        Ok(_) => form_js_error(&form, "unexpected response", "Check did not return CheckOk.").into_response(),
        Err((msg, hint)) => form_js_error(&form, &msg, hint.as_deref().unwrap_or("")).into_response(),
    }
}
