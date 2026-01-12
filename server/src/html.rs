use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
};
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;

use crate::{
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    reducer::GroupKey,
    state::AppState,
    timeago,
};

fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                style { (r#"
                    :root {
                      color-scheme: light dark;
                      --bg: Canvas;
                      --fg: CanvasText;
                      --muted: color-mix(in srgb, CanvasText 65%, transparent);
                      --faint: color-mix(in srgb, CanvasText 35%, transparent);
                      --border: color-mix(in srgb, CanvasText 18%, transparent);
                      --panel: color-mix(in srgb, CanvasText 6%, transparent);
                      --mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
                      --sans: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Helvetica, Arial, "Apple Color Emoji", "Segoe UI Emoji";
                      --serif: ui-serif, Iowan Old Style, Palatino, "Palatino Linotype", Georgia, Times, "Times New Roman", serif;
                    }
                    body {
                      font-family: var(--sans);
                      max-width: 880px;
                      margin: 24px auto;
                      padding: 0 16px;
                      line-height: 1.45;
                      color: var(--fg);
                      background: var(--bg);
                    }
                    a { text-decoration: none; }
                    a:hover { text-decoration: underline; }
                    h1,h2 { margin: 0 0 12px 0; }
                    h3 { margin: 18px 0 8px 0; }
                    .muted { color: var(--muted); }
                    .faint { color: var(--faint); }
                    .row { display: flex; justify-content: space-between; gap: 16px; }
                    pre { padding: 12px; border: 1px solid var(--border); border-radius: 10px; overflow-x: auto; background: var(--panel); }
                    table { width: 100%; border-collapse: collapse; }
                    td, th { padding: 6px 0; border-bottom: 1px solid var(--border); }
                    th { text-align: left; }
                    code { font-family: var(--mono); }
                    .slug { font-family: var(--mono); }
                    .prose { font-family: var(--serif); }

                    /* Votes: structured rendering */
                    .vote { border: 1px solid var(--border); border-radius: 12px; padding: 12px; margin: 10px 0; background: var(--panel); }
                    .vote-header { display: flex; justify-content: space-between; gap: 12px; align-items: baseline; flex-wrap: wrap; }
                    .vote-items { display: flex; gap: 10px; flex-wrap: wrap; }
                    .vote-meta { color: var(--muted); font-size: 0.9em; }
                    .ratio-wrap { margin-top: 8px; }
                    .ratio-label { color: var(--muted); font-size: 0.9em; margin-bottom: 6px; font-family: var(--mono); }
                    .ratio-bar { display: flex; height: 12px; border-radius: 999px; overflow: hidden; border: 1px solid var(--border); background: color-mix(in srgb, CanvasText 3%, transparent); }
                    .ratio-left { background: rgba(46, 160, 67, 0.75); }
                    .ratio-right { background: rgba(248, 81, 73, 0.70); }
                    .vote-body { margin-top: 10px; white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.4; font-family: var(--serif); }

                    /* Component boxes */
                    .component { border: 1px solid var(--border); border-radius: 14px; padding: 12px; margin: 12px 0; background: var(--panel); }
                    .component-meta { font-size: 0.85em; color: var(--faint); font-family: var(--mono); margin-top: -2px; }
                "#) }
            }
            body {
                (body)
            }
        }
    }
}

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

fn ratio_pct(left: i32, right: i32) -> f64 {
    let l = (left.max(0)) as f64;
    let r = (right.max(0)) as f64;
    let denom = l + r;
    if denom <= 0.0 {
        return 50.0;
    }
    (l / denom) * 100.0
}

pub async fn index(State(state): State<AppState>) -> impl IntoResponse {
    #[derive(Clone)]
    struct TagRow {
        tag: String,
        last_ts: i64,
        items: usize,
        aspects: usize,
        recent_votes: usize,
    }

    let now = now_ms();
    let rows: Vec<TagRow> = {
        let reduced = state.reduced.read().await;

        // Union of all "existing" tags we know about.
        let mut all_tags: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        all_tags.extend(reduced.ingests_by_tag.keys().cloned());
        all_tags.extend(reduced.tags.keys().cloned());
        all_tags.extend(reduced.groups.keys().map(|k| k.tag.clone()));

        let mut out = Vec::new();
        for tag in all_tags.into_iter() {
            // Recency: prefer newest ingest timestamp; fallback to newest vote timestamp if present.
            let last_ingest_ts = reduced
                .ingests_by_tag
                .get(&tag)
                .and_then(|q| q.front())
                .map(|ing| ing.ts)
                .unwrap_or(0);

            let mut aspects = 0usize;
            let mut recent_votes = 0usize;
            let mut last_vote_ts = 0i64;
            for (k, g) in reduced.groups.iter() {
                if k.tag != tag {
                    continue;
                }
                aspects += 1;
                recent_votes += g.recent_votes.len();
                if let Some(v) = g.recent_votes.front() {
                    if v.ts > last_vote_ts {
                        last_vote_ts = v.ts;
                    }
                }
            }

            let last_ts = last_ingest_ts.max(last_vote_ts);
            let items = reduced.tags.get(&tag).map(|s| s.len()).unwrap_or(0);
            out.push(TagRow {
                tag,
                last_ts,
                items,
                aspects,
                recent_votes,
            });
        }

        out
    };

    let mut rows = rows;
    rows.sort_by_key(|r| std::cmp::Reverse(r.last_ts));

    let page = layout(
        "slug.social",
        html! {
            h1 { "slug.social" }
            p class="muted" { "collective ranking via pairwise comparisons" }
            h2 { "tags" }
            @if rows.is_empty() {
                p class="muted" { "no tags yet" }
            } @else {
                ul {
                    @for r in rows {
                        @let href = format!("/~/{}", r.tag);
                        @let hover = timeago::rfc3339_utc(r.last_ts);
                        @let ago = timeago::timeago(now, r.last_ts);
                        li {
                            a href=(href) { "#" (r.tag) }
                            " "
                            span class="muted" title=(hover) {
                                (ago)
                                " · "
                                (format!("items={} · aspects={} · recent_votes={}", r.items, r.aspects, r.recent_votes))
                            }
                        }
                    }
                }
            }
        },
    );
    Html(page.into_string())
}

#[derive(Deserialize)]
pub struct ThreadQuery {
    aspect: Option<String>,
}

pub async fn thread_page(
    State(state): State<AppState>,
    Path(tag): Path<String>,
    Query(query): Query<ThreadQuery>,
) -> impl IntoResponse {
    let tag = tag.trim_start_matches('#').to_string();

    // If aspect is specified, render the aspect ranking view
    if let Some(aspect) = query.aspect {
        let aspect = aspect.trim_start_matches(':').to_string();
        return render_aspect_view(state, tag, aspect).await;
    }

    // Otherwise render the thread overview
    let (mut aspects, ingests) = {
        let reduced = state.reduced.read().await;
        let aspects: Vec<String> = reduced
            .groups
            .keys()
            .filter(|k| k.tag == tag)
            .map(|k| k.aspect.clone())
            .collect();
        let ingests = reduced
            .ingests_by_tag
            .get(&tag)
            .cloned()
            .unwrap_or_default();
        (aspects, ingests)
    };
    aspects.sort();
    aspects.dedup();

    let page = layout(
        &format!("#{tag}"),
        html! {
            h1 { "#" (tag) }
            p { a href="/" { "← index" } }
            h2 { "aspects" }
            @if aspects.is_empty() {
                p class="muted" { "no aspects yet" }
            } @else {
                ul {
                    @for a in aspects {
                        li { a href={(format!("/~/{tag}?aspect={a}"))} { ":" (a) } }
                    }
                }
            }

            h2 { "recent ingests" }
            @if ingests.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                @let now = now_ms();
                @for ing in ingests.iter().take(5) {
                    @let hover = timeago::rfc3339_utc(ing.ts);
                    @let ago = timeago::timeago(now, ing.ts);
                    p class="muted" title=(hover) { (format!("{} · @{}", ago, ing.actor)) }
                    pre { (ing.raw) }
                }
            }
        },
    );
    Html(page.into_string()).into_response()
}

async fn render_aspect_view(
    state: AppState,
    tag: String,
    aspect: String,
) -> axum::response::Response {
    let (group, items_in_tag): (crate::reducer::GroupState, Vec<String>) = {
        let reduced = state.reduced.read().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let Some(group) = reduced.groups.get(&key) else {
            let page = layout(
                "not found",
                html! {
                    h1 { "not found" }
                    p { a href="/" { "← index" } }
                },
            );
            return (StatusCode::NOT_FOUND, Html(page.into_string())).into_response();
        };
        let items_in_tag = reduced
            .tags
            .get(&tag)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_else(Vec::new);
        (group.clone(), items_in_tag)
    };

    // Build connected components from recorded voted pairs.
    let n = group.idx_to_item.len();
    let (comps, isolate_idxs) =
        connected_components_from_voted_pairs(n, group.voted_pairs.iter().copied());

    // Items in the tag that have no votes at all in this aspect (not even present in group).
    let mut no_vote_items: Vec<String> = items_in_tag
        .into_iter()
        .filter(|it| !group.item_to_idx.contains_key(it))
        .collect();
    no_vote_items.sort();

    // Sort components by size descending, then by item name for stability.
    let mut comps = comps;
    comps.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let page = layout(
        &format!("#{tag} :{aspect}"),
        html! {
            h1 {
                span class="slug" { "#" (tag) }
                " "
                span class="slug" { ":" (aspect) }
            }
            p { a href={(format!("/~/{tag}"))} { "← #" (tag) } " · " a href="/" { "index" } }

            h2 { "ranking groups (connected components)" }
            @if comps.is_empty() {
                p class="muted" { "no voted pairs yet in this aspect" }
            } @else {
                @for (ci, comp) in comps.iter().enumerate() {
                    @let ranked = ranked_items_subset(&group, comp, 10000, 1e-8);
                    @let pairs = group.voted_pairs.iter().filter(|(i,j)| comp.binary_search(i).is_ok() && comp.binary_search(j).is_ok()).count();
                    div class="component" {
                        h3 { (format!("component {}", ci + 1)) }
                        div class="component-meta" { (format!("items={} · pairs={}", comp.len(), pairs)) }
                        table {
                            thead { tr { th { "item" } th { "score" } } }
                            tbody {
                                @for r in ranked.iter() {
                                    tr {
                                        td { code { "/" (r.item) } }
                                        td { (format!("{:.6}", r.score)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            @if !isolate_idxs.is_empty() {
                h2 { "isolates (in graph, but no voted pair)" }
                ul {
                    @for idx in isolate_idxs {
                        @let name = group.idx_to_item.get(idx).cloned().unwrap_or_default();
                        li { code { "/" (name) } }
                    }
                }
            }

            @if !no_vote_items.is_empty() {
                h2 { "no votes in this aspect" }
                p class="muted" { "items exist in the thread, but have not been compared under this aspect yet" }
                ul {
                    @for it in no_vote_items {
                        li { code { "/" (it) } }
                    }
                }
            }

            h2 { "recent votes" }
            @if group.recent_votes.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                @let now = now_ms();
                @for v in group.recent_votes.iter().take(50) {
                    @let pct = ratio_pct(v.ratio_left, v.ratio_right);
                    @let hover = timeago::rfc3339_utc(v.ts);
                    @let ago = timeago::timeago(now, v.ts);
                    div class="vote" {
                        div class="vote-header" {
                            div class="vote-items" {
                                code { "/" (v.a) }
                                span { "vs" }
                                code { "/" (v.b) }
                            }
                            div class="vote-meta" title=(hover) {
                                (ago) " · @" (v.actor)
                            }
                        }
                        div class="ratio-wrap" {
                            div class="ratio-label" {
                                (format!("ratio {}:{} (left {})", v.ratio_left, v.ratio_right, format!("{:.1}%", pct)))
                            }
                            div class="ratio-bar" aria-label={(format!("ratio {}:{} (left {:.1}%)", v.ratio_left, v.ratio_right, pct))} {
                                div class="ratio-left" style={(format!("width: {:.3}%;", pct))} {}
                                div class="ratio-right" style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                            }
                        }
                        div class="vote-body" { (v.body) }
                    }
                }
            }
        },
    );

    Html(page.into_string()).into_response()
}


