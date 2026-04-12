use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};
use serde::Deserialize;
use serde_json::json;

use crate::api::optional_principal;
use crate::canonical_path::canonicalize_tag;
use crate::form_template::template_json_compact;
use crate::reducer::ScopeId;
use crate::state::AppState;

use super::access::user_can_post_room;
use super::access::user_can_view_room;
use super::feed::{collect_thread_rows_for_scope, render_thread_feed};
use super::ingest::ingest_entry_markup;
use super::nav::ThreadNav;
use super::new_thread::fragment_room_new_thread_form;
use super::page::{auth_strip, bc_room};
use super::paginator::{render_thread_paginator, PAGE_SIZE};
use super::room_members::room_members_section_markup;
use crate::html::ui_action::UI_RPC_FIELD;
use crate::html::{
    bc_threads, cli_panel, layout, now_ms, theme_from_jar, theme_next_from_uri,
};

fn compose_form(nav: &ThreadNav, thread_tag: &str, show: bool) -> Markup {
    if !show {
        return html! {};
    }
    html! {
        section class="compose" id="thread-compose" {
            form id="thread-compose-form" method="POST" action="/ui" data-check-action="/ui" data-check-rpc=(template_json_compact(&json!({
                "action": "check_ingest",
                "room": nav.room_wire,
                "thread_tag": thread_tag,
                "text": {"$form": "text"},
                "error_target": "thread-compose-errors",
                "form_id": "thread-compose-form",
            })).unwrap()) {
                input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&json!({
                    "action": "post_ingest",
                    "room": nav.room_wire,
                    "thread_tag": thread_tag,
                    "text": {"$form": "text"},
                    "error_target": "thread-compose-errors",
                    "form_id": "thread-compose-form",
                })).unwrap());
                textarea name="text" rows="5" cols="80" placeholder="prose or ~/items and votes…" {}
                p {
                    button type="submit" { "post" }
                }
            }
            div id="thread-compose-errors" {}
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ThreadViewQuery {
    pub offset: Option<usize>,
}

async fn thread_view_inner(
    state: AppState,
    tag: String,
    q: ThreadViewQuery,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let scope = nav.scope();

    let all_ids: Vec<String> = {
        let reduced = state.reduced.read().await;
        reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()))
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    };

    let total = all_ids.len();
    let offset = q.offset.unwrap_or(0);
    let page_ids: Vec<String> = all_ids.into_iter().skip(offset).take(PAGE_SIZE).collect();

    let (display_ingests, _subtitle) = {
        let reduced = state.reduced.read().await;
        let ingests = page_ids
            .iter()
            .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
            .collect::<Vec<_>>();
        (ingests, None::<String>)
    };

    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    let sc = nav.scope();
    let show_compose = match &sc {
        ScopeId::Public => user.is_some(),
        ScopeId::Room(rid) => user
            .as_ref()
            .map(|u| user_can_post_room(&reduced, rid, u))
            .unwrap_or(false),
    };
    let strip = auth_strip(&headers, &jar, &reduced);
    let now = now_ms();
    let entry_rows: Vec<Markup> = display_ingests
        .iter()
        .enumerate()
        .map(|(i, ing)| {
            let post_idx = offset + i;
            ingest_entry_markup(&nav, &tag, post_idx, ing, user.as_deref(), now, &reduced)
        })
        .collect();
    drop(reduced);

    let paginator_top = render_thread_paginator(&nav, &tag, offset, total, true);
    let paginator_bot = render_thread_paginator(&nav, &tag, offset, total, false);

    let bc: Markup = match &sc {
        ScopeId::Public => bc_threads(Some(&tag)),
        ScopeId::Room(rid) => {
            let slug = if let Some((_, s)) = rid.split_once('/') {
                s
            } else {
                rid.as_str()
            };
            bc_room(&nav, slug, Some(&tag))
        }
    };

    let cli = match &sc {
        ScopeId::Public => format!("npx slugsocial public forum show {tag}"),
        ScopeId::Room(r) => format!("npx slugsocial private {r} forum show {tag}"),
    };

    let page = layout(
        &format!("#{tag}"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc) }
            p class="muted" { "top=oldest · bottom=newest" }
            div id="thread-feed-region" {
                @if display_ingests.is_empty() {
                    p class="muted" { "no activity yet" }
                } @else {
                    (paginator_top)
                    @for row in &entry_rows {
                        (row)
                    }
                    (paginator_bot)
                }
            }
            div id="thread-live-region" {
                (compose_form(&nav, &tag, show_compose))
            }
            (cli_panel(std::slice::from_ref(&cli)))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}

/// Thread view — `/t/:tag`
pub async fn thread_view(
    State(state): State<AppState>,
    Path(tag): Path<String>,
    Query(q): Query<ThreadViewQuery>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    thread_view_inner(state, tag, q, ThreadNav::public(), headers, jar, uri).await
}

/// Room thread — `/r/:short/:slug/t/:tag`
pub async fn room_thread_view(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag)): Path<(String, String, String)>,
    Query(q): Query<ThreadViewQuery>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let room_id = format!("{room_short}/{room_slug}");
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    drop(reduced);
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    thread_view_inner(state, tag, q, nav, headers, jar, uri)
        .await
        .into_response()
}

pub(super) fn room_not_found_page(jar: &CookieJar, uri: &Uri) -> impl IntoResponse {
    let body = html! {
        nav class="breadcrumb" { a href="/" { "slug.social" } }
        h1 { "not found" }
        p { "The requested page could not be found." }
        p { a href="/" { "home" } }
    };
    let page = layout(
        "not found — slug.social",
        "view-thread",
        body,
        None,
        theme_from_jar(jar),
        &theme_next_from_uri(uri),
    );
    (StatusCode::NOT_FOUND, Html(page.into_string()))
}

/// Private room index — `/r/:short/:slug`
pub async fn room_page(
    State(state): State<AppState>,
    Path((room_short, room_slug)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let room_id = format!("{room_short}/{room_slug}");
    let now = now_ms();
    let reduced = state.reduced.read().await;
    if !reduced.rooms.contains(&room_id) {
        drop(reduced);
        return (StatusCode::NOT_FOUND, "room not found").into_response();
    }
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    let scope = ScopeId::Room(room_id.clone());
    let mut rows = collect_thread_rows_for_scope(&reduced, &scope, now);
    let strip = auth_strip(&headers, &jar, &reduced);
    let show_new = user
        .as_ref()
        .map(|u| user_can_post_room(&reduced, &room_id, u))
        .unwrap_or(false);
    rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));

    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        drop(reduced);
        return (StatusCode::NOT_FOUND, "room not found").into_response();
    };
    let members_markup = room_members_section_markup(&reduced, &room_id, false);
    let forum_cli = format!("npx slugsocial private {room_id} forum list");
    let garden_cli = format!("npx slugsocial private {room_id} garden tree");
    let audit_cli = format!("npx slugsocial private {room_id} audit");
    drop(reduced);

    let slug_display = room_slug.as_str();
    let page = layout(
        &format!("room {slug_display} — slug.social"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc_room(&nav, slug_display, None)) }
            (members_markup)
            h3 { "threads" }
            (render_thread_feed(Some(&nav), "room-thread-feed", &rows, now))
            @if show_new {
                div id="room-new-thread-ui-slot" {
                    (fragment_room_new_thread_form(&nav, true, false))
                }
            }
            (cli_panel(&[forum_cli, garden_cli, audit_cli]))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}
