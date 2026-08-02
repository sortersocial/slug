//! Single `POST /ui` entry: parse `__rpc__` → [`HtmlUiAction`], resolve [`WebSession`] once, dispatch.

use axum::{
    body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use slug_types::{RpcBatch, RpcBatchResponse, RpcCommand, RpcResult};
use std::collections::HashMap;

use crate::{
    api::{
        auth::{resolve_web_session, WebSession},
        handle_rpc_batch,
        rpc::{rpc_post_redact, rpc_post_with_bearer, rpc_room_delete, rpc_thread_graduate},
    },
    canonical_path::canonicalize_tag,
    resolvers::resolve_github_children,
    html::vote_compare_post_success_js,
    html::{
        external_resolver_status_markup, fragment_new_thread_slot, login_to_post_hint_markup,
        parse_html_ui_from_form, room_members_section_markup, thread_feed_html,
        thread_feed_html_for_room, thread_latest_page_region, thread_ui_collapse_redacted_post,
        garden_ui_copy_rank, thread_ui_copy_thread, thread_ui_expand_post_full,
        thread_ui_expand_redacted_post, ui_js_warn, user_can_post_room, user_can_view_room,
        HtmlUiAction, JsBuilder, ThreadNav,
    },
    reducer::{scope_from_room_wire, ScopeId},
    state::AppState,
};

pub async fn post_ui_html(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let action = match parse_html_ui_from_form(&form) {
        Ok(a) => a,
        Err(e) => return ui_js_warn(&e.to_string()).into_response(),
    };

    let reduced = state.reduced.read().await;
    let session = resolve_web_session(&headers, &jar, &reduced);
    drop(reduced);

    dispatch_ui_action(&state, session.as_ref(), action).await
}

/// All UI command logic: HTTP extractors stop above; this only sees [`AppState`], session, and [`HtmlUiAction`].
fn sanitize_vote_compare_next(next: &str) -> String {
    let s = next.trim();
    if s.starts_with('/') && !s.starts_with("//") && s.len() < 8192 {
        s.to_string()
    } else {
        "/".to_string()
    }
}

fn sanitize_garden_pin_next(next: &str) -> String {
    let s = next.trim();
    if s.starts_with('/') && !s.starts_with("//") && s.len() < 8192 {
        s.to_string()
    } else {
        "/".to_string()
    }
}

fn login_redirect_for(next: &str) -> String {
    format!(
        "/login?next={}",
        urlencoding::encode(&sanitize_garden_pin_next(next))
    )
}

fn redirect_with_pin_cookie(cookie_header_value: &str, location: &str) -> Response {
    let loc = HeaderValue::try_from(location).unwrap_or_else(|_| HeaderValue::from_static("/"));
    let hv = HeaderValue::try_from(cookie_header_value).expect("set-cookie header");
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, loc)
        .header(header::SET_COOKIE, hv)
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn dispatch_ui_action(
    state: &AppState,
    session: Option<&WebSession>,
    action: HtmlUiAction,
) -> Response {
    match action {
        HtmlUiAction::PostIngest {
            room,
            thread_tag,
            text,
            error_target,
            form_id,
        } => {
            let Some(session) = session else {
                return js_redirect("/login").into_response();
            };
            let room = room.trim().to_string();
            let thread_tag = thread_tag.trim().to_string();
            if text.trim().is_empty() {
                return form_js_error(
                    error_target.as_ref(),
                    "empty post",
                    "Write something in the text area (DSL / prose).",
                )
                .into_response();
            }
            match rpc_post_with_bearer(
                state,
                &session.bearer,
                room.clone(),
                thread_tag.clone(),
                None,
                text,
            )
            .await
            {
                Ok(RpcResult::PostOk { .. }) => post_success_response(
                    state,
                    &room,
                    &thread_tag,
                    error_target.as_ref(),
                    form_id.as_ref(),
                    Some(session.username.as_str()),
                )
                .await
                .into_response(),
                Ok(_) => form_js_error(
                    error_target.as_ref(),
                    "unexpected response",
                    "Post did not return PostOk.",
                )
                .into_response(),
                Err((msg, hint)) => {
                    form_js_error(error_target.as_ref(), &msg, hint.as_deref().unwrap_or(""))
                        .into_response()
                }
            }
        }
        HtmlUiAction::CheckIngest {
            room,
            thread_tag,
            text,
            error_target,
            form_id: _,
        } => {
            let Some(session) = session else {
                return js_redirect("/login").into_response();
            };
            let room = room.trim().to_string();
            let thread_tag = canonicalize_tag(&thread_tag);
            if thread_tag.is_empty() {
                return form_js_error(
                    error_target.as_ref(),
                    "missing thread tag",
                    "Set a thread tag before posting.",
                )
                .into_response();
            }
            if text.trim().is_empty() {
                return js_clear_errors(&form_error_target(error_target.as_ref())).into_response();
            }
            match rpc_check_with_bearer(state, &session.bearer, room, text.clone()).await {
                Ok(RpcResult::CheckOk { .. }) => {
                    js_clear_errors(&form_error_target(error_target.as_ref())).into_response()
                }
                Ok(_) => form_js_error(
                    error_target.as_ref(),
                    "unexpected response",
                    "Check did not return CheckOk.",
                )
                .into_response(),
                Err((msg, hint)) => {
                    form_js_error(error_target.as_ref(), &msg, hint.as_deref().unwrap_or(""))
                        .into_response()
                }
            }
        }
        HtmlUiAction::VoteComparePost {
            room,
            thread_tag,
            left_item,
            right_item,
            ratio_left,
            ratio_right,
            explanation,
            next,
            pool,
            form_action,
        } => {
            if form_action != "/ui" {
                return (
                    StatusCode::BAD_REQUEST,
                    "invalid vote_compare_post form_action",
                )
                    .into_response();
            }
            let Some(session) = session else {
                return js_redirect(&login_redirect_for(&next)).into_response();
            };
            let err_tgt = Some("vote-compare-errors".to_string());
            let room = room.trim().to_string();
            let thread_tag = canonicalize_tag(&thread_tag);
            if thread_tag.is_empty() {
                return form_js_error(
                    err_tgt.as_ref(),
                    "missing thread",
                    "Thread tag is required.",
                )
                .into_response();
            }
            let exp = explanation.trim();
            if exp.is_empty() {
                return form_js_error(
                    err_tgt.as_ref(),
                    "empty explanation",
                    "Write a short reason in the textarea.",
                )
                .into_response();
            }
            let left_id = match crate::path_types::ItemId::parse(left_item.trim()) {
                Some(i) => i.normalized_storage(),
                None => {
                    return form_js_error(err_tgt.as_ref(), "bad item", "Invalid left item path.")
                        .into_response();
                }
            };
            let right_id = match crate::path_types::ItemId::parse(right_item.trim()) {
                Some(i) => i.normalized_storage(),
                None => {
                    return form_js_error(err_tgt.as_ref(), "bad item", "Invalid right item path.")
                        .into_response();
                }
            };
            let pool_id = pool.as_deref().and_then(|p| {
                crate::path_types::ItemId::parse(p.trim()).map(|i| i.normalized_storage())
            });
            let rl = ratio_left.trim().parse::<i32>().unwrap_or(0).max(0);
            let rr = ratio_right.trim().parse::<i32>().unwrap_or(0).max(0);
            if rl == 0 || rr == 0 {
                return form_js_error(
                    err_tgt.as_ref(),
                    "invalid ratio",
                    "Both ratio sides must be ≥ 1.",
                )
                .into_response();
            }
            if rl > 100 || rr > 100 {
                return form_js_error(
                    err_tgt.as_ref(),
                    "invalid ratio",
                    "Ratio sides must be ≤ 100.",
                )
                .into_response();
            }
            let (rl, rr) = crate::dsl::reduce_ratio(rl, rr);

            let text = format!(
                "{{\n{}\n}}\n{} {}:{} {}\n",
                exp,
                left_id.as_str(),
                rl,
                rr,
                right_id.as_str()
            );

            match rpc_post_with_bearer(
                state,
                &session.bearer,
                room.clone(),
                thread_tag.clone(),
                None,
                text,
            )
            .await
            {
                Ok(RpcResult::PostOk {
                    post_id,
                    post_index,
                    ..
                }) => {
                    let Some(pid) = post_id else {
                        let loc = sanitize_vote_compare_next(&next);
                        return js_redirect(&loc).into_response();
                    };
                    let nav = if room == "public" {
                        ThreadNav::public()
                    } else {
                        let Some(n) = ThreadNav::from_room_id(&room) else {
                            return form_js_error(
                                err_tgt.as_ref(),
                                "bad room",
                                "Unknown room for vote post.",
                            )
                            .into_response();
                        };
                        n
                    };
                    let js = vote_compare_post_success_js(
                        state,
                        &nav,
                        &room,
                        &thread_tag,
                        &left_id,
                        &right_id,
                        pool_id.as_ref(),
                        pid.as_str(),
                        post_index,
                    )
                    .await;
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
                        .body(axum::body::Body::from(js))
                        .unwrap()
                }
                Ok(_) => form_js_error(
                    err_tgt.as_ref(),
                    "unexpected response",
                    "Post did not return PostOk.",
                )
                .into_response(),
                Err((msg, hint)) => {
                    form_js_error(err_tgt.as_ref(), &msg, hint.as_deref().unwrap_or(""))
                        .into_response()
                }
            }
        }
        HtmlUiAction::SetGardenPin {
            clear,
            room_wire,
            item_storage,
            next,
            form_action,
        } => {
            if form_action != "/ui" {
                return (
                    StatusCode::BAD_REQUEST,
                    "invalid set_garden_pin form_action",
                )
                    .into_response();
            }
            let next_path = sanitize_garden_pin_next(&next);
            use crate::html::{encode_pin_cookie_value, GARDEN_PIN_COOKIE};
            use crate::path_types::ItemId;
            if clear {
                let cookie = format!("{GARDEN_PIN_COOKIE}=; Path=/; SameSite=Lax; Max-Age=0");
                return redirect_with_pin_cookie(&cookie, &next_path);
            }
            let room = room_wire.trim().to_string();
            if room.is_empty() {
                return (StatusCode::BAD_REQUEST, "missing room").into_response();
            }
            let Some(raw) = item_storage
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            else {
                return (StatusCode::BAD_REQUEST, "missing item").into_response();
            };
            let Some(item) = ItemId::parse(&raw) else {
                return (StatusCode::BAD_REQUEST, "bad item").into_response();
            };
            let item = item.normalized_storage();
            let val = encode_pin_cookie_value(&room, item.as_str());
            let cookie =
                format!("{GARDEN_PIN_COOKIE}={val}; Path=/; SameSite=Lax; Max-Age=7776000");
            redirect_with_pin_cookie(&cookie, &next_path)
        }
        HtmlUiAction::ResolveExternal {
            room_wire,
            item_storage,
            mode,
            next,
            form_action,
        } => {
            if form_action != "/ui" {
                return (
                    StatusCode::BAD_REQUEST,
                    "invalid resolve_external form_action",
                )
                    .into_response();
            }
            let Some(session) = session else {
                return js_redirect(&login_redirect_for(&next)).into_response();
            };
            let room = room_wire.trim();
            if room.is_empty() {
                return ui_js_warn("missing room").into_response();
            }
            let reduced = state.reduced.read().await;
            if matches!(scope_from_room_wire(room), ScopeId::Room(_))
                && !user_can_post_room(&reduced, room, &session.username) {
                    drop(reduced);
                    return ui_js_warn("forbidden").into_response();
                }
            drop(reduced);

            let Some(item) = crate::path_types::ItemId::parse(item_storage.trim()) else {
                return ui_js_warn("bad item").into_response();
            };
            let target = if mode.trim() == "siblings" {
                match item.parent() {
                    Some(parent) => parent.normalized_storage(),
                    None => return ui_js_warn("no parent to resolve siblings").into_response(),
                }
            } else {
                item.normalized_storage()
            };
            match resolve_github_children(state, room, &target).await {
                Ok(stats) => JsBuilder::new()
                    .morph_inner_selector(
                        "#external-resolver-status",
                        external_resolver_status_markup(Ok(stats), &sanitize_garden_pin_next(&next)),
                    )
                    .redirect(&sanitize_garden_pin_next(&next))
                    .into_response(),
                Err(msg) => JsBuilder::new()
                    .morph_inner_selector(
                        "#external-resolver-status",
                        external_resolver_status_markup(Err(msg.as_str()), &next),
                    )
                    .into_response(),
            }
        }
        HtmlUiAction::RedactPost { post_id } => {
            let Some(session) = session else {
                return js_redirect("/login").into_response();
            };
            let h = headers_from_bearer(&session.bearer);
            match rpc_post_redact(state, &h, post_id).await {
                Ok(RpcResult::RedactPostOk {}) => {
                    redact_success_response(state).await.into_response()
                }
                Ok(_) => (StatusCode::BAD_REQUEST, "unexpected response").into_response(),
                Err((msg, hint)) => {
                    let detail = hint.as_deref().unwrap_or("");
                    js_error("#errors", &msg, detail).into_response()
                }
            }
        }
        HtmlUiAction::SetRoomMembersExpanded {
            room_wire,
            expanded,
        } => {
            let room_wire = room_wire.trim().to_string();
            if room_wire.is_empty() {
                return ui_js_warn("missing room").into_response();
            }
            let reduced = state.reduced.read().await;
            let user = session.map(|s| s.username.as_str());
            if !reduced.rooms.contains(&room_wire) {
                drop(reduced);
                return ui_js_warn("room not found").into_response();
            }
            if !user_can_view_room(&reduced, &room_wire, user) {
                drop(reduced);
                return ui_js_warn("forbidden").into_response();
            }
            let markup = room_members_section_markup(&reduced, &room_wire, expanded, user);
            drop(reduced);
            JsBuilder::new()
                .morph_selector("#room-members-section", markup)
                .into_response()
        }
        HtmlUiAction::DeleteRoom { room } => {
            let Some(session) = session else {
                return js_redirect("/login").into_response();
            };
            let room = room.trim().to_string();
            if room.is_empty() {
                return ui_js_warn("missing room").into_response();
            }
            let h = headers_from_bearer(&session.bearer);
            match rpc_room_delete(state, &h, room).await {
                Ok(RpcResult::RoomDeletedOk {}) => js_redirect("/").into_response(),
                Ok(_) => (StatusCode::BAD_REQUEST, "unexpected response").into_response(),
                Err((msg, hint)) => {
                    let detail = hint.as_deref().unwrap_or("");
                    js_error("#errors", &msg, detail).into_response()
                }
            }
        }
        HtmlUiAction::SetNewThreadComposeExpanded {
            room_wire,
            expanded,
        } => {
            let room_wire = room_wire.trim().to_string();
            if room_wire.is_empty() {
                return ui_js_warn("missing room").into_response();
            }
            if room_wire == "public" {
                let reduced = state.reduced.read().await;
                let _user = session.map(|s| s.username.as_str());
                drop(reduced);
                let can_post = session.is_some();
                let nav = ThreadNav::public();
                let markup = if can_post {
                    fragment_new_thread_slot(&nav, true, expanded)
                } else {
                    login_to_post_hint_markup()
                };
                let mut b = JsBuilder::new().morph_inner_selector("#new-thread-ui-slot", markup);
                if expanded && can_post {
                    b = b.focus_selector("#new-thread-tag");
                }
                return b.into_response();
            }
            let reduced = state.reduced.read().await;
            let user = session.map(|s| s.username.as_str());
            if !reduced.rooms.contains(&room_wire) {
                drop(reduced);
                return ui_js_warn("room not found").into_response();
            }
            if !user_can_view_room(&reduced, &room_wire, user) {
                drop(reduced);
                return ui_js_warn("forbidden").into_response();
            }
            let can_post = session
                .as_ref()
                .map(|s| user_can_post_room(&reduced, &room_wire, &s.username))
                .unwrap_or(false);
            drop(reduced);
            let Some(nav) = ThreadNav::from_room_id(&room_wire) else {
                return ui_js_warn("bad room").into_response();
            };
            let markup = if can_post {
                fragment_new_thread_slot(&nav, true, expanded)
            } else {
                login_to_post_hint_markup()
            };
            let mut b = JsBuilder::new().morph_inner_selector("#new-thread-ui-slot", markup);
            if expanded && can_post {
                b = b.focus_selector("#new-thread-tag");
            }
            b.into_response()
        }
        HtmlUiAction::ExpandPostFull {
            room,
            thread_tag,
            post_index,
        } => {
            let viewer = session.map(|s| s.username.as_str());
            thread_ui_expand_post_full(state, &room, &thread_tag, post_index, viewer).await
        }
        HtmlUiAction::ExpandRedactedPost {
            room,
            thread_tag,
            post_index,
        } => {
            let viewer = session.map(|s| s.username.as_str());
            thread_ui_expand_redacted_post(state, &room, &thread_tag, post_index, viewer).await
        }
        HtmlUiAction::CollapseRedactedPost {
            room,
            thread_tag,
            post_index,
        } => {
            let viewer = session.map(|s| s.username.as_str());
            thread_ui_collapse_redacted_post(state, &room, &thread_tag, post_index, viewer).await
        }
        HtmlUiAction::CopyThread {
            room,
            thread_tag,
            copy_btn_id,
        } => {
            let viewer = session.map(|s| s.username.as_str());
            thread_ui_copy_thread(state, &room, &thread_tag, &copy_btn_id, viewer).await
        }
        HtmlUiAction::CopyGardenRank {
            room,
            parent_path,
            depth,
            copy_btn_id,
            external_hosts,
        } => {
            let viewer = session.map(|s| s.username.as_str());
            garden_ui_copy_rank(
                state,
                &room,
                &parent_path,
                depth,
                &copy_btn_id,
                external_hosts,
                viewer,
            )
            .await
        }
        HtmlUiAction::GraduateThread { room, thread_tag } => {
            let Some(session) = session else {
                return js_redirect("/login").into_response();
            };
            let room = room.trim().to_string();
            let thread_tag = canonicalize_tag(&thread_tag);
            if room.is_empty() || room == "public" {
                return ui_js_warn("graduation requires a private room").into_response();
            }
            let h = headers_from_bearer(&session.bearer);
            match rpc_thread_graduate(state, &h, room, thread_tag.clone()).await {
                Ok(RpcResult::ThreadGraduatedOk { web, .. }) => js_redirect(&web).into_response(),
                Ok(_) => (StatusCode::BAD_REQUEST, "unexpected response").into_response(),
                Err((msg, hint)) => {
                    let detail = hint.as_deref().unwrap_or("");
                    js_error("#errors", &msg, detail).into_response()
                }
            }
        }
    }
}

fn headers_from_bearer(bearer: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(hv) = HeaderValue::from_str(&format!("Bearer {bearer}")) {
        headers.insert(header::AUTHORIZATION, hv);
    }
    headers
}

fn post_redirect_location(room: &str, thread_tag: &str) -> String {
    let tag = canonicalize_tag(thread_tag);
    if room.trim() == "public" {
        format!("/t/{tag}")
    } else {
        let room = room.trim();
        let Some(seg) = slug_types::room_route_segment(room) else {
            return "/".to_string();
        };
        format!("/r/{seg}/t/{tag}")
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

fn form_error_target(error_target: Option<&String>) -> String {
    error_target
        .map(|s| s.as_str())
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

fn form_js_error(error_target: Option<&String>, title: &str, detail: &str) -> Response {
    js_error(&form_error_target(error_target), title, detail)
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
        return Err((format!("rpc check http {status}"), None));
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

async fn post_success_response(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    error_target: Option<&String>,
    form_id: Option<&String>,
    viewer: Option<&str>,
) -> Response {
    let error_target = form_error_target(error_target);
    let room = room.trim().to_string();
    let thread_tag = canonicalize_tag(thread_tag);
    let thread_location = post_redirect_location(&room, &thread_tag);
    let form_id = form_id.map(|s| s.as_str()).unwrap_or("");
    let scope = scope_from_room_wire(&room);
    let feed_markup = match &scope {
        ScopeId::Public => thread_feed_html(state).await,
        ScopeId::Room(_) => thread_feed_html_for_room(state, &room).await,
    };
    let (latest_offset, thread_markup) = thread_latest_page_region(
        state,
        match &scope {
            ScopeId::Public => None,
            ScopeId::Room(_) => Some(room.as_str()),
        },
        &thread_tag,
        viewer,
    )
    .await;
    let feed_selector = match &scope {
        ScopeId::Public => "#thread-feed",
        ScopeId::Room(_) => "#room-thread-feed",
    };
    let latest_page_location = if latest_offset == 0 {
        thread_location.clone()
    } else {
        format!("{thread_location}?offset={latest_offset}")
    };

    let builder = JsBuilder::new()
        .morph_selector(&error_target, empty_error_markup(&error_target))
        .morph_selector(feed_selector, feed_markup)
        .if_current_path_matches(&thread_location, |builder| {
            // Poster is viewing the latest page: append in place (page-aligned
            // windows keep existing posts where they are).
            let builder = builder.if_page_offset_at_least(latest_offset, |b| {
                let b = b.morph_selector("#thread-feed-region", thread_markup);
                if !form_id.trim().is_empty() {
                    b.qs(&format!("#{form_id}")).reset()
                } else {
                    b
                }
            });
            // Poster is on an older page: jump to the latest page so they see
            // their new post instead of silently rewriting the page they read.
            builder.if_page_offset_below(latest_offset, |b| b.redirect(&latest_page_location))
        })
        .if_current_path_not_matches(&thread_location, |builder| {
            builder.redirect(&thread_location)
        });

    builder.into_response()
}

async fn redact_success_response(state: &AppState) -> Response {
    let feed_markup = thread_feed_html(state).await;
    JsBuilder::new()
        .morph_selector("#thread-feed", feed_markup)
        .into_response()
}
