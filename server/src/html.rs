use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
};
use maud::{html, Markup, DOCTYPE};

use crate::{
    ranking::ranked_items,
    reducer::{GroupKey, GroupState},
    state::AppState,
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
                    :root { color-scheme: light dark; }
                    body { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
                           max-width: 820px; margin: 24px auto; padding: 0 16px; line-height: 1.4; }
                    a { text-decoration: none; }
                    a:hover { text-decoration: underline; }
                    h1,h2 { margin: 0 0 12px 0; }
                    .muted { opacity: 0.7; }
                    .row { display: flex; justify-content: space-between; gap: 16px; }
                    pre { padding: 12px; border: 1px solid rgba(127,127,127,0.25); border-radius: 8px; overflow-x: auto; }
                    table { width: 100%; border-collapse: collapse; }
                    td, th { padding: 6px 0; border-bottom: 1px solid rgba(127,127,127,0.2); }
                    th { text-align: left; }
                "#) }
            }
            body {
                (body)
            }
        }
    }
}

pub async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let keys = state.group_keys().await;
    let mut tags: Vec<String> = keys.into_iter().map(|k| k.tag).collect();
    tags.sort();
    tags.dedup();

    let page = layout(
        "slug.social",
        html! {
            h1 { "slug.social" }
            p class="muted" { "collective ranking via pairwise comparisons" }
            h2 { "tags" }
            @if tags.is_empty() {
                p class="muted" { "no votes yet" }
            } @else {
                ul {
                    @for t in tags {
                        li { a href={(format!("/t/{t}"))} { "#" (t) } }
                    }
                }
            }
        },
    );
    Html(page.into_string())
}

pub async fn tag_page(State(state): State<AppState>, Path(tag): Path<String>) -> impl IntoResponse {
    let tag = tag.trim_start_matches('#').to_string();
    let (mut aspects, ingests) = {
        let reduced = state.reduced.read().await;
        let aspects: Vec<String> = reduced
            .groups
            .keys()
            .filter(|k| k.tag == tag)
            .map(|k| k.aspect.clone())
            .collect()
            ;
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
                        li { a href={(format!("/t/{tag}/a/{a}"))} { ":" (a) } }
                    }
                }
            }

            h2 { "recent ingests" }
            @if ingests.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                @for ing in ingests.iter().take(5) {
                    p class="muted" { (format!("ts={} · key={}", ing.ts, ing.voter_key_id)) }
                    pre { (ing.raw) }
                }
            }
        },
    );
    Html(page.into_string())
}

fn render_group(tag: &str, aspect: &str, group: &mut GroupState) -> Markup {
    let ranking = ranked_items(group, 10000, 1e-8);
    html! {
        h1 { "#" (tag) " " ":" (aspect) }
        p { a href={(format!("/t/{tag}"))} { "← #" (tag) } " · " a href="/" { "index" } }

        h2 { "ranking" }
        @if ranking.is_empty() {
            p class="muted" { "no items yet" }
        } @else {
            table {
                thead { tr { th { "item" } th { "score" } } }
                tbody {
                    @for r in ranking.iter().take(50) {
                        tr {
                            td { code { "/" (r.item) } }
                            td { (format!("{:.6}", r.score)) }
                        }
                    }
                }
            }
        }

        h2 { "recent votes" }
        @if group.recent_votes.is_empty() {
            p class="muted" { "none yet" }
        } @else {
            pre {
                @for v in group.recent_votes.iter().take(25) {
                    @if let Some(body) = &v.body {
                        (format!("#{} :{}  /{}  {}  /{}  [{}]\n{{{}}}\n\n",
                            v.tag, v.aspect, v.a, v.score, v.b, v.voter_key_id, body))
                    } @else {
                        (format!("#{} :{}  /{}  {}  /{}  [{}]\n",
                            v.tag, v.aspect, v.a, v.score, v.b, v.voter_key_id))
                    }
                }
            }
        }
    }
}

pub async fn tag_aspect_page(
    State(state): State<AppState>,
    Path((tag, aspect)): Path<(String, String)>,
) -> impl IntoResponse {
    let tag = tag.trim_start_matches('#').to_string();
    let aspect = aspect.trim_start_matches(':').to_string();

    let page = {
        let mut reduced = state.reduced.write().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let Some(group) = reduced.groups.get_mut(&key) else {
            let page = layout(
                "not found",
                html! {
                    h1 { "not found" }
                    p { a href="/" { "← index" } }
                },
            );
            return (StatusCode::NOT_FOUND, Html(page.into_string())).into_response();
        };
        layout(&format!("#{tag} :{aspect}"), render_group(&tag, &aspect, group))
    };

    Html(page.into_string()).into_response()
}


