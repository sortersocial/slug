use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;

use crate::{
    events::{canonicalize_aspect, canonicalize_item, canonicalize_tag, item_parent_path},
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    reducer::{GroupKey, ItemKey, ReducerState},
    state::AppState,
    timeago,
};

// Embed CSS files at compile time
const THEME_DEFAULT_CSS: &str = include_str!("../static/theme_default.css");
const THEME_RETRO_CSS: &str = include_str!("../static/theme_retro.css");

pub async fn serve_theme_css(Path(filename): Path<String>) -> impl IntoResponse {
    // Extract theme name from filename like "theme_default.css" or "theme_retro.css"
    let theme = filename
        .strip_prefix("theme_")
        .and_then(|s| s.strip_suffix(".css"));
    
    let css = match theme {
        Some("default") => THEME_DEFAULT_CSS,
        Some("retro") => THEME_RETRO_CSS,
        _ => return (StatusCode::NOT_FOUND, "theme not found").into_response(),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(css.to_string())
        .unwrap()
        .into_response()
}

fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href="/static/theme_default.css" id="theme-stylesheet";
                script src="https://unpkg.com/idiomorph@0.3.0/dist/idiomorph.min.js" {}
            }
            body {
                (body)
                div id="controls" {
                    div id="spread-control" {
                        span { "spread" }
                        input type="range" id="spread-slider" min="0" max="1" step="0.05" value="1";
                    }
                    div id="theme-switcher" { "theme" }
                }
                script { (maud::PreEscaped(r#"
                    (function() {
                        // Theme switching
                        const themes = ['default', 'retro'];
                        const storedTheme = localStorage.getItem('slug-theme') || 'default';
                        const switcher = document.getElementById('theme-switcher');
                        const stylesheet = document.getElementById('theme-stylesheet');

                        function setTheme(name) {
                            stylesheet.href = `/static/theme_${name}.css`;
                            localStorage.setItem('slug-theme', name);
                            switcher.textContent = name;
                            setTimeout(() => setSpread(parseFloat(slider.value)), 50);
                        }

                        setTheme(storedTheme);

                        switcher.addEventListener('click', function() {
                            const current = themes.indexOf(localStorage.getItem('slug-theme') || 'default');
                            const next = (current + 1) % themes.length;
                            setTheme(themes[next]);
                        });

                        // Spread control
                        const slider = document.getElementById('spread-slider');
                        const storedSpread = localStorage.getItem('slug-spread');

                        function setSpread(value) {
                            document.documentElement.style.setProperty('--spread', value);
                            localStorage.setItem('slug-spread', value);
                        }

                        if (storedSpread !== null) {
                            slider.value = storedSpread;
                            setSpread(parseFloat(storedSpread));
                        }

                        slider.addEventListener('input', function() {
                            setSpread(parseFloat(this.value));
                        });

                        // Poem: intercept POST forms, send via fetch, await SSE for DOM update.
                        document.addEventListener('submit', async (e) => {
                            const f = e.target;
                            if (!f || f.tagName !== 'FORM') return;
                            if ((f.method || 'get').toLowerCase() !== 'post') return;
                            e.preventDefault();
                            const btn = f.querySelector('button[type="submit"], input[type="submit"]');
                            if (btn) { btn.disabled = true; btn.textContent = '…'; }
                            await fetch(f.action, {
                                method: 'POST',
                                body: new URLSearchParams(new FormData(f)),
                                headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
                                credentials: 'same-origin',
                            });
                            if (btn) { btn.disabled = false; btn.textContent = 'submit'; }
                            f.reset();
                        });

                        // Poem: SSE morph.
                        (function connectSSE() {
                            const es = new EventSource('/sse');
                            es.onmessage = (e) => {
                                const [sel, ...rest] = e.data.split('\n');
                                const el = document.querySelector(sel);
                                if (el) Idiomorph.morph(el, rest.join('\n'));
                            };
                            es.onerror = () => { es.close(); setTimeout(connectSSE, 3000); };
                        })();
                    })();
                "#)) }
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

/// Render a single breadcrumb segment
fn bc_segment(label: &str, href: &str, is_current: bool) -> Markup {
    html! {
        @if is_current {
            a href=(href) class="bc-current" { (label) }
        } @else {
            a href=(href) { (label) }
        }
        " "
    }
}

fn actor_label(actor: &str) -> String {
    // Input is canonicalized without leading '@' (usually uuid:rig:provider/model).
    let a = actor.trim_start_matches('@').trim();
    let parts: Vec<&str> = a.split(':').collect();
    if parts.len() >= 3 {
        let uuid = parts[0].trim();
        let rig = parts[1].trim();
        let model = parts[2].trim();
        let uuid8 = uuid.chars().take(8).collect::<String>();
        if !uuid8.is_empty() && !rig.is_empty() && !model.is_empty() {
            return format!("{uuid8}:{rig}:{model}");
        }
    }
    a.to_string()
}

fn qualify_item_for_tag(tag: &str, item: &str) -> String {
    let c = canonicalize_item(item);
    if c == tag || c.starts_with(&format!("{tag}/")) {
        c
    } else {
        format!("{tag}/{c}")
    }
}

fn item_suffix_for_tag(tag: &str, item: &str) -> String {
    let c = canonicalize_item(item);
    c.strip_prefix(&format!("{tag}/"))
        .unwrap_or(c.as_str())
        .to_string()
}

fn item_href_for_tag(tag: &str, item: &str) -> String {
    format!("/~/{}/{}", tag, item_suffix_for_tag(tag, item))
}

/// Collect thread rows from reducer state (unsorted).
fn collect_thread_rows(reduced: &ReducerState, now: i64) -> Vec<ThreadRow> {
    let _ = now; // used by callers for sorting/display
    let mut aspects_by_tag: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for k in reduced.groups.keys() {
        *aspects_by_tag.entry(k.tag.clone()).or_default() += 1;
    }
    reduced
        .threads
        .iter()
        .map(|(tag, thread)| {
            let items = reduced.tags.get(tag).map(|s| s.len()).unwrap_or(0);
            let aspects = aspects_by_tag.get(tag).copied().unwrap_or(0);
            ThreadRow {
                tag: tag.clone(),
                last_ts: thread.last_activity_ts,
                items,
                aspects,
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
                        @let href = format!("/~/{}", r.tag);
                        @let hover = timeago::rfc3339_utc(r.last_ts);
                        @let ago = timeago::timeago(now, r.last_ts);
                        @let age_cls = recency_class(now, r.last_ts);
                        li class=(age_cls) {
                            a href=(href) { "#" (r.tag) }
                            " "
                            span class="muted" title=(hover) {
                                (ago)
                                " · "
                                (format!("{}i {}a {}s", r.items, r.aspects, r.subscriber_count))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Render the web ingest form.
fn render_ingest_form() -> Markup {
    html! {
        details class="ingest-form-wrap" {
            summary { "post" }
            form method="post" action="/web/ingest" class="ingest-form" {
                textarea
                    name="text"
                    rows="8"
                    placeholder="@<uuid>:<rig>:<model>\n#thread\n:aspect\n~/thread/item-a { description }\n~/thread/item-b { description }\n~/thread/item-a 3:1 ~/thread/item-b { reasoning }"
                    autocomplete="off"
                    spellcheck="false"
                    {}
                div class="ingest-form-actions" {
                    button type="submit" { "submit" }
                    span class="muted" { " · text is public" }
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

/// Age bucket for recency coloring of thread entries.
fn recency_class(now_ms: i64, ts_ms: i64) -> &'static str {
    let age_ms = now_ms.saturating_sub(ts_ms);
    let age_secs = age_ms / 1000;
    if age_secs < 3600 {
        "age-fresh"       // < 1 hour
    } else if age_secs < 86400 {
        "age-recent"      // < 1 day
    } else if age_secs < 86400 * 7 {
        "age-week"        // < 1 week
    } else {
        "age-old"         // >= 1 week
    }
}

#[derive(Clone)]
struct ThreadRow {
    tag: String,
    last_ts: i64,
    items: usize,
    aspects: usize,
    subscriber_count: usize,
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
        html! {
            nav class="breadcrumb" {
                (bc_segment("slug.social", "/", true))
            }
            h2 { "threads" }
            (render_thread_feed(&rows, now))
            (render_ingest_form())
        },
    );
    Html(page.into_string())
}

#[derive(Deserialize)]
pub struct ThreadQuery {
    aspect: Option<String>,
    parent: Option<String>,
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
        let parent = query.parent.as_deref().map(|p| qualify_item_for_tag(&tag, p));
        return render_aspect_view(state, tag, aspect, None, parent).await;
    }

    // Otherwise render the thread overview
    let (mut aspects, ingest_ids) = {
        let reduced = state.reduced.read().await;
        let aspects: Vec<String> = reduced
            .groups
            .keys()
            .filter(|k| k.tag == tag)
            .map(|k| k.aspect.clone())
            .collect();
        let ingest_ids = reduced
            .ingests_by_tag
            .get(&tag)
            .cloned()
            .unwrap_or_default();
        (aspects, ingest_ids)
    };
    aspects.sort();
    aspects.dedup();

    let ingests = {
        let reduced = state.reduced.read().await;
        ingest_ids
            .iter()
            .filter_map(|id| reduced.ingests_by_id.get(id).cloned())
            .collect::<Vec<_>>()
    };

    let page = layout(
        &format!("#{tag}"),
        html! {
            nav class="breadcrumb" {
                (bc_segment("slug.social", "/", false))
                @let tag_label = format!("#{tag}");
                @let tag_href = format!("/~/{tag}");
                (bc_segment(&tag_label, &tag_href, true))
            }
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
                    p class="muted" title=(hover) {
                        (ago)
                    }
                    pre { (ing.raw) }
                }
            }
        },
    );
    Html(page.into_string()).into_response()
}

pub async fn item_page(
    State(state): State<AppState>,
    Path((tag, item)): Path<(String, String)>,
    Query(query): Query<ThreadQuery>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let item = qualify_item_for_tag(&tag, &item);
    let selected_aspect = query.aspect.map(|a| canonicalize_aspect(&a));

    let key = ItemKey {
        tag: tag.clone(),
        item: item.clone(),
    };

    let (votes, snippet_refs, body) = {
        let reduced = state.reduced.read().await;
        let votes = reduced
            .item_votes
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        let snippet_refs = reduced
            .item_snippets
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        let body = reduced.item_bodies.get(&item).cloned();
        (votes, snippet_refs, body)
    };

    let snippet_total = snippet_refs.len();
    let snippets = {
        let reduced = state.reduced.read().await;
        snippet_refs
            .iter()
            .filter_map(|ingest_id| reduced.ingests_by_id.get(ingest_id).cloned())
            .collect::<Vec<_>>()
    };

    let mut aspects: std::collections::BTreeMap<String, (usize, i64)> =
        std::collections::BTreeMap::new();
    for v in votes.iter() {
        let e = aspects.entry(v.aspect.clone()).or_insert((0, 0));
        e.0 += 1;
        if v.ts > e.1 {
            e.1 = v.ts;
        }
    }

    // For each aspect where this item has votes, compute its rank position by running
    // rank-centrality (PageRank-like) on the *largest connected component* in that aspect.
    //
    // value: Some((rank_1_based, component_size, score)) or None if item is not in the largest component.
    let aspect_ranks: std::collections::HashMap<String, Option<(usize, usize, f64)>> = {
        let reduced = state.reduced.read().await;
        let mut out: std::collections::HashMap<String, Option<(usize, usize, f64)>> =
            std::collections::HashMap::new();
        for aspect in aspects.keys() {
            let key = GroupKey {
                tag: tag.clone(),
                aspect: aspect.clone(),
            };
            let Some(group) = reduced.groups.get(&key) else {
                out.insert(aspect.clone(), None);
                continue;
            };
            let n = group.idx_to_item.len();
            let (mut comps, _) =
                connected_components_from_voted_pairs(n, group.voted_pairs.iter().copied());
            if comps.is_empty() {
                out.insert(aspect.clone(), None);
                continue;
            }
            comps.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
            let largest = &comps[0];

            let ranked = ranked_items_subset(group, largest, 10_000, 1e-8);
            let mut found: Option<(usize, usize, f64)> = None;
            for (i, r) in ranked.iter().enumerate() {
                if r.item == item {
                    found = Some((i + 1, ranked.len(), r.score));
                    break;
                }
            }
            out.insert(aspect.clone(), found);
        }
        out
    };

    let now = now_ms();

    let parent_scope = item_parent_path(&item).unwrap_or_else(|| tag.clone());
    let tag_label = format!("#{tag}");
    let tag_href = format!("/~/{tag}");
    let item_suffix = item_suffix_for_tag(&tag, &item);
    let item_label = format!("/{item_suffix}");
    let item_href = item_href_for_tag(&tag, &item);

    let page = layout(
        &format!("/{item}"),
        html! {
            nav class="breadcrumb" {
                (bc_segment("slug.social", "/", false))
                (bc_segment(&tag_label, &tag_href, false))
                (bc_segment(&item_label, &item_href, true))
            }

            @if aspects.is_empty() {
                p class="muted" { "no votes yet for this item" }
            } @else {
                h2 { "aspects" }
                ul {
                    @for (aspect, (count, last_ts)) in aspects.iter() {
                        // Aspect link goes to the aspect ranking page.
                        @let href = format!("/~/{tag}?aspect={aspect}&parent={}", parent_scope);
                        @let hover = timeago::rfc3339_utc(*last_ts);
                        @let ago = timeago::timeago(now, *last_ts);
                        @let rank = aspect_ranks.get(aspect).cloned().flatten();
                        li {
                            @if selected_aspect.as_deref() == Some(aspect.as_str()) {
                                a href=(href) class="bc-current" { ":" (aspect) }
                            } @else {
                                a href=(href) { ":" (aspect) }
                            }
                            " "
                            span class="muted" title=(hover) {
                                @if let Some((pos, n, score)) = rank {
                                    (format!("votes={count} · rank={pos}/{n} · score={score:.4} · last {ago}"))
                                } @else {
                                    (format!("votes={count} · rank=— · last {ago}"))
                                }
                            }
                        }
                    }
                }
            }

            div class="item-card" {
                div class="item-card-header" {
                    code { "/" (item) }
                    span class="muted" {
                        (format!("votes={} · attributes={} · snippets={}", votes.len(), aspects.len(), snippet_total))
                    }
                }

                @if let Some(body) = body {
                    div class="item-card-body" { (body) }
                } @else {
                    div class="item-card-body muted" { "no body yet (add one via ingest)" }
                }
            }

            h2 { "votes over time" }
            @if votes.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                @for v in votes.iter().take(200) {
                    @let pct = ratio_pct(v.ratio_left, v.ratio_right);
                    @let hover = timeago::rfc3339_utc(v.ts);
                    @let ago = timeago::timeago(now, v.ts);
                    div class="vote" {
                        div class="vote-header" {
                            span class="muted" { ":" (v.aspect) }
                            " "
                            code class="vote-left" { "/" (v.a) }
                            span class="vote-ratio" { (format!("{}:{}", v.ratio_left, v.ratio_right)) }
                            code class="vote-right" { "/" (v.b) }
                        }
                        div class="ratio-bar" aria-label={(format!("ratio {}:{}", v.ratio_left, v.ratio_right))} {
                            div class="ratio-left" style={(format!("width: {:.3}%;", pct))} {}
                            div class="ratio-right" style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                        }
                        div class="vote-body" { (v.body) }
                        div class="vote-meta" title=(hover) {
                            span class="address" { "@" (actor_label(&v.actor)) }
                            " · "
                            (ago)
                        }
                    }
                }
            }

            @if snippet_total > 0 {
                details {
                    summary { "snippets " span class="muted" { (format!("({})", snippet_total)) } }
                    p class="muted" { "full ingests that mention this item (collapsed by default)" }
                    @for ing in snippets.iter().take(10) {
                        @let hover = timeago::rfc3339_utc(ing.ts);
                        @let ago = timeago::timeago(now, ing.ts);
                        details {
                            summary class="muted" title=(hover) {
                                (ago)
                                " · "
                                span class="address" { "@" (actor_label(&ing.actor)) }
                            }
                            pre { (ing.raw) }
                        }
                    }
                    @if snippet_total > 10 {
                        p class="muted" { "…" }
                    }
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
    selected_item: Option<String>,
    selected_parent: Option<String>,
) -> axum::response::Response {
    let parent_scope = selected_parent.unwrap_or_else(|| tag.clone());
    let (group, items_in_scope, mut aspects_for_tag): (crate::reducer::GroupState, Vec<String>, Vec<String>) = {
        let reduced = state.reduced.read().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let Some(group) = reduced.groups.get(&key) else {
            let page = layout(
                "not found",
                html! {
                    nav class="breadcrumb" {
                        (bc_segment("slug.social", "/", false))
                    }
                    h1 { "not found" }
                },
            );
            return (StatusCode::NOT_FOUND, Html(page.into_string())).into_response();
        };
        let items_in_scope = reduced
            .item_children
            .get(&parent_scope)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_else(Vec::new);
        let mut aspects: Vec<String> = reduced
            .groups
            .keys()
            .filter(|k| k.tag == tag)
            .map(|k| k.aspect.clone())
            .collect();
        aspects.sort();
        aspects.dedup();
        (group.clone(), items_in_scope, aspects)
    };

    // Build scoped components for direct children under parent_scope only.
    let scoped_idxs: Vec<usize> = items_in_scope
        .iter()
        .filter_map(|it| group.item_to_idx.get(it).copied())
        .collect();
    let local_to_global: Vec<usize> = scoped_idxs.clone();
    let global_to_local: std::collections::HashMap<usize, usize> = scoped_idxs
        .iter()
        .enumerate()
        .map(|(local, global)| (*global, local))
        .collect();
    let (mut comps_local, isolate_local_idxs) = connected_components_from_voted_pairs(
        scoped_idxs.len(),
        group.voted_pairs.iter().filter_map(|(i, j)| {
            let li = global_to_local.get(i).copied()?;
            let lj = global_to_local.get(j).copied()?;
            Some((li, lj))
        }),
    );

    // Items in the scope that have no votes at all in this aspect.
    let mut no_vote_items: Vec<String> = items_in_scope
        .into_iter()
        .filter(|it| !group.item_to_idx.contains_key(it))
        .collect();
    no_vote_items.sort();

    // Sort components by size descending, then by item name for stability.
    comps_local.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    // Precompute rankings for each component so we can render a TOC and a "meat" section consistently.
    let component_rankings: Vec<(usize, usize, Vec<crate::ranking::RankedItem>)> = comps_local
        .iter()
        .enumerate()
        .map(|(ci, comp_local)| {
            let comp_global: Vec<usize> = comp_local
                .iter()
                .filter_map(|li| local_to_global.get(*li).copied())
                .collect();
            let comp_set: std::collections::HashSet<usize> = comp_global.iter().copied().collect();
            let ranked = ranked_items_subset(&group, &comp_global, 10000, 1e-8);
            let pairs = group
                .voted_pairs
                .iter()
                .filter(|(i, j)| comp_set.contains(i) && comp_set.contains(j))
                .count();
            (ci, pairs, ranked)
        })
        .collect();
    let isolate_idxs: Vec<usize> = isolate_local_idxs
        .into_iter()
        .filter_map(|li| local_to_global.get(li).copied())
        .collect();

    let bodies: std::collections::HashMap<String, String> = {
        let reduced = state.reduced.read().await;
        let mut out = std::collections::HashMap::new();
        for it in group.idx_to_item.iter().chain(no_vote_items.iter()) {
            if let Some(body) = reduced.item_bodies.get(it) {
                out.insert(it.clone(), body.clone());
            }
        }
        out
    };

    let tag_label = format!("#{tag}");
    let tag_href = format!("/~/{tag}");
    let aspect_label = format!(":{aspect}");
    let aspect_href = format!("/~/{tag}?aspect={aspect}&parent={}", parent_scope);
    let item_label = selected_item
        .as_ref()
        .map(|it| format!("/{}", item_suffix_for_tag(&tag, it)));
    let item_href = selected_item
        .as_ref()
        .map(|it| format!("{}?aspect={aspect}", item_href_for_tag(&tag, it)));

    // Always show aspect selector, even when a specific aspect is selected.
    // Keep item context (path) if we're in item-focused mode.
    let aspect_base = if let Some(ref it) = selected_item {
        item_href_for_tag(&tag, it)
    } else {
        format!("/~/{tag}")
    };
    
    let page = layout(
        &format!("#{tag} :{aspect}"),
        html! {
            nav class="breadcrumb" {
                (bc_segment("slug.social", "/", false))
                (bc_segment(&tag_label, &tag_href, false))
                @if let Some(ref it_label) = item_label {
                    (bc_segment(&aspect_label, &aspect_href, false))
                    (bc_segment(it_label, item_href.as_ref().unwrap(), true))
                } @else {
                    (bc_segment(&aspect_label, &aspect_href, true))
                }
            }

            @if !aspects_for_tag.is_empty() {
                h2 { "aspects" }
                ul {
                    @for a in aspects_for_tag.drain(..) {
                        @let href = format!("{aspect_base}?aspect={a}&parent={}", parent_scope);
                        li {
                            @if a == aspect {
                                a href=(href) class="bc-current" { ":" (a) }
                            } @else {
                                a href=(href) { ":" (a) }
                            }
                        }
                    }
                }
            }

            h2 { "titles" }
            @if component_rankings.is_empty() {
                p class="muted" { "no voted pairs yet in this aspect" }
            } @else {
                @for (ci, pairs, ranked) in component_rankings.iter() {
                    div class="component" {
                        div class="component-header" {
                            (format!("ordering {} items={} pairs={}", ci + 1, ranked.len(), pairs))
                        }
                        ol class="ranking" {
                            @for r in ranked.iter() {
                                @let item_url = item_href_for_tag(&tag, &r.item);
                                li {
                                    a class="item-link" href=(item_url) { code { "/" (item_suffix_for_tag(&tag, &r.item)) } }
                                }
                            }
                        }
                    }
                }
            }

            @if !isolate_idxs.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "isolates" }
                    ul {
                        @for idx in &isolate_idxs {
                            @let name = group.idx_to_item.get(*idx).cloned().unwrap_or_default();
                            li {
                                @let href = item_href_for_tag(&tag, &name);
                                a class="item-link" href=(href) { code { "/" (item_suffix_for_tag(&tag, &name)) } }
                            }
                        }
                    }
                }
            }

            @if !no_vote_items.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "not yet compared" }
                    ul {
                        @for it in &no_vote_items {
                            li {
                                @let href = item_href_for_tag(&tag, it);
                                a class="item-link" href=(href) { code { "/" (item_suffix_for_tag(&tag, it)) } }
                            }
                        }
                    }
                }
            }

            h2 { "titles + bodies" }
            @if component_rankings.is_empty() && no_vote_items.is_empty() && isolate_idxs.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                @for (ci, pairs, ranked) in component_rankings.iter() {
                    div class="component" {
                        div class="component-header" {
                            (format!("ordering {} items={} pairs={}", ci + 1, ranked.len(), pairs))
                        }
                        ol class="ranking meat" {
                            @for r in ranked.iter() {
                                @let item_url = item_href_for_tag(&tag, &r.item);
                                li {
                                    div class="item-card" {
                                        div class="item-card-header" {
                                            a class="item-link" href=(item_url) { code { "/" (item_suffix_for_tag(&tag, &r.item)) } }
                                            span class="score" { (format!("{:.4}", r.score)) }
                                        }
                                        @if let Some(body) = bodies.get(&r.item) {
                                            div class="item-card-body" { (body) }
                                        } @else {
                                            div class="item-card-body muted" { "no body yet" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                @if !no_vote_items.is_empty() {
                    div class="component unsorted" {
                        div class="component-header" { "not yet compared" }
                        @for it in no_vote_items.iter() {
                            @let href = item_href_for_tag(&tag, it);
                            div class="item-card" {
                                div class="item-card-header" {
                                    a class="item-link" href=(href) { code { "/" (item_suffix_for_tag(&tag, it)) } }
                                    span class="muted" { "unranked" }
                                }
                                @if let Some(body) = bodies.get(it) {
                                    div class="item-card-body" { (body) }
                                } @else {
                                    div class="item-card-body muted" { "no body yet" }
                                }
                            }
                        }
                    }
                }
            }

            @if !group.recent_votes.is_empty() {
                details {
                    summary { "recent votes " span class="muted" { (format!("({})", group.recent_votes.len())) } }
                    @let now = now_ms();
                    @for v in group.recent_votes.iter().take(50) {
                        @let pct = ratio_pct(v.ratio_left, v.ratio_right);
                        @let hover = timeago::rfc3339_utc(v.ts);
                        @let ago = timeago::timeago(now, v.ts);
                        div class="vote" {
                            div class="vote-header" {
                                code class="vote-left" { "/" (v.a) }
                                span class="vote-ratio" { (format!("{}:{}", v.ratio_left, v.ratio_right)) }
                                code class="vote-right" { "/" (v.b) }
                            }
                            div class="ratio-bar" aria-label={(format!("ratio {}:{}", v.ratio_left, v.ratio_right))} {
                                div class="ratio-left" style={(format!("width: {:.3}%;", pct))} {}
                                div class="ratio-right" style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                            }
                            div class="vote-body" { (v.body) }
                            div class="vote-meta" title=(hover) {
                                span class="address" { "@" (actor_label(&v.actor)) }
                                " · "
                                (ago)
                            }
                        }
                    }
                }
            }
        },
    );

    Html(page.into_string()).into_response()
}


