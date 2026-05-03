use axum::{
    extract::{Query, State},
    http::{HeaderMap, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup, PreEscaped};
use serde::Deserialize;

use crate::{
    api::optional_principal,
    events::ThreadCapability,
    middleware::canonical_view_url,
    reducer::{ReducerState, ScopeId},
    state::AppState,
    timeago,
};

use super::{authorship_address, bc_segment, cli_panel, layout, now_ms, theme_from_jar, theme_next_from_uri};

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

// --- View structs for template rendering ---

struct ItemRow {
    path: String,
    body: Option<String>,
}

struct ThreadRow {
    tag: String,
    subtitle: Option<String>,
    post_count: usize,
    last_activity: i64,
}

struct PostRow {
    thread: String,
    /// Pre-formatted attribution string for display (includes `@` / `@@` from `authorship_address`).
    actor_display: String,
    text: String,
    ts: i64,
}

// --- Search logic ---

fn tokenize(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn contains_all(text: &str, words: &[String]) -> bool {
    let lower = text.to_lowercase();
    words.iter().all(|w| lower.contains(w.as_str()))
}

fn contains_any(text: &str, words: &[String]) -> usize {
    let lower = text.to_lowercase();
    words.iter().filter(|w| lower.contains(w.as_str())).count()
}

struct SearchResults {
    items: Vec<ItemRow>,
    threads: Vec<ThreadRow>,
    posts: Vec<PostRow>,
}

fn can_view_scope(state: &ReducerState, scope: &ScopeId, principal: Option<&str>) -> bool {
    match scope {
        ScopeId::Public => true,
        ScopeId::Room(room_id) => principal.is_some_and(|u| state.user_has_cap(room_id, u, ThreadCapability::View)),
    }
}

fn search(state: &ReducerState, q: &str, limit: usize, principal: Option<&str>) -> SearchResults {
    let words = tokenize(q);
    if words.is_empty() {
        return SearchResults { items: vec![], threads: vec![], posts: vec![] };
    }

    let public = state.public();
    let mut scored_items: Vec<(u32, ItemRow)> = Vec::new();
    let mut scored_threads: Vec<(u32, ThreadRow)> = Vec::new();
    let mut scored_posts: Vec<(u32, PostRow)> = Vec::new();

    // Search items (paths + bodies)
    for item in &public.items {
        let mut score: u32 = 0;
        let path_lower = item.as_str().to_lowercase();
        if contains_all(&path_lower, &words) {
            score += 10;
        } else if contains_any(&path_lower, &words) > 0 {
            score += 5;
        }
        if let Some(body) = public.item_bodies.get(item) {
            if contains_all(body, &words) {
                score += 6;
            } else {
                let any = contains_any(body, &words);
                if any > 0 {
                    score += any as u32;
                }
            }
        }
        if score > 0 {
            scored_items.push((score, ItemRow {
                path: item.as_str().to_string(),
                body: public.item_bodies.get(item).cloned(),
            }));
        }
    }

    // Search threads (shared site scope only — `room` wire `public`)
    for ((scope, tag), thread_state) in &state.forum_threads {
        if scope != &ScopeId::Public {
            continue;
        }
        let mut score: u32 = 0;
        if contains_all(tag, &words) {
            score += 8;
        } else if contains_any(tag, &words) > 0 {
            score += 4;
        }
        if score > 0 {
            let post_count = state
                .ingests_by_scope_thread
                .get(&(ScopeId::Public, tag.clone()))
                .map(|q| q.len())
                .unwrap_or(0);
            scored_threads.push((score, ThreadRow {
                tag: tag.clone(),
                subtitle: None,
                post_count,
                last_activity: thread_state.last_activity_ts,
            }));
        }
    }

    // Search posts
    for (id, ingest) in &state.ingests_by_id {
        let mut score: u32 = 0;
        if contains_all(&ingest.raw, &words) {
            score += 4;
        } else {
            let any = contains_any(&ingest.raw, &words);
            if any > 0 {
                score += any as u32;
            }
        }
        if score > 0 {
            let Some((scope, tag)) = state
                .ingests_by_scope_thread
                .iter()
                .find_map(|((scope, tag), ids)| {
                    if ids.contains(id) {
                        Some((scope.clone(), tag.clone()))
                    } else {
                        None
                    }
                }) else {
                    continue;
                };
            if !can_view_scope(state, &scope, principal) {
                continue;
            }
            let thread = match scope {
                ScopeId::Public => format!("#{tag}"),
                ScopeId::Room(rid) => format!("{rid}/#{tag}"),
            };
            scored_posts.push((score, PostRow {
                thread,
                actor_display: authorship_address(&ingest.principal, &ingest.delegate),
                text: ingest.raw.clone(),
                ts: ingest.ts,
            }));
        }
    }

    scored_items.sort_by(|a, b| b.0.cmp(&a.0));
    scored_threads.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.last_activity.cmp(&a.1.last_activity)));
    scored_posts.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.ts.cmp(&a.1.ts)));

    scored_items.truncate(limit);
    scored_threads.truncate(limit);
    scored_posts.truncate(limit);

    SearchResults {
        items: scored_items.into_iter().map(|(_, r)| r).collect(),
        threads: scored_threads.into_iter().map(|(_, r)| r).collect(),
        posts: scored_posts.into_iter().map(|(_, r)| r).collect(),
    }
}

// --- Snippet highlighting ---

/// Snap a byte offset to the nearest char boundary at or before `pos`.
fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() { return s.len(); }
    let mut i = pos;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}

/// Snap a byte offset to the nearest char boundary at or after `pos`.
fn ceil_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() { return s.len(); }
    let mut i = pos;
    while i < s.len() && !s.is_char_boundary(i) { i += 1; }
    i
}

fn highlight_snippet(text: &str, words: &[String], max_len: usize) -> String {
    // Work entirely on the escaped string so all byte offsets are consistent.
    let escaped_full = escape_html(text);
    let escaped_lower = escaped_full.to_lowercase();

    let first_pos = words
        .iter()
        .filter_map(|w| escaped_lower.find(w.as_str()))
        .min()
        .unwrap_or(0);

    let raw_start = first_pos.saturating_sub(max_len / 3);
    let start = if raw_start > 0 {
        let snapped = ceil_char_boundary(&escaped_full, raw_start);
        // Try to snap to a word boundary (space)
        escaped_full[snapped..].find(' ').map(|i| snapped + i + 1).unwrap_or(snapped)
    } else {
        0
    };
    let start = ceil_char_boundary(&escaped_full, start);
    let end = floor_char_boundary(&escaped_full, (start + max_len).min(escaped_full.len()));

    let snippet = &escaped_full[start..end];
    let snippet_lower = &escaped_lower[start..end];

    let mut marks: Vec<(usize, usize)> = Vec::new();
    for word in words {
        let mut search_from = 0;
        while let Some(pos) = snippet_lower[search_from..].find(word.as_str()) {
            let abs_pos = search_from + pos;
            let mark_end = abs_pos + word.len();
            // Only mark if boundaries are valid
            if snippet.is_char_boundary(abs_pos) && snippet.is_char_boundary(mark_end) {
                marks.push((abs_pos, mark_end));
            }
            search_from = abs_pos + word.len().max(1);
        }
    }

    if marks.is_empty() {
        let mut result = String::new();
        if start > 0 { result.push('\u{2026}'); }
        result.push_str(snippet);
        if end < escaped_full.len() { result.push('\u{2026}'); }
        return result;
    }

    marks.sort_by_key(|&(a, _)| a);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in marks {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }

    let mut result = String::new();
    if start > 0 { result.push('\u{2026}'); }
    let mut cursor = 0;
    for (s, e) in &merged {
        result.push_str(&snippet[cursor..*s]);
        result.push_str("<mark>");
        result.push_str(&snippet[*s..*e]);
        result.push_str("</mark>");
        cursor = *e;
    }
    result.push_str(&snippet[cursor..]);
    if end < escaped_full.len() { result.push('\u{2026}'); }
    result
}

// --- HTML rendering ---

fn render_search_results(results: &SearchResults, query: &str) -> Markup {
    let words = tokenize(query);
    let now = now_ms();

    html! {
        div id="search-results" {
            @if results.items.is_empty() && results.threads.is_empty() && results.posts.is_empty() {
                @if !query.is_empty() {
                    p class="muted" { "no results" }
                }
            } @else {
                @if !results.items.is_empty() {
                    h3 { "items " span class="muted" { "(" (results.items.len()) ")" } }
                    ul class="search-items" {
                        @for r in &results.items {
                            li {
                                a href=(format!("/~/{}", r.path)) class="pre-link" {
                                    "~/" (PreEscaped(highlight_snippet(&r.path, &words, r.path.len() + 10)))
                                }
                                @if let Some(ref body) = r.body {
                                    div class="search-snippet muted" {
                                        (PreEscaped(highlight_snippet(body, &words, 120)))
                                    }
                                }
                            }
                        }
                    }
                }
                @if !results.threads.is_empty() {
                    h3 { "threads " span class="muted" { "(" (results.threads.len()) ")" } }
                    ul class="search-threads" {
                        @for r in &results.threads {
                            li {
                                a href=(format!("/t/{}", r.tag)) {
                                    "#" (PreEscaped(highlight_snippet(&r.tag, &words, r.tag.len() + 10)))
                                }
                                @if let Some(subtitle) = &r.subtitle {
                                    ": " (subtitle)
                                }
                                " "
                                span class="muted" {
                                    (r.post_count) "n · " (timeago::timeago(now, r.last_activity))
                                }
                            }
                        }
                    }
                }
                @if !results.posts.is_empty() {
                    h3 { "posts " span class="muted" { "(" (results.posts.len()) ")" } }
                    ul class="search-posts" {
                        @for r in &results.posts {
                            @let (post_href, post_label) = if let Some((room, tag)) = r.thread.split_once("/#") {
                                if let Some(seg) = slug_types::room_route_segment(room) {
                                    (format!("/r/{seg}/t/{tag}"), format!("{room}/#{tag}"))
                                } else {
                                    ("/".to_string(), r.thread.clone())
                                }
                            } else if let Some(tag) = r.thread.strip_prefix('#') {
                                (format!("/t/{tag}"), r.thread.clone())
                            } else {
                                ("/".to_string(), r.thread.clone())
                            };
                            li {
                                div class="search-post-meta muted" {
                                    a href=(post_href) { (post_label) }
                                    " · " (r.actor_display)
                                    " · " (timeago::timeago(now, r.ts))
                                }
                                div class="search-snippet" {
                                    (PreEscaped(highlight_snippet(&r.text, &words, 160)))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn bc_search() -> Markup {
    html! {
        a href="/" { "slug.social" }
        (bc_segment("search", "/search", true))
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

pub async fn search_page(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let query = params.q.clone().unwrap_or_default();
    let results = if query.len() >= 2 {
        let reduced = state.reduced.read().await;
        let principal = optional_principal(&headers, &jar, &reduced);
        search(&reduced, &query, 50, principal.as_deref())
    } else {
        SearchResults { items: vec![], threads: vec![], posts: vec![] }
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout(
        "search \u{2014} slug.social",
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_search()) }
            div class="search-container" {
                input type="text" id="search-input"
                    placeholder="search items, threads, posts\u{2026}"
                    value=(query) autocomplete="off" autofocus;
            }
            (render_search_results(&results, &query))
            (cli_panel(&["npx slugsocial search <query>"]))
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string())
}

pub async fn search_results_fragment(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    let query = params.q.unwrap_or_default();
    let results = if query.len() >= 2 {
        let reduced = state.reduced.read().await;
        let principal = optional_principal(&headers, &jar, &reduced);
        search(&reduced, &query, 50, principal.as_deref())
    } else {
        SearchResults { items: vec![], threads: vec![], posts: vec![] }
    };

    Html(render_search_results(&results, &query).into_string())
}
