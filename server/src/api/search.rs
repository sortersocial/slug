use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::state::AppState;

use super::helpers::item_path_for_api;

// ============================================================================
// Search
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SearchApiQuery {
    pub q: Option<String>,
    pub limit: Option<usize>,
}

fn tokenize_query(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn text_contains_all(text: &str, words: &[String]) -> bool {
    let lower = text.to_lowercase();
    words.iter().all(|w| lower.contains(w.as_str()))
}

fn text_contains_any(text: &str, words: &[String]) -> usize {
    let lower = text.to_lowercase();
    words.iter().filter(|w| lower.contains(w.as_str())).count()
}

fn snippet_around(text: &str, words: &[String], max_len: usize) -> String {
    let lower = text.to_lowercase();
    let first_pos = words.iter().filter_map(|w| lower.find(w.as_str())).min().unwrap_or(0);
    let start = first_pos.saturating_sub(max_len / 3);
    let start = if start > 0 {
        let mut i = start;
        while i < text.len() && !text.is_char_boundary(i) { i += 1; }
        text[i..].find(' ').map(|j| i + j + 1).unwrap_or(i)
    } else {
        0
    };
    let mut end = (start + max_len).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) { end += 1; }
    text[start..end].to_string()
}

pub async fn get_search(
    State(state): State<AppState>,
    Query(params): Query<SearchApiQuery>,
) -> impl IntoResponse {
    let q = params.q.unwrap_or_default();
    let limit = params.limit.unwrap_or(50).min(200);

    let words = tokenize_query(&q);
    if words.is_empty() {
        return Json(slug_types::SearchResponse {
            items: vec![], threads: vec![], posts: vec![],
        }).into_response();
    }

    let reduced = state.reduced.read().await;

    let mut scored_items: Vec<(u32, slug_types::SearchItemHit)> = Vec::new();
    for item in &reduced.items {
        let mut score: u32 = 0;
        if text_contains_all(item.as_str(), &words) { score += 10; }
        else if text_contains_any(item.as_str(), &words) > 0 { score += 5; }
        if let Some(body) = reduced.item_bodies.get(item) {
            if text_contains_all(body, &words) { score += 6; }
            else {
                let any = text_contains_any(body, &words);
                if any > 0 { score += any as u32; }
            }
        }
        if score > 0 {
            scored_items.push((score, slug_types::SearchItemHit {
                path: item_path_for_api(item.as_str()),
                body: reduced.item_bodies.get(item).map(|b| snippet_around(b, &words, 120)),
            }));
        }
    }

    let mut scored_threads: Vec<(u32, i64, slug_types::SearchThreadHit)> = Vec::new();
    for (tag, ts) in &reduced.threads {
        let mut score: u32 = 0;
        if text_contains_all(tag, &words) { score += 8; }
        else if text_contains_any(tag, &words) > 0 { score += 4; }
        if score > 0 {
            let post_count = reduced.ingests_by_thread.get(tag).map(|q| q.len()).unwrap_or(0);
            scored_threads.push((score, ts.last_activity_ts, slug_types::SearchThreadHit {
                tag: format!("#{tag}"),
                post_count,
                last_activity: ts.last_activity_ts,
            }));
        }
    }

    let mut scored_posts: Vec<(u32, i64, slug_types::SearchPostHit)> = Vec::new();
    for (id, ingest) in &reduced.ingests_by_id {
        let mut score: u32 = 0;
        if text_contains_all(&ingest.raw, &words) { score += 4; }
        else {
            let any = text_contains_any(&ingest.raw, &words);
            if any > 0 { score += any as u32; }
        }
        if score > 0 {
            let thread = reduced.ingests_by_thread.iter()
                .find(|(_, ids)| ids.contains(id))
                .map(|(tag, _)| format!("#{tag}"))
                .unwrap_or_else(|| "#unknown".to_string());
            scored_posts.push((score, ingest.ts, slug_types::SearchPostHit {
                thread,
                actor: format!("@{}", ingest.principal),
                snippet: snippet_around(&ingest.raw, &words, 160),
                ts: ingest.ts,
            }));
        }
    }

    scored_items.sort_by(|a, b| b.0.cmp(&a.0));
    scored_threads.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    scored_posts.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    scored_items.truncate(limit);
    scored_threads.truncate(limit);
    scored_posts.truncate(limit);

    Json(slug_types::SearchResponse {
        items: scored_items.into_iter().map(|(_, h)| h).collect(),
        threads: scored_threads.into_iter().map(|(_, _, h)| h).collect(),
        posts: scored_posts.into_iter().map(|(_, _, h)| h).collect(),
    }).into_response()
}
