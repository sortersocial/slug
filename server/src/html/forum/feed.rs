use axum::{
    extract::State,
    http::{HeaderMap, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};

use crate::api::optional_principal;
use crate::canonical_path::canonicalize_tag;
use crate::reducer::{ReducerState, ScopeId};
use crate::state::AppState;
use crate::timeago;

use super::ingest::ingest_entry_markup;
use super::nav::ThreadNav;
use super::new_thread::{fragment_new_thread_slot, login_to_post_hint_markup};
use super::page::auth_strip;
use super::paginator::{render_thread_paginator, PAGE_SIZE};
use crate::html::{
    bc_threads, cli_panel, layout, now_ms, recency_class, theme_from_jar, theme_next_from_uri,
};

#[derive(Clone)]
pub(super) struct ThreadRow {
    pub(super) tag: String,
    pub(super) subtitle: Option<String>,
    pub(super) last_ts: i64,
    pub(super) ingests: usize,
}

pub(super) fn collect_thread_rows_for_scope(reduced: &ReducerState, scope: &ScopeId, now: i64) -> Vec<ThreadRow> {
    let _ = now;
    reduced
        .forum_threads
        .iter()
        .filter(|((s, _), _)| s == scope)
        .map(|((_, tag), thread)| {
            let ingests = reduced
                .ingests_by_scope_thread
                .get(&(scope.clone(), tag.clone()))
                .map(|q| q.len())
                .unwrap_or(0);
            ThreadRow {
                tag: tag.clone(),
                subtitle: None,
                last_ts: thread.last_activity_ts,
                ingests,
            }
        })
        .collect()
}

pub(super) fn rooms_for_user(reduced: &ReducerState, username: &str) -> Vec<String> {
    let mut v: Vec<String> = reduced
        .grants
        .iter()
        .filter(|(rid, m)| reduced.rooms.contains(*rid) && m.contains_key(username))
        .map(|(rid, _)| rid.clone())
        .collect();
    v.sort();
    v
}

/// `feed_id` is e.g. `thread-feed` (public bump list, SSE) or `room-thread-feed`.
pub(super) fn render_thread_feed(nav: Option<&ThreadNav>, feed_id: &str, rows: &[ThreadRow], now: i64) -> Markup {
    html! {
        div id=(feed_id) {
            @if rows.is_empty() {
                p class="muted" { "no threads yet" }
            } @else {
                ul class="thread-feed" {
                    @for r in rows {
                        @let thread_href = nav
                            .as_ref()
                            .map(|n| n.thread_url(&r.tag))
                            .unwrap_or_else(|| format!("/t/{}", r.tag));
                        @let hover = timeago::rfc3339_utc(r.last_ts);
                        @let ago = timeago::timeago(now, r.last_ts);
                        @let age_cls = recency_class(now, r.last_ts);
                        li class=(age_cls) {
                            a href=(thread_href) {
                                "#" (r.tag)
                                @if let Some(subtitle) = &r.subtitle {
                                    ": " (subtitle)
                                }
                                " "
                                span class="muted" title=(hover) {
                                    (ago)
                                    " · "
                                    (format!("{}n", r.ingests))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Returns the current public thread feed markup for SSE (`#thread-feed`).
pub async fn thread_feed_html(state: &AppState) -> Markup {
    let now = now_ms();
    let nav = ThreadNav::public();
    let mut rows = {
        let reduced = state.reduced.read().await;
        collect_thread_rows_for_scope(&reduced, &ScopeId::Public, now)
    };
    rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    render_thread_feed(Some(&nav), "thread-feed", &rows, now)
}

/// Returns the current private room thread feed markup for SSE (`#room-thread-feed`).
pub async fn thread_feed_html_for_room(state: &AppState, room_id: &str) -> Markup {
    let now = now_ms();
    let Some(nav) = ThreadNav::from_room_id(room_id) else {
        return html! { div id="room-thread-feed" { p class="muted" { "room not found" } } };
    };
    let mut rows = {
        let reduced = state.reduced.read().await;
        collect_thread_rows_for_scope(&reduced, &ScopeId::Room(room_id.to_string()), now)
    };
    rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    render_thread_feed(Some(&nav), "room-thread-feed", &rows, now)
}

fn render_thread_feed_region_markup(
    nav: &ThreadNav,
    tag: &str,
    display_ingests: &[crate::events::Ingest],
    offset: usize,
    total: usize,
    now: i64,
    viewer: Option<&str>,
    reduced: &ReducerState,
) -> Markup {
    let paginator_top = render_thread_paginator(nav, tag, offset, total, true);
    let paginator_bot = render_thread_paginator(nav, tag, offset, total, false);
    html! {
        div id="thread-feed-region" {
            @if display_ingests.is_empty() {
                p class="muted" { "no activity yet" }
            } @else {
                (paginator_top)
                @for (i, ing) in display_ingests.iter().enumerate() {
                    @let post_idx = offset + i;
                    (ingest_entry_markup(nav, tag, post_idx, ing, viewer, now, reduced))
                }
                (paginator_bot)
            }
        }
    }
}

pub async fn thread_feed_region_markup(
    state: &AppState,
    room_id: Option<&str>,
    tag: &str,
    viewer: Option<&str>,
) -> Markup {
    let tag = canonicalize_tag(tag);
    let Some(nav) = (match room_id {
        Some(room_id) => ThreadNav::from_room_id(room_id),
        None => Some(ThreadNav::public()),
    }) else {
        return html! { div id="thread-feed-region" { p class="muted" { "thread not found" } } };
    };
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
    let offset = total.saturating_sub(PAGE_SIZE);
    let page_ids: Vec<String> = all_ids.into_iter().skip(offset).take(PAGE_SIZE).collect();
    let reduced = state.reduced.read().await;
    let display_ingests = page_ids
        .iter()
        .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
        .collect::<Vec<_>>();
    render_thread_feed_region_markup(
        &nav,
        &tag,
        &display_ingests,
        offset,
        total,
        now_ms(),
        viewer,
        &reduced,
    )
}

/// Home: private rooms (signed-in), then public bump-ordered threads.
pub async fn home(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let now = now_ms();
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    let room_ids = user
        .as_ref()
        .map(|u| rooms_for_user(&reduced, u))
        .unwrap_or_default();
    let mut public_rows = collect_thread_rows_for_scope(&reduced, &ScopeId::Public, now);
    drop(reduced);
    public_rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));

    let nav = ThreadNav::public();
    let reduced_read = state.reduced.read().await;
    let strip = auth_strip(&headers, &jar, &reduced_read);
    drop(reduced_read);

    let page = layout(
        "slug.social",
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc_threads(None, None)) }
            @if !room_ids.is_empty() {
                h2 { "your rooms" }
                ul class="thread-feed" {
                    @for rid in &room_ids {
                        @if let Some(nav_r) = ThreadNav::from_room_id(rid) {
                            @let slug = if let Some((_, s)) = rid.split_once('/') { s } else { rid.as_str() };
                            li {
                                a href=(nav_r.room_url()) {
                                    (slug)
                                    span class="muted" { " · " (rid) }
                                }
                            }
                        }
                    }
                }
            }
            p class="muted" { "dark = time-ordered · light = vote-ranked" }
            div id="new-thread-ui-slot" {
                @if user.is_some() {
                    (fragment_new_thread_slot(&nav, true, false))
                } @else {
                    (login_to_post_hint_markup())
                }
            }
            (render_thread_feed(Some(&nav), "thread-feed", &public_rows, now))
            (cli_panel(&["npx slugsocial public forum list"]))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string())
}
