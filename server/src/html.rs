use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
};
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;

use crate::{
    ranking::ranked_items,
    reducer::{GroupKey, GroupState},
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

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
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
                @for ing in ingests.iter().take(5) {
                    p class="muted" { (format!("ts={} · @{} · key={}", ing.ts, ing.actor, ing.voter_key_id)) }
                    pre { (ing.raw) }
                }
            }
        },
    );
    Html(page.into_string()).into_response()
}

fn render_group(tag: &str, aspect: &str, group: &mut GroupState) -> Markup {
    let ranking = ranked_items(group, 10000, 1e-8);
    html! {
        h1 { "#" (tag) " " ":" (aspect) }
        p { a href={(format!("/~/{tag}"))} { "← #" (tag) } " · " a href="/" { "index" } }

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
                    @let ratio = format!("{}:{}", v.ratio_left, v.ratio_right);
                    (format!("#{} :{}  /{}  {}  /{}  [@{}]\n{{{}}}\n\n",
                        v.tag, v.aspect, v.a, ratio, v.b, v.actor, v.body))
                }
            }
        }
    }
}

async fn render_aspect_view(
    state: AppState,
    tag: String,
    aspect: String,
) -> axum::response::Response {
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


