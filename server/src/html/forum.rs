use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use maud::{html, Markup};

use crate::{
    events::canonicalize_tag,
    reducer::ReducerState,
    state::AppState,
    timeago,
};

use super::{
    actor_label, bc_segment, bc_threads, cli_panel, layout, now_ms, recency_class,
    render_linkified_with_embeds,
};

#[derive(Clone)]
struct ThreadRow {
    tag: String,
    last_ts: i64,
    ingests: usize,
}

/// Collect thread rows from reducer state (unsorted).
fn collect_thread_rows(reduced: &ReducerState, now: i64) -> Vec<ThreadRow> {
    let _ = now;
    reduced
        .threads
        .iter()
        .map(|(tag, thread)| {
            let ingests = reduced.ingests_by_thread.get(tag).map(|q| q.len()).unwrap_or(0);
            ThreadRow {
                tag: tag.clone(),
                last_ts: thread.last_activity_ts,
                ingests,
            }
        })
        .collect()
}

/// Render the thread feed div (id="thread-feed"). Used by both index() and SSE broadcast.
fn render_thread_feed(rows: &[ThreadRow], now: i64) -> Markup {
    html! {
        div id="thread-feed" {
            @if rows.is_empty() {
                p class="muted" { "no threads yet" }
            } @else {
                ul class="thread-feed" {
                    @for r in rows {
                        @let thread_href = format!("/t/{}", r.tag);
                        @let hover = timeago::rfc3339_utc(r.last_ts);
                        @let ago = timeago::timeago(now, r.last_ts);
                        @let age_cls = recency_class(now, r.last_ts);
                        li class=(age_cls) {
                            a href=(thread_href) { "#" (r.tag) }
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


/// Returns the current thread feed HTML fragment for SSE broadcast.
/// selector: `#thread-feed`
pub async fn thread_feed_html(state: &AppState) -> String {
    let now = now_ms();
    let mut rows = {
        let reduced = state.reduced.read().await;
        collect_thread_rows(&reduced, now)
    };
    rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    render_thread_feed(&rows, now).into_string()
}

pub async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let now = now_ms();
    let mut rows: Vec<ThreadRow> = {
        let reduced = state.reduced.read().await;
        collect_thread_rows(&reduced, now)
    };
    // Bump order: most recently active first.
    rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));

    let views = state.views.get_views("/");
    let page = layout(
        "slug.social",
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_threads(None)) }
            p class="muted" { "dark = time-ordered · light = vote-ranked" }
            h2 { "threads" }
            (render_thread_feed(&rows, now))
            (cli_panel("npx slugsocial forum"))
        },
        Some(views),
    );
    Html(page.into_string())
}

fn bc_thread_post(tag: &str, index: usize) -> Markup {
    let page_offset = (index / PAGE_SIZE) * PAGE_SIZE;
    let thread_href = format!("/t/{tag}?offset={page_offset}");
    html! {
        a href="/" { "slug.social" }
        (bc_segment(&format!("#{tag}"), &thread_href, false))
        (bc_segment(&index.to_string(), &format!("/t/{tag}/{index}"), true))
    }
}


#[derive(Debug, Deserialize)]
pub struct ThreadViewQuery {
    pub offset: Option<usize>,
}

const PAGE_SIZE: usize = 10;

fn render_thread_paginator(tag: &str, offset: usize, total: usize, top: bool) -> Markup {
    let newer_offset = offset.checked_add(PAGE_SIZE).filter(|&o| o < total);
    let older_offset = if offset > 0 { Some(offset.saturating_sub(PAGE_SIZE)) } else { None };
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
                a href=(format!("/t/{tag}?offset={o}")) class="post-nav-btn" { "← older" }
            } @else {
                span class="post-nav-btn disabled" { "← older" }
            }
            span class="post-nav-pos muted" {
                (offset + 1) "–" (total.min(offset + PAGE_SIZE)) " / " (total)
            }
            @if let Some(o) = newer_offset {
                a href=(format!("/t/{tag}?offset={o}")) class="post-nav-btn" { "newer →" }
            } @else {
                span class="post-nav-btn disabled" { "newer →" }
            }
            @if !on_latest {
                a href=(format!("/t/{tag}?offset={latest_offset}")) class="post-nav-btn" { "latest" }
            }
        }
    }
}

/// Thread view — `/t/:tag` — dark, paginated.
pub async fn thread_view(
    State(state): State<AppState>,
    Path(tag): Path<String>,
    Query(q): Query<ThreadViewQuery>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);

    // ingests_by_thread is newest-first; collect all IDs in chronological order.
    let all_ids: Vec<String> = {
        let reduced = state.reduced.read().await;
        reduced
            .ingests_by_thread
            .get(&tag)
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    };

    let total = all_ids.len();
    // Default: first page (oldest posts first, like a book).
    let offset = q.offset.unwrap_or(0);
    let page_ids: Vec<String> = all_ids.into_iter().skip(offset).take(PAGE_SIZE).collect();

    let display_ingests = {
        let reduced = state.reduced.read().await;
        page_ids
            .iter()
            .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
            .collect::<Vec<_>>()
    };

    let now = now_ms();
    let paginator_top = render_thread_paginator(&tag, offset, total, true);
    let paginator_bot = render_thread_paginator(&tag, offset, total, false);

    let views = state.views.get_views(&format!("/t/{tag}"));
    let page = layout(
        &format!("#{tag}"),
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_threads(Some(&tag))) }
            h2 { "#" (tag) }
            p class="muted" { "top=oldest · bottom=newest" }
            @if display_ingests.is_empty() {
                p class="muted" { "no activity yet" }
            } @else {
                (paginator_top)
                @for (i, ing) in display_ingests.iter().enumerate() {
                    @let post_idx = offset + i;
                    @let post_href = format!("/t/{tag}/{post_idx}");
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
                            a href="#" class="show-full-link"
                              onclick=(format!("fetch('/t/{tag}/{post_idx}/expand').then(r=>r.text()).then(eval);return false")) {
                                "[show full post]"
                            }
                        }
                    }
                }
                (paginator_bot)
            }
            (cli_panel(&format!("npx slugsocial forum {tag}")))
        },
        Some(views),
    );
    Html(page.into_string()).into_response()
}

/// Single-post view — `/t/:tag/:index` — shows one ingest at full length.
pub async fn thread_post_view(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let now = now_ms();
    let index: usize = index_str.parse().unwrap_or(0);
    let ing = {
        let reduced = state.reduced.read().await;
        // ingests_by_thread is most-recent-first; chronological index 0 = oldest = last in deque.
        reduced.ingests_by_thread.get(&tag)
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned())
    };

    let views = state.views.get_views(&format!("/t/{tag}/{index}"));
    let page = layout(
        &format!("#{tag} / post #{index}"),
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_threads(Some(&tag))) }
            h2 { "#" (tag) " / post #" (index) }
            @if let Some(ing) = ing {
                @let hover = timeago::rfc3339_utc(ing.ts);
                @let ago = timeago::timeago(now, ing.ts);
                div class="ingest-entry" data-ingest-id=(ing.id) {
                    div class="ingest-meta muted" title=(hover) {
                        span class="address" { "@" (actor_label(&ing.actor)) }
                        " · "
                        (ago)
                    }
                    (render_linkified_with_embeds(&ing.raw))
                }
            } @else {
                p class="muted" { "post not found" }
            }
        },
        Some(views),
    );
    Html(page.into_string()).into_response()
}

/// Inline expand handler — `/t/:tag/:id/expand`
/// Returns a JS snippet that morphs the truncated post into its full version.
pub async fn thread_post_expand(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let now = now_ms();

    let ing = {
        let reduced = state.reduced.read().await;
        reduced
            .ingests_by_thread
            .get(&tag)
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned())
    };

    let Some(ing) = ing else {
        return (StatusCode::NOT_FOUND, "post not found").into_response();
    };

    let post_href = format!("/t/{tag}/{index}");
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

    let full_html_str = full_html.into_string();
    // Escape backticks and backslashes for JS template literal
    let escaped = full_html_str
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("${", "\\${");

    let ingest_id_escaped = ing.id.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        "Idiomorph.morph(document.querySelector('[data-ingest-id=\"{ingest_id_escaped}\"]'), `{escaped}`)"
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        .body(js)
        .unwrap()
        .into_response()
}
