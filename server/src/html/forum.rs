use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};
use serde::Deserialize;

use crate::{
    api::optional_principal,
    canonical_path::canonicalize_tag,
    events::ThreadCapability,
    reducer::{ReducerState, ScopeId},
    state::AppState,
    timeago,
};

use super::{
    authorship_address, bc_segment, bc_threads, cli_panel, layout, now_ms, recency_class,
    render_linkified_with_embeds, JsBuilder,
};

#[derive(Clone)]
struct ThreadRow {
    tag: String,
    subtitle: Option<String>,
    last_ts: i64,
    ingests: usize,
}

/// URL helpers for public `/t/…` and private room threads `/r/{short}/{slug}/t/…`.
#[derive(Clone)]
pub struct ThreadNav {
    pub room_wire: String,
    scope: ScopeId,
    room_path: String,
    thread_path_prefix: String,
}

impl ThreadNav {
    pub fn public() -> Self {
        Self {
            room_wire: "public".into(),
            scope: ScopeId::Public,
            room_path: "/t".into(),
            thread_path_prefix: "/t".into(),
        }
    }

    /// `room_id` wire form `shortid/slug`.
    pub fn from_room_id(room_id: &str) -> Option<Self> {
        let (short, slug) = room_id.split_once('/')?;
        if short.is_empty() || slug.is_empty() {
            return None;
        }
        Some(Self {
            room_wire: room_id.to_string(),
            scope: ScopeId::Room(room_id.to_string()),
            room_path: format!("/r/{short}/{slug}"),
            thread_path_prefix: format!("/r/{short}/{slug}/t"),
        })
    }

    fn scope(&self) -> ScopeId {
        self.scope.clone()
    }

    fn room_url(&self) -> &str {
        &self.room_path
    }

    fn thread_url(&self, tag: &str) -> String {
        format!("{}/{}", self.thread_path_prefix, tag)
    }

    fn thread_page_url(&self, tag: &str, offset: usize) -> String {
        let base = self.thread_url(tag);
        if offset == 0 {
            base
        } else {
            format!("{base}?offset={offset}")
        }
    }

    fn post_url(&self, tag: &str, idx: usize) -> String {
        format!("{}/{}/{}", self.thread_path_prefix, tag, idx)
    }

    fn expand_url(&self, tag: &str, idx: usize) -> String {
        format!("{}/{}/{}/expand", self.thread_path_prefix, tag, idx)
    }
}

fn collect_thread_rows_for_scope(reduced: &ReducerState, scope: &ScopeId, now: i64) -> Vec<ThreadRow> {
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

fn rooms_for_user(reduced: &ReducerState, username: &str) -> Vec<String> {
    let mut v: Vec<String> = reduced
        .grants
        .iter()
        .filter(|(rid, m)| reduced.rooms.contains(*rid) && m.contains_key(username))
        .map(|(rid, _)| rid.clone())
        .collect();
    v.sort();
    v
}

fn user_can_view_room(reduced: &ReducerState, room_id: &str, username: Option<&str>) -> bool {
    if !reduced.rooms.contains(room_id) {
        return false;
    }
    let Some(u) = username else {
        return false;
    };
    reduced.user_has_cap(room_id, u, ThreadCapability::View)
}

fn user_can_post_room(reduced: &ReducerState, room_id: &str, username: &str) -> bool {
    reduced.user_has_cap(room_id, username, ThreadCapability::Post)
}

/// `feed_id` is e.g. `thread-feed` (public bump list, SSE) or `room-thread-feed`.
fn render_thread_feed(nav: Option<&ThreadNav>, feed_id: &str, rows: &[ThreadRow], now: i64) -> Markup {
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

fn auth_strip(
    headers: &HeaderMap,
    jar: &CookieJar,
    reduced: &ReducerState,
) -> Markup {
    match optional_principal(headers, jar, reduced) {
        Some(u) => html! {
            p class="muted auth-strip" {
                "@" (u)
                " · "
                a href="/logout" { "log out" }
            }
        },
        None => html! {
            p class="muted auth-strip" {
                a href="/login" { "log in" }
            }
        },
    }
}

fn bc_room(nav: &ThreadNav, room_slug: &str, thread_tag: Option<&str>) -> Markup {
    html! {
        a href="/" { "slug.social" }
        @if let Some(t) = thread_tag {
            (bc_segment(
                &format!("r / {room_slug}"),
                nav.room_url(),
                false,
            ))
            (bc_segment(&format!("#{t}"), &nav.thread_url(t), true))
        } @else {
            (bc_segment(
                &format!("r / {room_slug}"),
                nav.room_url(),
                true,
            ))
        }
    }
}

fn compose_form(nav: &ThreadNav, thread_tag: &str, show: bool) -> Markup {
    if !show {
        return html! {};
    }
    html! {
        section class="compose" id="thread-compose" {
            h3 { "reply" }
            p class="muted" { "Uses the same ingest DSL as the CLI. You must be logged in." }
            div id="errors" {}
            form method="POST" action="/post" {
                input type="hidden" name="room" value=(nav.room_wire.clone());
                input type="hidden" name="thread_tag" value=(thread_tag);
                textarea name="text" rows="8" cols="80" placeholder="prose or ~/items and votes…" {}
                p {
                    button type="submit" { "post" }
                }
            }
        }
    }
}

fn new_thread_form_public(show: bool) -> Markup {
    if !show {
        return html! {};
    }
    html! {
        section class="compose" id="public-new-thread-compose" {
            h3 { "new public thread" }
            p class="muted" { "Set thread tag and body. Example: start with a title line or use the CLI-shaped DSL." }
            div id="errors" {}
            form method="POST" action="/post" {
                input type="hidden" name="room" value="public";
                label for="new-thread-tag" { "thread tag" }
                input type="text" id="new-thread-tag" name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" placeholder="my-topic";
                label for="new-thread-text" { "text" }
                textarea id="new-thread-text" name="text" rows="6" placeholder="#my-topic\n\nYour first post…" {}
                p { button type="submit" { "create / post" } }
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
                    @let post_href = nav.post_url(tag, post_idx);
                    @let hover = timeago::rfc3339_utc(ing.ts);
                    @let ago = timeago::timeago(now, ing.ts);
                    @let truncated = ing.raw.len() > 2000;
                    @let display_body = if truncated { &ing.raw[..2000] } else { &ing.raw[..] };
                    div class="ingest-entry" data-ingest-id=(ing.id) {
                        div class="ingest-meta muted" title=(hover) {
                            a href=(post_href) class="post-num" { "#" (post_idx) }
                            " · "
                            (ago)
                        }
                        (render_linkified_with_embeds(display_body))
                        @if truncated {
                            @let exp = nav.expand_url(tag, post_idx);
                            a href="#" class="show-full-link"
                              onclick=(format!("fetch('{exp}').then(r=>r.text()).then(eval);return false")) {
                                "[show full post]"
                            }
                        }
                    }
                }
                (paginator_bot)
            }
        }
    }
}

pub async fn thread_feed_region_markup(state: &AppState, room_id: Option<&str>, tag: &str) -> Markup {
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
    let display_ingests = {
        let reduced = state.reduced.read().await;
        page_ids
            .iter()
            .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
            .collect::<Vec<_>>()
    };
    render_thread_feed_region_markup(&nav, &tag, &display_ingests, offset, total, now_ms())
}

/// Home: private rooms (signed-in), then public bump-ordered threads.
pub async fn home(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
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
    let show_forms = user.is_some();
    drop(reduced_read);

    let page = layout(
        "slug.social",
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc_threads(None)) }
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
            h2 { "public threads" }
            p class="muted" { "dark = time-ordered · light = vote-ranked" }
            (render_thread_feed(Some(&nav), "thread-feed", &public_rows, now))
            (new_thread_form_public(show_forms))
            (cli_panel("npx slugsocial forum"))
        },
        None,
    );
    Html(page.into_string())
}

#[derive(Debug, Deserialize)]
pub struct ThreadViewQuery {
    pub offset: Option<usize>,
}

const PAGE_SIZE: usize = 10;

fn render_thread_paginator(nav: &ThreadNav, tag: &str, offset: usize, total: usize, top: bool) -> Markup {
    let newer_offset = offset.checked_add(PAGE_SIZE).filter(|&o| o < total);
    let older_offset = if offset > 0 {
        Some(offset.saturating_sub(PAGE_SIZE))
    } else {
        None
    };
    let latest_offset = total.saturating_sub(PAGE_SIZE);
    let on_latest = offset >= latest_offset;
    let (id, scroll_href, scroll_label) = if top {
        ("top", "#bottom", "↓")
    } else {
        ("bottom", "#top", "↑")
    };
    html! {
        div class="thread-paginator" id=(id) {
            a href=(scroll_href) class="post-nav-btn" { (scroll_label) }
            @if let Some(o) = older_offset {
                a href=(nav.thread_page_url(tag, o)) class="post-nav-btn" { "← older" }
            } @else {
                span class="post-nav-btn disabled" { "← older" }
            }
            span class="post-nav-pos muted" {
                (offset + 1) "–" (total.min(offset + PAGE_SIZE)) " / " (total)
            }
            @if let Some(o) = newer_offset {
                a href=(nav.thread_page_url(tag, o)) class="post-nav-btn" { "newer →" }
            } @else {
                span class="post-nav-btn disabled" { "newer →" }
            }
            @if !on_latest {
                a href=(nav.thread_page_url(tag, latest_offset)) class="post-nav-btn" { "latest" }
            }
        }
    }
}

async fn thread_view_inner(
    state: AppState,
    tag: String,
    q: ThreadViewQuery,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
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

    let (display_ingests, subtitle) = {
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
    drop(reduced);

    let now = now_ms();
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
        ScopeId::Public => format!("npx slugsocial forum {tag}"),
        ScopeId::Room(r) => format!("npx slugsocial private {r} forum {tag}"),
    };

    let page = layout(
        &format!("#{tag}"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc) }
            h2 { "#" (tag) @if let Some(sub) = &subtitle { ": " (sub) } }
            p class="muted" { "top=oldest · bottom=newest" }
            div id="thread-feed-region" {
                @if display_ingests.is_empty() {
                    p class="muted" { "no activity yet" }
                } @else {
                    (paginator_top)
                    @for (i, ing) in display_ingests.iter().enumerate() {
                        @let post_idx = offset + i;
                        @let post_href = nav.post_url(&tag, post_idx);
                        @let hover = timeago::rfc3339_utc(ing.ts);
                        @let ago = timeago::timeago(now, ing.ts);
                        @let truncated = ing.raw.len() > 2000;
                        @let display_body = if truncated { &ing.raw[..2000] } else { &ing.raw[..] };
                        div class="ingest-entry" data-ingest-id=(ing.id) {
                            div class="ingest-meta muted" title=(hover) {
                                a href=(post_href) class="post-num" { "#" (post_idx) }
                                " · "
                                (ago)
                            }
                            (render_linkified_with_embeds(display_body))
                            @if truncated {
                                @let exp = nav.expand_url(&tag, post_idx);
                                a href="#" class="show-full-link"
                                  onclick=(format!("fetch('{exp}').then(r=>r.text()).then(eval);return false")) {
                                    "[show full post]"
                                }
                            }
                        }
                    }
                    (paginator_bot)
                }
            }
            div id="thread-live-region" {
                (compose_form(&nav, &tag, show_compose))
            }
            (cli_panel(&cli))
        },
        None,
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
) -> impl IntoResponse {
    thread_view_inner(state, tag, q, ThreadNav::public(), headers, jar).await
}

/// Room thread — `/r/:short/:slug/t/:tag`
pub async fn room_thread_view(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag)): Path<(String, String, String)>,
    Query(q): Query<ThreadViewQuery>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let room_id = format!("{room_short}/{room_slug}");
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page().into_response();
    }
    drop(reduced);
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    thread_view_inner(state, tag, q, nav, headers, jar)
        .await
        .into_response()
}

fn room_not_found_page() -> impl IntoResponse {
    let body = html! {
        nav class="breadcrumb" { a href="/" { "slug.social" } }
        h1 { "not found" }
        p { "The requested page could not be found." }
        p { a href="/" { "home" } }
    };
    let page = layout("not found — slug.social", "view-thread", body, None);
    (StatusCode::NOT_FOUND, Html(page.into_string()))
}

/// Private room index — `/r/:short/:slug`
pub async fn room_page(
    State(state): State<AppState>,
    Path((room_short, room_slug)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
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
        return room_not_found_page().into_response();
    }
    let scope = ScopeId::Room(room_id.clone());
    let mut rows = collect_thread_rows_for_scope(&reduced, &scope, now);
    let strip = auth_strip(&headers, &jar, &reduced);
    let show_new = user
        .as_ref()
        .map(|u| user_can_post_room(&reduced, &room_id, u))
        .unwrap_or(false);
    drop(reduced);
    rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));

    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "room not found").into_response();
    };
    let slug_display = room_slug.as_str();
    let cli = format!("npx slugsocial private {room_id} forum");

    let page = layout(
        &format!("room {slug_display} — slug.social"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc_room(&nav, slug_display, None)) }
            h2 { (slug_display) }
            p class="muted" { (room_id) }
            h3 { "threads" }
            (render_thread_feed(Some(&nav), "room-thread-feed", &rows, now))
            @if show_new {
                (new_thread_form_for_room(&nav, show_new))
            }
            (cli_panel(&cli))
        },
        None,
    );
    Html(page.into_string()).into_response()
}

fn new_thread_form_for_room(nav: &ThreadNav, show: bool) -> Markup {
    if !show {
        return html! {};
    }
    html! {
        section class="compose" id="room-new-thread-compose" {
            h3 { "new thread in this room" }
            div id="errors" {}
            form method="POST" action="/post" {
                input type="hidden" name="room" value=(nav.room_wire.clone());
                label for="room-new-tag" { "thread tag" }
                input type="text" id="room-new-tag" name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" required;
                textarea name="text" rows="6" placeholder="First post body…" required {}
                p { button type="submit" { "post" } }
            }
        }
    }
}

async fn thread_post_view_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let scope = nav.scope();
    let now = now_ms();
    let (ing, subtitle) = {
        let reduced = state.reduced.read().await;
        let ing = reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned());
        (ing, None::<String>)
    };

    let sc = nav.scope();
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

    let page = layout(
        &format!("#{tag} / post #{index}"),
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc) }
            h2 { "#" (tag) @if let Some(sub) = &subtitle { ": " (sub) } " / post #" (index) }
            @if let Some(ing) = ing {
                @let hover = timeago::rfc3339_utc(ing.ts);
                @let ago = timeago::timeago(now, ing.ts);
                div class="ingest-entry" data-ingest-id=(ing.id) {
                    div class="ingest-meta muted" title=(hover) {
                        span class="address" { (authorship_address(&ing.principal, &ing.delegate)) }
                        " · "
                        (ago)
                    }
                    (render_linkified_with_embeds(&ing.raw))
                }
            } @else {
                p class="muted" { "post not found" }
            }
        },
        None,
    );
    Html(page.into_string()).into_response()
}

pub async fn thread_post_view(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
) -> impl IntoResponse {
    thread_post_view_inner(state, tag, index_str, ThreadNav::public()).await
}

pub async fn room_thread_post_view(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag, index_str)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let room_id = format!("{room_short}/{room_slug}");
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page().into_response();
    }
    drop(reduced);
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    thread_post_view_inner(state, tag, index_str, nav)
        .await
        .into_response()
}

async fn thread_post_expand_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let now = now_ms();
    let scope = nav.scope();

    let ing = {
        let reduced = state.reduced.read().await;
        reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned())
    };

    let Some(ing) = ing else {
        return (StatusCode::NOT_FOUND, "post not found").into_response();
    };

    let post_href = nav.post_url(&tag, index);
    let hover = timeago::rfc3339_utc(ing.ts);
    let ago = timeago::timeago(now, ing.ts);

    let full_html = html! {
        div class="ingest-entry" data-ingest-id=(ing.id) {
            div class="ingest-meta muted" title=(hover) {
                a href=(post_href) class="post-num" { "#" (index) }
                " · "
                (ago)
            }
            (render_linkified_with_embeds(&ing.raw))
        }
    };

    JsBuilder::new()
        .morph_selector(
            &format!("[data-ingest-id=\"{}\"]", ing.id),
            full_html,
        )
        .into_response()
        .into_response()
}

pub async fn thread_post_expand(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
) -> impl IntoResponse {
    thread_post_expand_inner(state, tag, index_str, ThreadNav::public()).await
}

pub async fn room_thread_post_expand(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag, index_str)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let room_id = format!("{room_short}/{room_slug}");
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page().into_response();
    }
    drop(reduced);
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    thread_post_expand_inner(state, tag, index_str, nav)
        .await
        .into_response()
}
