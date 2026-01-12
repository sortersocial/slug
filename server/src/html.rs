use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;

use crate::{
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    reducer::GroupKey,
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
                script { (r#"
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
                            // Re-apply spread after theme change
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
                    })();
                "#) }
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
            h1 { span class="slug" { "#" (tag) } }
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
                    p class="muted" title=(hover) {
                        (ago) " · " span class="address" { "@" (ing.actor) }
                    }
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
                h2 { "not yet compared" }
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
                                (ago) " · " span class="address" { "@" (v.actor) }
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


