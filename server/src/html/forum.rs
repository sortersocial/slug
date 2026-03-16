use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse},
};
use maud::{html, Markup};

use crate::{
    events::canonicalize_tag,
    reducer::ReducerState,
    state::AppState,
    timeago,
};

use super::{
    actor_label, bc_threads, layout, linkify_slugs, now_ms, recency_class,
};

#[derive(Clone)]
struct ThreadRow {
    tag: String,
    last_ts: i64,
    ingests: usize,
    subscriber_count: usize,
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
                subscriber_count: thread.subscriber_count,
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
                                (format!("{}n {}s", r.ingests, r.subscriber_count))
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

    let page = layout(
        "slug.social",
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_threads(None)) }
            h2 { "threads" }
            (render_thread_feed(&rows, now))

        },
    );
    Html(page.into_string())
}

/// Thread view — `/t/:tag` — dark, recent ingests only.
pub async fn thread_view(
    State(state): State<AppState>,
    Path(tag): Path<String>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let (global_viewers, local_viewers) = state.presence_counts(&tag).await;

    let ingest_ids = {
        let reduced = state.reduced.read().await;
        reduced.ingests_by_thread.get(&tag).cloned().unwrap_or_default()
    };
    let ingests = {
        let reduced = state.reduced.read().await;
        ingest_ids
            .iter()
            .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
            .collect::<Vec<_>>()
    };

    // ingests_by_thread is newest-first (push_front). Take 50 most recent, render oldest-first.
    let mut display_ingests: Vec<_> = ingests.iter().take(50).collect();
    display_ingests.reverse();

    let now = now_ms();

    let page = layout(
        &format!("#{tag}"),
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_threads(Some(&tag))) }
            h2 { "#" (tag) }
            div id="presence-bar" class="presence-bar muted" data-thread-tag=(tag) {
                span { "viewing now: " span id="presence-global" { (global_viewers) } }
                " · "
                span { "neighbors here: " span id="presence-local" { (local_viewers) } }
            }
            @if display_ingests.is_empty() {
                p class="muted" { "no activity yet" }
            } @else {
                @for ing in &display_ingests {
                    @let hover = timeago::rfc3339_utc(ing.ts);
                    @let ago = timeago::timeago(now, ing.ts);
                    div class="ingest-entry" data-ingest-id=(ing.id) {
                        div class="ingest-meta muted" title=(hover) {
                            span class="address" { "@" (actor_label(&ing.actor)) }
                            " · "
                            (ago)
                        }
                        pre { (maud::PreEscaped(linkify_slugs(&ing.raw))) }
                    }
                }
            }

        },
    );
    Html(page.into_string()).into_response()
}
