use axum::{
    extract::State,
    http::{HeaderMap, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};

use crate::api::optional_principal;
use crate::canonical_path::canonicalize_tag;
use crate::middleware::canonical_view_url;
use crate::reducer::{ReducerState, ScopeId};
use crate::state::AppState;
use crate::timeago;

use super::ingest::ingest_entry_markup;
use super::nav::ThreadNav;
use super::new_thread::{fragment_new_thread_slot, login_to_post_hint_markup};
use super::page::auth_strip;
use super::paginator::{latest_page_offset, render_thread_paginator, snap_page_offset, PAGE_SIZE};
use crate::html::{
    bc_threads, cli_panel, layout_with_post_stats, now_ms, recency_class, recency_color_style,
    theme_from_jar, theme_next_from_uri,
};

#[derive(Clone)]
pub(super) struct ThreadRow {
    pub(super) tag: String,
    pub(super) subtitle: Option<String>,
    pub(super) last_ts: i64,
    pub(super) last_actor: String,
    pub(super) ingests: usize,
}

#[derive(Clone)]
pub(super) struct RoomRow {
    pub(super) id: String,
    /// Most recent forum post in the room, or room-created time if empty.
    pub(super) last_ts: i64,
}

pub(super) fn collect_thread_rows_for_scope(
    reduced: &ReducerState,
    scope: &ScopeId,
    now: i64,
) -> Vec<ThreadRow> {
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
                last_actor: thread.last_actor.clone(),
                ingests,
            }
        })
        .collect()
}

/// Rooms the user can access, newest activity first (tie-break: room id).
pub(super) fn rooms_for_user(reduced: &ReducerState, username: &str) -> Vec<RoomRow> {
    let mut v: Vec<RoomRow> = reduced
        .grants
        .iter()
        .filter(|(rid, m)| reduced.rooms.contains(*rid) && m.contains_key(username))
        .map(|(rid, _)| RoomRow {
            id: rid.clone(),
            last_ts: reduced.room_last_activity_ts(rid),
        })
        .collect();
    v.sort_by(|a, b| b.last_ts.cmp(&a.last_ts).then_with(|| a.id.cmp(&b.id)));
    v
}

/// `feed_id` is e.g. `thread-feed` (public bump list, SSE) or `room-thread-feed`.
pub(super) fn render_thread_feed(
    nav: Option<&ThreadNav>,
    feed_id: &str,
    rows: &[ThreadRow],
    now: i64,
) -> Markup {
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
                        @let ts_style = recency_color_style(now, r.last_ts);
                        li class=(age_cls) {
                            a href=(thread_href) {
                                "#" (r.tag)
                                @if let Some(subtitle) = &r.subtitle {
                                    ": " (subtitle)
                                }
                                " "
                                span title=(hover) {
                                    span class="ts-recency" style=(ts_style.as_str()) { (ago) }
                                    span class="muted" {
                                        @if !r.last_actor.is_empty() {
                                            " by @" (r.last_actor)
                                        }
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

#[allow(clippy::too_many_arguments)]
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

/// One page-aligned `#thread-feed-region` render, addressed by its start offset.
pub(crate) struct ThreadRegionPageMorph {
    pub offset: usize,
    pub markup: Markup,
}

/// Region markups for the pages a thread change can affect, first entry = latest page.
pub(crate) struct ThreadRegionPageMorphs {
    pub latest_offset: usize,
    pub pages: Vec<ThreadRegionPageMorph>,
}

fn thread_not_found_region() -> Markup {
    html! { div id="thread-feed-region" { p class="muted" { "thread not found" } } }
}

/// `room_id` is the wire form (`short/slug` or `"public"`). `broadcast_web_refresh` passes
/// `Some("public")` for the public forum; `ThreadNav::from_room_id` only accepts `short/slug`.
fn thread_nav_for_room_id(room_id: Option<&str>) -> Option<ThreadNav> {
    match room_id {
        Some("public") | None => Some(ThreadNav::public()),
        Some(room_id) => ThreadNav::from_room_id(room_id),
    }
}

/// Region markups for the pages a thread change can affect: the **latest page**
/// (where new posts append), the page just before it (its paginator gains a live
/// `newer →` link when a new page starts), and the page containing
/// `changed_post_index` (redactions of posts on older pages). Offsets are
/// page-aligned and deduplicated; older pages are never re-rendered, so viewers
/// reading them are not disturbed.
pub(crate) async fn thread_region_page_morphs(
    state: &AppState,
    room_id: Option<&str>,
    tag: &str,
    viewer: Option<&str>,
    changed_post_index: Option<usize>,
) -> ThreadRegionPageMorphs {
    let tag = canonicalize_tag(tag);
    let Some(nav) = thread_nav_for_room_id(room_id) else {
        return ThreadRegionPageMorphs {
            latest_offset: 0,
            pages: vec![ThreadRegionPageMorph {
                offset: 0,
                markup: thread_not_found_region(),
            }],
        };
    };
    let scope = nav.scope();
    let now = now_ms();

    let reduced = state.reduced.read().await;
    let all_ids: Vec<String> = reduced
        .ingests_by_scope_thread
        .get(&(scope.clone(), tag.clone()))
        .map(|q| q.iter().rev().cloned().collect())
        .unwrap_or_default();
    let total = all_ids.len();
    let latest_offset = latest_page_offset(total);

    let mut offsets: Vec<usize> = vec![latest_offset];
    if latest_offset >= PAGE_SIZE {
        offsets.push(latest_offset - PAGE_SIZE);
    }
    if let Some(idx) = changed_post_index {
        let page = snap_page_offset(idx, total);
        if !offsets.contains(&page) {
            offsets.push(page);
        }
    }

    let pages = offsets
        .into_iter()
        .map(|offset| {
            let display_ingests: Vec<crate::events::Ingest> = all_ids
                .iter()
                .skip(offset)
                .take(PAGE_SIZE)
                .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
                .collect();
            ThreadRegionPageMorph {
                offset,
                markup: render_thread_feed_region_markup(
                    &nav,
                    &tag,
                    &display_ingests,
                    offset,
                    total,
                    now,
                    viewer,
                    &reduced,
                ),
            }
        })
        .collect();

    ThreadRegionPageMorphs {
        latest_offset,
        pages,
    }
}

/// Latest-page region markup only — for the poster's own `POST /ui` response.
pub(crate) async fn thread_latest_page_region(
    state: &AppState,
    room_id: Option<&str>,
    tag: &str,
    viewer: Option<&str>,
) -> (usize, Markup) {
    let morphs = thread_region_page_morphs(state, room_id, tag, viewer, None).await;
    let latest_offset = morphs.latest_offset;
    let markup = morphs
        .pages
        .into_iter()
        .next()
        .map(|p| p.markup)
        .unwrap_or_else(thread_not_found_region);
    (latest_offset, markup)
}

/// Retired `/t` index path → `/`: the public thread index lives at the site root.
pub async fn redirect_forum_index(uri: Uri) -> impl IntoResponse {
    match uri.query() {
        Some(q) if !q.is_empty() => axum::response::Redirect::permanent(&format!("/?{q}")),
        _ => axum::response::Redirect::permanent("/"),
    }
}

/// Public thread index (`GET /`): private rooms (signed-in), then public bump-ordered threads.
pub async fn thread_index(
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
    let post_stats = reduced.public_post_stats();
    drop(reduced);
    public_rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));

    let nav = ThreadNav::public();
    let reduced_read = state.reduced.read().await;
    let strip = auth_strip(&headers, &jar, &reduced_read);
    drop(reduced_read);

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout_with_post_stats(
        "slug.social",
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc_threads(None, None)) }
            @if !room_ids.is_empty() {
                h2 { "your rooms" }
                ul class="thread-feed" {
                    @for room in &room_ids {
                        @if let Some(nav_r) = ThreadNav::from_room_id(&room.id) {
                            @let slug = if let Some((_, s)) = room.id.split_once('/') { s } else { room.id.as_str() };
                            @let hover = timeago::rfc3339_utc(room.last_ts);
                            @let ago = timeago::timeago(now, room.last_ts);
                            @let age_cls = recency_class(now, room.last_ts);
                            @let ts_style = recency_color_style(now, room.last_ts);
                            li class=(age_cls) {
                                a href=(nav_r.room_url()) {
                                    (slug)
                                    " "
                                    span title=(hover) {
                                        span class="ts-recency" style=(ts_style.as_str()) { (ago) }
                                        span class="muted" { " · " (room.id) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            p class="muted" { "dark = time-ordered · light = vote-ranked" }
            p class="muted home-judge-cta" {
                "new here? "
                a href="/vote" { "judge one pair" }
                " — rankings come from comparisons."
            }
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
        Some(view_count),
        post_stats,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string())
}
