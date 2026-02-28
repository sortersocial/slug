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
    reducer::{GroupKey, ReducerState},
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

fn layout(title: &str, view: &str, body: Markup) -> Markup {
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
            body class=(view) {
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

/// Escape HTML special chars for safe injection.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Replace ~/path slugs in raw text with clickable links. Links are styled to match
/// surrounding text (no visual difference). Path chars: alphanumeric, _, -, /.
fn linkify_slugs(raw: &str) -> String {
    let escaped = escape_html(raw);
    let mut out = String::with_capacity(escaped.len() + 64);
    let mut i = 0;
    let s = escaped.as_str();
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("~/") {
            let path_len = rest[2..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '/')
                .map(|c| c.len_utf8())
                .sum::<usize>();
            if path_len > 0 {
                let path = &rest[2..2 + path_len];
                out.push_str(r#"<a href="/~/"#);
                out.push_str(path);
                out.push_str(r#"" class="pre-link">~/"#);
                out.push_str(path);
                out.push_str("</a>");
                i += 2 + path_len;
                continue;
            }
        }
        if let Some((j, c)) = rest.char_indices().next() {
            out.push(c);
            i += j + c.len_utf8();
        } else {
            break;
        }
    }
    out
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

fn with_query_param(url: &str, key: &str, value: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{key}={value}")
}

fn with_aspect(url: &str, aspect: Option<&str>) -> String {
    match aspect {
        Some(a) => with_query_param(url, "aspect", a),
        None => url.to_string(),
    }
}

/// Render a single breadcrumb segment with `/` separator.
fn bc_segment(label: &str, href: &str, is_current: bool) -> Markup {
    html! {
        span class="bc-sep" { " / " }
        @if is_current {
            a href=(href) class="bc-current" { (label) }
        } @else {
            a href=(href) { (label) }
        }
    }
}

/// Render the ontology path breadcrumb: `slug.social / ~ / ns`.
fn bc_ontology(ns: &str, aspect: Option<&str>) -> Markup {
    let root_href = with_aspect("/~", aspect);
    let ns_href = with_aspect(&format!("/~/{ns}"), aspect);
    html! {
        a href="/" { "slug.social" }
        (bc_segment("~", &root_href, false))
        (bc_segment(ns, &ns_href, true))
    }
}

/// Breadcrumb for any canonical path, e.g. "parables" or "parables/counting-the-cost".
fn bc_path(path: &str) -> Markup {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let ns = segs.first().copied().unwrap_or(path);
    let thread_href = format!("/t/{ns}");
    html! {
        a href="/" { "slug.social" }
        (bc_segment("~", "/~", false))
        @for (i, seg) in segs.iter().enumerate() {
            @let href = format!("/~/{}", segs[..=i].join("/"));
            @let is_last = i == segs.len() - 1;
            (bc_segment(seg, &href, is_last))
        }
        span class="bc-side" {
            a href=(thread_href) { "#" (ns) }
        }
    }
}

/// Render the thread path breadcrumb: `slug.social / #tag`
/// with an optional side-link to the ontology root for this tag.
fn bc_thread(tag: &str, side_ontology: bool) -> Markup {
    let thread_href = format!("/t/{tag}");
    let ontology_href = format!("/~/{tag}");
    html! {
        a href="/" { "slug.social" }
        (bc_segment(&format!("#{tag}"), &thread_href, true))
        @if side_ontology {
            span class="bc-side" {
                a href=(ontology_href) { "~" }
            }
        }
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


/// The display path of an item relative to its namespace root (strips `ns/` prefix).
fn item_display_path(ns: &str, item: &str) -> String {
    let c = canonicalize_item(item);
    c.strip_prefix(&format!("{ns}/"))
        .unwrap_or(c.as_str())
        .to_string()
}

/// The URL for an item under `/~`.
fn item_href(ns: &str, item: &str, aspect: Option<&str>) -> String {
    with_aspect(&format!("/~/{}/{}", ns, item_display_path(ns, item)), aspect)
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
                        @let garden_href = format!("/~/{}", r.tag);
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
                            " "
                            a href=(garden_href) class="muted" { "~" }
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
    ingests: usize,
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
        "view-thread",
        html! {
            nav class="breadcrumb" {
                a href="/" class="bc-current" { "slug.social" }
                span class="bc-side" { a href="/~" { "~" } }
            }
            h2 { "threads" }
            (render_thread_feed(&rows, now))
            (render_ingest_form())
        },
    );
    Html(page.into_string())
}

/// Ontology index — root-level items (children of ""), light view.
/// Requires ?aspect= to show content; without it, only the aspect selector menu is shown.
pub async fn garden_index(
    State(state): State<AppState>,
    Query(query): Query<ThreadQuery>,
) -> impl IntoResponse {
    let aspects = all_aspects(&state).await;
    let selected_aspect = query.aspect.map(|a| canonicalize_aspect(&a));

    let roots: Vec<(String, usize)> = if selected_aspect.is_some() {
        let reduced = state.reduced.read().await;
        let mut v: Vec<(String, usize)> = reduced
            .item_children
            .get("")
            .map(|s| {
                s.iter().map(|path| {
                    let children = reduced.item_children.get(path).map(|c| c.len()).unwrap_or(0);
                    (path.clone(), children)
                }).collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    } else {
        vec![]
    };

    let page = layout(
        "~/",
        "view-ontology",
        html! {
            (render_ontology_header(&aspects, "/~", selected_aspect.as_deref()))
            @if selected_aspect.is_some() {
                nav class="breadcrumb" {
                    a href="/" { "slug.social" }
                    (bc_segment("~", &with_aspect("/~", selected_aspect.as_deref()), true))
                }
                h2 { "paths" }
                @if roots.is_empty() {
                    p class="muted" { "no items yet" }
                } @else {
                    ul {
                        @for (path, children) in &roots {
                            @let href = with_aspect(&format!("/~/{}", path), selected_aspect.as_deref());
                            li {
                                a href=(href) { "~/" (path) }
                                " "
                                span class="muted" { (format!("{}c", children)) }
                            }
                        }
                    }
                }
            } @else if aspects.is_empty() {
                p class="muted" { "no aspects yet — add votes to create aspects" }
            }
        },
    );
    Html(page.into_string())
}

#[derive(Deserialize)]
pub struct ThreadQuery {
    aspect: Option<String>,
    parent: Option<String>,
}

/// All aspects in the system (from ranking groups).
async fn all_aspects(state: &AppState) -> Vec<String> {
    let reduced = state.reduced.read().await;
    let mut a: Vec<String> = reduced.groups.keys().map(|k| k.aspect.clone()).collect();
    a.sort();
    a
}

/// Ontology header: aspect selector (always first, global). Renders a select that navigates on change.
fn render_ontology_header(aspects: &[String], base_url: &str, current: Option<&str>) -> Markup {
    html! {
        div class="ontology-header" {
            label for="ontology-aspect-select" { "aspect" }
            select id="ontology-aspect-select" onchange="if(this.value){window.location.href=this.value}" {
                @if aspects.is_empty() {
                    option value="" disabled selected { "no aspects yet" }
                } @else {
                    @for a in aspects {
                        @let href = with_aspect(base_url, Some(a.as_str()));
                        option value=(href) selected[current == Some(a.as_str())] { ":" (a) }
                    }
                }
            }
        }
    }
}

/// Thread view — `/t/:tag` — dark, recent ingests only.
pub async fn thread_view(
    State(state): State<AppState>,
    Path(tag): Path<String>,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);

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

    let now = now_ms();

    let page = layout(
        &format!("#{tag}"),
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_thread(&tag, true)) }
            h2 { "#" (tag) }
            @if ingests.is_empty() {
                p class="muted" { "no activity yet" }
            } @else {
                @for ing in ingests.iter().rev().take(50) {
                    @let hover = timeago::rfc3339_utc(ing.ts);
                    @let ago = timeago::timeago(now, ing.ts);
                    div class="ingest-entry" {
                        div class="ingest-meta muted" title=(hover) {
                            span class="address" { "@" (actor_label(&ing.actor)) }
                            " · "
                            (ago)
                        }
                        pre { (maud::PreEscaped(linkify_slugs(&ing.raw))) }
                    }
                }
            }
            (render_ingest_form())
        },
    );
    Html(page.into_string()).into_response()
}

/// Single public handler for all `/~/*path` routes.
pub async fn ontology_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(query): Query<ThreadQuery>,
) -> impl IntoResponse {
    let path = canonicalize_item(&path);
    let selected_aspect = query.aspect.map(|a| canonicalize_aspect(&a));
    render_path_view(state, path, selected_aspect).await
}

/// Generic tree view for any path under /~. Works identically at every depth.
/// Requires ?aspect= to show content; without it, only the aspect selector menu is shown.
/// With ?aspect=X delegates to the ranking view scoped to this subtree.
async fn render_path_view(
    state: AppState,
    path: String,
    selected_aspect: Option<String>,
) -> axum::response::Response {
    let ns = path.split('/').next().unwrap_or(&path).to_string();

    // ?aspect=X: ranking view scoped to this path's subtree.
    if let Some(aspect) = selected_aspect.clone() {
        let parent_scope = Some(path.clone());
        return render_aspect_view(state, ns, aspect, None, parent_scope).await;
    }

    // No aspect: show only aspect selector (menu).
    let aspects = all_aspects(&state).await;
    let base_url = format!("/~/{path}");
    let page = layout(
        &format!("~/{path}"),
        "view-ontology",
        html! {
            (render_ontology_header(&aspects, &base_url, None))
            @if aspects.is_empty() {
                p class="muted" { "no aspects yet — add votes to create aspects" }
            }
        },
    );
    return Html(page.into_string()).into_response();
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
        let key = GroupKey { aspect: aspect.clone() };
        let Some(group) = reduced.groups.get(&key) else {
            let page = layout(
                "not found",
                "view-ontology",
                html! {
                    nav class="breadcrumb" {
                        a href="/" { "slug.social" }
                        (bc_segment("~", "/~", false))
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
        // Aspects available on items under this parent scope.
        let mut aspects: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for it in &items_in_scope {
            if let Some(votes) = reduced.item_votes.get(it) {
                for v in votes {
                    aspects.insert(v.aspect.clone());
                }
            }
        }
        (group.clone(), items_in_scope, aspects.into_iter().collect())
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

    let aspect_base = if let Some(ref it) = selected_item {
        item_href(&tag, it, None)
    } else {
        format!("/~/{tag}")
    };

    let page = layout(
        &format!("~/{tag} :{aspect}"),
        "view-ontology",
        html! {
            nav class="breadcrumb" {
                (bc_ontology(&tag, Some(&aspect)))
            }

            @if !aspects_for_tag.is_empty() {
                h2 { "aspects" }
                ul {
                    @for a in aspects_for_tag.drain(..) {
                        @let href = with_query_param(&with_aspect(&aspect_base, Some(&a)), "parent", &parent_scope);
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
                div class="component-groups" {
                    @for (ci, pairs, ranked) in component_rankings.iter() {
                        div class="component" {
                            div class="component-header" {
                                span class="component-index" { (format!("{}.", ci + 1)) }
                                span class="component-meta" { (format!("items={} pairs={}", ranked.len(), pairs)) }
                            }
                            ol class="ranking" {
                                @for r in ranked.iter() {
                                    @let item_url = item_href(&tag, &r.item, Some(&aspect));
                                    li {
                                        a class="item-link" href=(item_url) { code { "/" (item_display_path(&tag, &r.item)) } }
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
                        @for idx in &isolate_idxs {
                            @let name = group.idx_to_item.get(*idx).cloned().unwrap_or_default();
                            li {
                                @let href = item_href(&tag, &name, Some(&aspect));
                                a class="item-link" href=(href) { code { "/" (item_display_path(&tag, &name)) } }
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
                                @let href = item_href(&tag, it, Some(&aspect));
                                a class="item-link" href=(href) { code { "/" (item_display_path(&tag, it)) } }
                            }
                        }
                    }
                }
            }

            h2 { "titles + bodies" }
            @if component_rankings.is_empty() && no_vote_items.is_empty() && isolate_idxs.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                div class="component-groups" {
                    @for (ci, pairs, ranked) in component_rankings.iter() {
                        div class="component" {
                            div class="component-header" {
                                span class="component-index" { (format!("{}.", ci + 1)) }
                                span class="component-meta" { (format!("items={} pairs={}", ranked.len(), pairs)) }
                            }
                            ol class="ranking meat" {
                                @for r in ranked.iter() {
                                    @let item_url = item_href(&tag, &r.item, Some(&aspect));
                                    li {
                                        div class="item-card" {
                                            div class="item-card-header" {
                                                a class="item-link" href=(item_url) { code { "/" (item_display_path(&tag, &r.item)) } }
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
                }                

                @if !no_vote_items.is_empty() {
                    div class="component unsorted" {
                        div class="component-header" { "not yet compared" }
                        @for it in no_vote_items.iter() {
                            @let href = item_href(&tag, it, Some(&aspect));
                            div class="item-card" {
                                div class="item-card-header" {
                                    a class="item-link" href=(href) { code { "/" (item_display_path(&tag, it)) } }
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
                        @let current = selected_item.as_ref();
                        div class="vote" {
                            div class="vote-header" {
                                a class="item-link" href=(with_aspect(&format!("/~/{}", v.a), Some(&aspect))) { code class="vote-left" { "/" (v.a) } }
                                span class="vote-ratio" { (format!("{}:{}", v.ratio_left, v.ratio_right)) }
                                a class="item-link" href=(with_aspect(&format!("/~/{}", v.b), Some(&aspect))) { code class="vote-right" { "/" (v.b) } }
                            }
                            div class="ratio-bar" aria-label={(format!("ratio {}:{}", v.ratio_left, v.ratio_right))} {
                                @let left_class = format!("ratio-left{}", if current == Some(&v.a) { " current" } else { "" });
                                @let right_class = format!("ratio-right{}", if current == Some(&v.b) { " current" } else { "" });
                                div class=(left_class) style={(format!("width: {:.3}%;", pct))} {}
                                div class=(right_class) style={(format!("width: {:.3}%;", 100.0 - pct))} {}
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


