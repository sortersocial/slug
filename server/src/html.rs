use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;

use crate::{
    events::{canonicalize_aspect, canonicalize_item, canonicalize_tag},
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    reducer::{GroupKey, ItemKey},
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
        let rig = parts[1].trim();
        let model = parts[2].trim();
        if !rig.is_empty() && !model.is_empty() {
            return format!("{rig}:{model}");
        }
    }
    a.to_string()
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
                .and_then(|ing_id| reduced.ingests_by_id.get(ing_id).map(|ing| ing.ts))
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
            nav class="breadcrumb" {
                (bc_segment("slug.social", "/", true))
            }
            h2 { "threads" }
            @if rows.is_empty() {
                p class="muted" { "no threads yet" }
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
                                (format!("items={} aspects={} recent_votes={}", r.items, r.aspects, r.recent_votes))
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
        return render_aspect_view(state, tag, aspect, None).await;
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
    let item = canonicalize_item(&item);

    if let Some(aspect) = query.aspect {
        let aspect = canonicalize_aspect(&aspect);
        return render_aspect_view(state, tag, aspect, Some(item)).await;
    }

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

    let tag_label = format!("#{tag}");
    let tag_href = format!("/~/{tag}");
    let item_label = format!("/{item}");
    let item_href = format!("/~/{tag}/{item}");

    let page = layout(
        &format!("/{item}"),
        html! {
            nav class="breadcrumb" {
                (bc_segment("slug.social", "/", false))
                (bc_segment(&tag_label, &tag_href, false))
                (bc_segment(&item_label, &item_href, true))
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

            h2 { "attributes" }
            @if aspects.is_empty() {
                p class="muted" { "no votes yet for this item" }
            } @else {
                ul {
                    @for (aspect, (count, last_ts)) in aspects.iter() {
                        @let href = format!("/~/{tag}/{item}?aspect={aspect}");
                        @let hover = timeago::rfc3339_utc(*last_ts);
                        @let ago = timeago::timeago(now, *last_ts);
                        @let rank = aspect_ranks.get(aspect).cloned().flatten();
                        li {
                            a href=(href) { ":" (aspect) }
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
) -> axum::response::Response {
    let (group, items_in_tag, mut aspects_for_tag): (crate::reducer::GroupState, Vec<String>, Vec<String>) = {
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
        let items_in_tag = reduced
            .tags
            .get(&tag)
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
        (group.clone(), items_in_tag, aspects)
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

    let tag_label = format!("#{tag}");
    let tag_href = format!("/~/{tag}");
    let aspect_label = format!(":{aspect}");
    let aspect_href = format!("/~/{tag}?aspect={aspect}");
    let item_label = selected_item.as_ref().map(|it| format!("/{it}"));
    let item_href = selected_item.as_ref().map(|it| format!("/~/{tag}/{it}?aspect={aspect}"));

    // Always show aspect selector, even when a specific aspect is selected.
    // Keep item context (path) if we're in item-focused mode.
    let aspect_base = if let Some(ref it) = selected_item {
        format!("/~/{tag}/{it}")
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
                ul {
                    @for a in aspects_for_tag.drain(..) {
                        @let href = format!("{aspect_base}?aspect={a}");
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

            h2 { "orderings" }
            @if comps.is_empty() {
                p class="muted" { "no voted pairs yet in this aspect" }
            } @else {
                @for (ci, comp) in comps.iter().enumerate() {
                    @let ranked = ranked_items_subset(&group, comp, 10000, 1e-8);
                    @let pairs = group.voted_pairs.iter().filter(|(i,j)| comp.binary_search(i).is_ok() && comp.binary_search(j).is_ok()).count();
                    div class="component" {
                        div class="component-header" {
                            (format!("ordering {} items={} pairs={}", ci + 1, comp.len(), pairs))
                        }
                        ol class="ranking" {
                            @for r in ranked.iter() {
                                @let is_selected = selected_item.as_ref() == Some(&r.item);
                                // Item slug links to the item page (not the aspect view).
                                // The "selected" aspect/item state is represented in breadcrumbs.
                                @let item_url = format!("/~/{tag}/{}", r.item);
                                li {
                                    a class="item-link" href=(item_url) {
                                        code { "/" (r.item) }
                                    }
                                    span class="score" { (format!("{:.4}", r.score)) }
                                }
                                @if is_selected {
                                    div class="item-card" {
                                        div class="item-card-header" {
                                            code { "/" (r.item) }
                                            span class="score" { (format!("{:.4}", r.score)) }
                                        }
                                        div class="item-card-body" {
                                            "/" (r.item)
                                        }
                                    }
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
                        @for idx in isolate_idxs {
                            @let name = group.idx_to_item.get(idx).cloned().unwrap_or_default();
                            li {
                                @let href = format!("/~/{tag}/{name}");
                                a class="item-link" href=(href) { code { "/" (name) } }
                            }
                        }
                    }
                }
            }

            @if !no_vote_items.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "not yet compared" }
                    ul {
                        @for it in no_vote_items {
                            li {
                                @let href = format!("/~/{tag}/{it}");
                                a class="item-link" href=(href) { code { "/" (it) } }
                            }
                        }
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
        },
    );

    Html(page.into_string()).into_response()
}


