use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};
use serde::Deserialize;

use crate::{
    api::optional_principal,
    canonical_path::{canonicalize_item, canonicalize_tag},
    events::ThreadCapability,
    form_template::template_json_compact,
    identity::parse_username,
    reducer::{scope_from_room_wire, ReducerState, ScopeId},
    state::AppState,
    timeago,
};

use super::ui_action::{HtmlUiAction, UI_RPC_FIELD};

use super::{
    bc_segment, bc_threads, cli_panel, layout, now_ms, profile_href, recency_class,
    render_linkified_with_embeds_in_scope, theme_from_jar, theme_next_from_uri, JsBuilder,
};

#[derive(Clone)]
struct ThreadRow {
    tag: String,
    subtitle: Option<String>,
    last_ts: i64,
    ingests: usize,
}

#[derive(Clone)]
struct RoomMemberRow {
    username: String,
    capabilities: Vec<&'static str>,
}

/// URL helpers for public `/t/…` and private room threads `/r/{short}/{slug}/t/…`.
#[derive(Clone)]
pub struct ThreadNav {
    pub room_wire: String,
    scope: ScopeId,
    room_path: String,
    thread_path_prefix: String,
    garden_path_prefix: String,
}

impl ThreadNav {
    pub(crate) fn public() -> Self {
        Self {
            room_wire: "public".into(),
            scope: ScopeId::Public,
            room_path: "/t".into(),
            thread_path_prefix: "/t".into(),
            garden_path_prefix: "/~".into(),
        }
    }

    /// `room_id` wire form `shortid/slug`.
    pub(crate) fn from_room_id(room_id: &str) -> Option<Self> {
        let (short, slug) = room_id.split_once('/')?;
        if short.is_empty() || slug.is_empty() {
            return None;
        }
        Some(Self {
            room_wire: room_id.to_string(),
            scope: ScopeId::Room(room_id.to_string()),
            room_path: format!("/r/{short}/{slug}"),
            thread_path_prefix: format!("/r/{short}/{slug}/t"),
            garden_path_prefix: format!("/r/{short}/{slug}/~"),
        })
    }

    pub(crate) fn scope(&self) -> ScopeId {
        self.scope.clone()
    }

    pub(crate) fn room_url(&self) -> &str {
        &self.room_path
    }

    pub(crate) fn thread_url(&self, tag: &str) -> String {
        format!("{}/{}", self.thread_path_prefix, tag)
    }

    pub(crate) fn garden_root_url(&self) -> &str {
        &self.garden_path_prefix
    }

    pub(crate) fn garden_item_url(&self, item: &str) -> String {
        if let Some(tail) = crate::path_types::CanonicalItemUrl::parse(item)
            .and_then(|c| c.tilde_tail().map(str::to_owned))
        {
            format!("{}/{}", self.garden_path_prefix, tail)
        } else {
            format!("{}/{}", self.garden_path_prefix, canonicalize_item(item))
        }
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

    fn expand_deleted_url(&self, tag: &str, idx: usize) -> String {
        format!("{}/{}/{}/expand-deleted", self.thread_path_prefix, tag, idx)
    }

    fn collapse_deleted_url(&self, tag: &str, idx: usize) -> String {
        format!("{}/{}/{}/collapse-deleted", self.thread_path_prefix, tag, idx)
    }
}

fn thread_nav_for_ingest(ing: &crate::events::Ingest) -> Option<ThreadNav> {
    let room = ing.room_id.trim();
    if room.is_empty() || room == "public" {
        Some(ThreadNav::public())
    } else {
        ThreadNav::from_room_id(room)
    }
}

fn thread_post_index_in_scope(reduced: &ReducerState, ing: &crate::events::Ingest) -> Option<usize> {
    let scope = scope_from_room_wire(&ing.room_id);
    let tag = canonicalize_tag(&ing.thread_tag);
    reduced
        .ingests_by_scope_thread
        .get(&(scope, tag))
        .and_then(|q| q.iter().rev().position(|id| id == &ing.id))
}

fn post_header_meta(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    principal: &str,
    ts: i64,
    now: i64,
) -> Markup {
    let post_href = nav.post_url(tag, post_idx);
    let profile = profile_href(principal);
    let hover = timeago::rfc3339_utc(ts);
    let ago = timeago::timeago(now, ts);
    html! {
        div class="ingest-meta muted" title=(hover) {
            a href=(post_href) class="post-num" { "#" (post_idx) }
            " "
            a href=(profile) class="post-author" { "@" (principal) }
            " · "
            (ago)
        }
    }
}

fn post_header_row(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    _viewer: Option<&str>,
    now: i64,
    show_delete: bool,
) -> Markup {
    let meta = post_header_meta(nav, tag, post_idx, &ing.principal, ing.ts, now);
    html! {
        div class="ingest-header-row" {
            (meta)
            @if show_delete {
                form class="post-delete-form" method="POST" action="/post/redact" {
                    input type="hidden" name="post_id" value=(ing.id);
                    button type="submit" class="post-delete-btn" { "delete" }
                }
            }
        }
    }
}

fn redacted_header_row(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    now: i64,
    expanded: bool,
) -> Markup {
    let meta = post_header_meta(nav, tag, post_idx, &ing.principal, ing.ts, now);
    let exp = nav.expand_deleted_url(tag, post_idx);
    let coll = nav.collapse_deleted_url(tag, post_idx);
    html! {
        div class="ingest-header-row ingest-tombstone-row" {
            (meta)
            span class="post-tombstone-inline muted" {
                "deleted · "
                @if expanded {
                    a href="#" class="hide-deleted-link"
                      onclick=(format!("fetch('{coll}').then(r=>r.text()).then(eval);return false")) {
                        "[hide deleted content]"
                    }
                } @else {
                    a href="#" class="show-deleted-link"
                      onclick=(format!("fetch('{exp}').then(r=>r.text()).then(eval);return false")) {
                        "[show deleted content]"
                    }
                }
            }
        }
    }
}

fn ingest_entry_markup(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    viewer: Option<&str>,
    now: i64,
    reduced: &ReducerState,
) -> Markup {
    let redacted = reduced.redacted_posts.contains(&ing.id);
    let show_delete = viewer == Some(ing.principal.as_str()) && !redacted;
    if redacted {
        html! {
            div class="ingest-entry ingest-redacted" data-ingest-id=(ing.id) {
                (redacted_header_row(nav, tag, post_idx, ing, now, false))
            }
        }
    } else {
        let truncated = ing.raw.len() > 2000;
        let display_body = if truncated { &ing.raw[..2000] } else { &ing.raw[..] };
        html! {
            div class="ingest-entry" data-ingest-id=(ing.id) {
                (post_header_row(nav, tag, post_idx, ing, viewer, now, show_delete))
                (render_linkified_with_embeds_in_scope(display_body, nav.garden_root_url()))
                @if truncated {
                    @let exp = nav.expand_url(tag, post_idx);
                    a href="#" class="show-full-link"
                      onclick=(format!("fetch('{exp}').then(r=>r.text()).then(eval);return false")) {
                        "[show full post]"
                    }
                }
            }
        }
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

pub(crate) fn user_can_view_room(reduced: &ReducerState, room_id: &str, username: Option<&str>) -> bool {
    if !reduced.rooms.contains(room_id) {
        return false;
    }
    let Some(u) = username else {
        return false;
    };
    reduced.user_has_cap(room_id, u, ThreadCapability::View)
}

pub(crate) fn user_can_post_room(reduced: &ReducerState, room_id: &str, username: &str) -> bool {
    reduced.user_has_cap(room_id, username, ThreadCapability::Post)
}

fn capability_label(cap: ThreadCapability) -> &'static str {
    match cap {
        ThreadCapability::View => "view",
        ThreadCapability::Post => "post",
        ThreadCapability::Vote => "vote",
        ThreadCapability::AddItem => "add_item",
        ThreadCapability::Manage => "manage",
    }
}

fn room_members_for_room(reduced: &ReducerState, room_id: &str) -> Vec<RoomMemberRow> {
    let mut rows: Vec<RoomMemberRow> = reduced
        .grants
        .get(room_id)
        .into_iter()
        .flat_map(|members| members.iter())
        .map(|(username, caps)| {
            let mut ordered = Vec::new();
            for cap in [
                ThreadCapability::View,
                ThreadCapability::Post,
                ThreadCapability::Vote,
                ThreadCapability::AddItem,
                ThreadCapability::Manage,
            ] {
                if caps.contains(&cap) {
                    ordered.push(capability_label(cap));
                }
            }
            RoomMemberRow {
                username: username.clone(),
                capabilities: ordered,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.username.cmp(&b.username));
    rows
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
                &format!("room:{room_slug}"),
                nav.room_url(),
                false,
            ))
            (bc_segment(&format!("#{t}"), &nav.thread_url(t), true))
        } @else {
            (bc_segment(
                &format!("room:{room_slug}"),
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
            form id="thread-compose-form" method="POST" action="/post" data-check-action="/post/check" {
                input type="hidden" name="room" value=(nav.room_wire.clone());
                input type="hidden" name="thread_tag" value=(thread_tag);
                input type="hidden" name="error_target" value="thread-compose-errors";
                input type="hidden" name="form_id" value="thread-compose-form";
                textarea name="text" rows="5" cols="80" placeholder="prose or ~/items and votes…" {}
                p {
                    button type="submit" { "post" }
                }
            }
            div id="thread-compose-errors" {}
        }
    }
}

fn new_thread_form_public(show: bool) -> Markup {
    if !show {
        return html! {};
    }
    html! {
        button
            type="button"
            class="form-toggle"
            data-toggle-target="#public-new-thread-compose"
            data-open-label="new public thread"
            data-close-label="hide new public thread"
            aria-expanded="false"
        {
            "new public thread"
        }
        section class="compose" id="public-new-thread-compose" hidden {
            h3 { "new public thread" }
            p class="muted" { "Set thread tag and body. Example: start with a title line or use the CLI-shaped DSL." }
            div id="public-new-thread-errors" {}
            form id="public-new-thread-form" method="POST" action="/post" data-check-action="/post/check" {
                input type="hidden" name="room" value="public";
                input type="hidden" name="error_target" value="public-new-thread-errors";
                input type="hidden" name="form_id" value="public-new-thread-form";
                label for="new-thread-tag" { "thread tag" }
                input type="text" id="new-thread-tag" name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" placeholder="my-topic";
                label for="new-thread-text" { "text" }
                textarea id="new-thread-text" name="text" rows="4" placeholder="#my-topic\n\nYour first post…" {}
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
            p class="muted" { "dark = time-ordered · light = vote-ranked" }
            div class="thread-feed-toolbar" {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(expand_public_new_thread_rpc_value());
                    button type="submit" class="section-add-btn" { "+" }
                }
            }
            div id="public-new-thread-ui-slot" {}
            (render_thread_feed(Some(&nav), "thread-feed", &public_rows, now))
            (cli_panel("npx slugsocial public forum list"))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
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
                a href="#" class="post-nav-btn disabled" { "← older" }
            }
            span class="post-nav-pos muted" {
                (offset + 1) "–" (total.min(offset + PAGE_SIZE)) " / " (total)
            }
            @if let Some(o) = newer_offset {
                a href=(nav.thread_page_url(tag, o)) class="post-nav-btn" { "newer →" }
            } @else {
                a href="#" class="post-nav-btn disabled" { "newer →" }
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
            (cli_panel(&cli))
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

fn room_not_found_page(jar: &CookieJar, uri: &Uri) -> impl IntoResponse {
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
    let members = room_members_for_room(&reduced, &room_id);
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
    let forum_cli = format!("npx slugsocial private {room_id} forum list");
    let garden_cli = format!("npx slugsocial private {room_id} garden tree");
    let audit_cli = format!("npx slugsocial private {room_id} audit");

    let page = layout(
        &format!("room {slug_display} — slug.social"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" { (bc_room(&nav, slug_display, None)) }
            h2 { (slug_display) }
            p class="muted" { (room_id) }
            p class="muted room-links" {
                "room garden · "
                a href=(nav.garden_root_url()) { "~" }
            }
            @if !members.is_empty() {
                h3 { "members" }
                ul class="room-members" {
                    @for member in &members {
                        li {
                            span class="room-member-name" { "@" (member.username) }
                            span class="muted" {
                                " · "
                                (member.capabilities.join(", "))
                            }
                        }
                    }
                }
            }
            h3 { "threads" }
            (render_thread_feed(Some(&nav), "room-thread-feed", &rows, now))
            @if show_new {
                div class="thread-feed-toolbar" {
                    form method="POST" action="/ui" {
                        input type="hidden" name=(UI_RPC_FIELD) value=(expand_room_new_thread_rpc_value(&nav));
                        button type="submit" class="section-add-btn" { "+" }
                    }
                }
                div id="room-new-thread-ui-slot" {}
            }
            (cli_panel(&forum_cli))
            (cli_panel(&garden_cli))
            (cli_panel(&audit_cli))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}

fn new_thread_form_for_room(nav: &ThreadNav, show: bool) -> Markup {
    if !show {
        return html! {};
    }
    html! {
        button
            type="button"
            class="form-toggle"
            data-toggle-target="#room-new-thread-compose"
            data-open-label="new thread in this room"
            data-close-label="hide room thread form"
            aria-expanded="false"
        {
            "new thread in this room"
        }
        section class="compose" id="room-new-thread-compose" hidden {
            h3 { "new thread in this room" }
            div id="room-new-thread-errors" {}
            form id="room-new-thread-form" method="POST" action="/post" data-check-action="/post/check" {
                input type="hidden" name="room" value=(nav.room_wire.clone());
                input type="hidden" name="error_target" value="room-new-thread-errors";
                input type="hidden" name="form_id" value="room-new-thread-form";
                label for="room-new-tag" { "thread tag" }
                input type="text" id="room-new-tag" name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" required;
                textarea name="text" rows="4" placeholder="First post body…" required {}
                p { button type="submit" { "post" } }
            }
        }
    }
}

pub(crate) fn expand_public_new_thread_rpc_value() -> String {
    template_json_compact(&HtmlUiAction::ExpandPublicNewThreadForm).expect("static json")
}

pub(crate) fn expand_room_new_thread_rpc_value(nav: &ThreadNav) -> String {
    template_json_compact(&HtmlUiAction::ExpandRoomNewThreadForm {
        room_wire: nav.room_wire.clone(),
    })
    .expect("static json")
}

pub(crate) fn login_to_post_hint_markup() -> Markup {
    html! {
        p class="muted" { "log in to post" }
    }
}

pub(crate) fn fragment_public_new_thread_form(show: bool) -> Markup {
    new_thread_form_public(show)
}

pub(crate) fn fragment_room_new_thread_form(nav: &ThreadNav, show: bool) -> Markup {
    new_thread_form_for_room(nav, show)
}

async fn thread_post_view_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
    viewer: Option<String>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let scope = nav.scope();
    let now = now_ms();
    let (ing, subtitle, body_markup) = {
        let reduced = state.reduced.read().await;
        let ing = reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned());
        let body = ing.as_ref().map(|i| {
            ingest_entry_markup(&nav, &tag, index, i, viewer.as_deref(), now, &reduced)
        });
        (ing, None::<String>, body)
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
            @if let (Some(_ing), Some(bm)) = (&ing, &body_markup) {
                (bm)
            } @else {
                p class="muted" { "post not found" }
            }
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}

pub async fn thread_post_view(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let viewer = optional_principal(&headers, &jar, &reduced);
    drop(reduced);
    thread_post_view_inner(state, tag, index_str, ThreadNav::public(), viewer, jar, uri)
        .await
}

pub async fn room_thread_post_view(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag, index_str)): Path<(String, String, String, String)>,
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
    thread_post_view_inner(state, tag, index_str, nav, user, jar, uri)
        .await
        .into_response()
}

async fn thread_post_expand_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
    viewer: Option<String>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let now = now_ms();
    let scope = nav.scope();

    let reduced = state.reduced.read().await;
    let ing = reduced
        .ingests_by_scope_thread
        .get(&(scope.clone(), tag.clone()))
        .and_then(|q| q.iter().rev().nth(index))
        .and_then(|id| reduced.ingests_by_id.get(id).cloned());
    let Some(ing) = ing else {
        return (StatusCode::NOT_FOUND, "post not found").into_response();
    };
    if reduced.redacted_posts.contains(&ing.id) {
        return (StatusCode::NOT_FOUND, "post not found").into_response();
    };
    let ing_id = ing.id.clone();
    drop(reduced);

    let full_html = html! {
        div class="ingest-entry" data-ingest-id=(ing_id) {
            (post_header_row(
                &nav,
                &tag,
                index,
                &ing,
                viewer.as_deref(),
                now,
                viewer.as_deref() == Some(ing.principal.as_str()),
            ))
            (render_linkified_with_embeds_in_scope(&ing.raw, nav.garden_root_url()))
        }
    };

    JsBuilder::new()
        .morph_selector(
            &format!("[data-ingest-id=\"{}\"]", ing_id),
            full_html,
        )
        .into_response()
        .into_response()
}

async fn thread_post_expand_deleted_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
    _viewer: Option<String>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let now = now_ms();
    let scope = nav.scope();

    let full_html = {
        let reduced = state.reduced.read().await;
        let ing = reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned());
        let Some(ing) = ing else {
            return (StatusCode::NOT_FOUND, "post not found").into_response();
        };
        if !reduced.redacted_posts.contains(&ing.id) {
            return (StatusCode::NOT_FOUND, "post not found").into_response();
        };
        html! {
            div class="ingest-entry ingest-redacted-expanded" data-ingest-id=(ing.id) {
                (redacted_header_row(&nav, &tag, index, &ing, now, true))
                (render_linkified_with_embeds_in_scope(&ing.raw, nav.garden_root_url()))
            }
        }
    };

    let ing_id = {
        let reduced = state.reduced.read().await;
        reduced
            .ingests_by_scope_thread
            .get(&(nav.scope(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .cloned()
    };
    let Some(ing_id) = ing_id else {
        return (StatusCode::NOT_FOUND, "post not found").into_response();
    };

    JsBuilder::new()
        .morph_selector(
            &format!("[data-ingest-id=\"{}\"]", ing_id),
            full_html,
        )
        .into_response()
        .into_response()
}

async fn thread_post_collapse_deleted_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
    viewer: Option<String>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let now = now_ms();
    let scope = nav.scope();

    let collapsed = {
        let reduced = state.reduced.read().await;
        let ing = reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned());
        let Some(ing) = ing else {
            return (StatusCode::NOT_FOUND, "post not found").into_response();
        };
        if !reduced.redacted_posts.contains(&ing.id) {
            return (StatusCode::NOT_FOUND, "post not found").into_response();
        };
        ingest_entry_markup(&nav, &tag, index, &ing, viewer.as_deref(), now, &reduced)
    };

    let ing_id = {
        let reduced = state.reduced.read().await;
        reduced
            .ingests_by_scope_thread
            .get(&(nav.scope(), tag.clone()))
            .and_then(|q| q.iter().rev().nth(index))
            .cloned()
    };
    let Some(ing_id) = ing_id else {
        return (StatusCode::NOT_FOUND, "post not found").into_response();
    };

    JsBuilder::new()
        .morph_selector(
            &format!("[data-ingest-id=\"{}\"]", ing_id),
            collapsed,
        )
        .into_response()
        .into_response()
}

struct ProfilePostRow {
    thread_tag: String,
    post_idx: usize,
    post_href: String,
    ts: i64,
    snippet: String,
}

pub async fn user_profile_page(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let canon = match parse_username(&username) {
        Ok(u) => u,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };

    let (rows, strip) = {
        let reduced = state.reduced.read().await;
        let viewer = optional_principal(&headers, &jar, &reduced);
        let ids = reduced.visible_posts_for_actor(&canon, viewer.as_deref());
        let mut rows: Vec<ProfilePostRow> = Vec::new();
        for id in ids {
            if reduced.redacted_posts.contains(&id) {
                continue;
            }
            let Some(ing) = reduced.ingests_by_id.get(&id).cloned() else {
                continue;
            };
            let Some(nav) = thread_nav_for_ingest(&ing) else {
                continue;
            };
            let Some(post_idx) = thread_post_index_in_scope(&reduced, &ing) else {
                continue;
            };
            let tag = canonicalize_tag(&ing.thread_tag);
            let post_href = nav.post_url(&tag, post_idx);
            let raw_one_line = ing.raw.lines().next().unwrap_or("").trim();
            let snippet: String = raw_one_line.chars().take(120).collect();
            rows.push(ProfilePostRow {
                thread_tag: tag,
                post_idx,
                post_href,
                ts: ing.ts,
                snippet,
            });
        }
        rows.sort_by(|a, b| b.ts.cmp(&a.ts));
        let strip = auth_strip(&headers, &jar, &reduced);
        (rows, strip)
    };

    let now = now_ms();
    let page = layout(
        &format!("@{canon}"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" {
                a href="/" { "slug.social" }
                (bc_segment(&format!("@{canon}"), &profile_href(&canon), true))
            }
            h2 { "@" (canon) }
            p class="muted" { "posts (newest first)" }
            @if rows.is_empty() {
                p class="muted" { "no public posts yet" }
            } @else {
                ul class="profile-post-list" {
                    @for r in &rows {
                        @let hover = timeago::rfc3339_utc(r.ts);
                        @let ago = timeago::timeago(now, r.ts);
                        li {
                            a href=(r.post_href.as_str()) {
                                "#" (r.thread_tag)
                                " / #"
                                (r.post_idx)
                            }
                            span class="muted" title=(hover) { " · " (ago) }
                            @if !r.snippet.is_empty() {
                                p class="profile-post-snippet muted" { (r.snippet) }
                            }
                        }
                    }
                }
            }
            (cli_panel(&format!("npx slugsocial public forum list")))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}

pub async fn thread_post_expand(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let viewer = optional_principal(&headers, &jar, &reduced);
    drop(reduced);
    thread_post_expand_inner(state, tag, index_str, ThreadNav::public(), viewer)
        .await
}

pub async fn room_thread_post_expand(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag, index_str)): Path<(String, String, String, String)>,
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
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        drop(reduced);
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    drop(reduced);
    thread_post_expand_inner(state, tag, index_str, nav, user)
        .await
        .into_response()
}

pub async fn thread_post_expand_deleted(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let viewer = optional_principal(&headers, &jar, &reduced);
    drop(reduced);
    thread_post_expand_deleted_inner(state, tag, index_str, ThreadNav::public(), viewer)
        .await
}

pub async fn room_thread_post_expand_deleted(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag, index_str)): Path<(String, String, String, String)>,
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
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        drop(reduced);
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    drop(reduced);
    thread_post_expand_deleted_inner(state, tag, index_str, nav, user)
        .await
        .into_response()
}

pub async fn thread_post_collapse_deleted(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let viewer = optional_principal(&headers, &jar, &reduced);
    drop(reduced);
    thread_post_collapse_deleted_inner(state, tag, index_str, ThreadNav::public(), viewer)
        .await
}

pub async fn room_thread_post_collapse_deleted(
    State(state): State<AppState>,
    Path((room_short, room_slug, tag, index_str)): Path<(String, String, String, String)>,
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
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        drop(reduced);
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    drop(reduced);
    thread_post_collapse_deleted_inner(state, tag, index_str, nav, user)
        .await
        .into_response()
}
