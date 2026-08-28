use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::api::optional_principal;
use crate::canonical_path::canonicalize_tag;
use crate::identity::parse_username;
use crate::middleware::canonical_view_url;
use crate::state::AppState;

use super::ingest::{thread_nav_for_ingest, thread_post_index_in_scope};
use super::page::auth_strip;
use crate::html::{
    bc_segment, cli_panel, layout, now_ms, profile_href, recency_color_style, theme_from_jar,
    theme_next_from_uri,
};
use crate::timeago;

struct ProfilePostRow {
    thread_tag: String,
    post_idx: usize,
    post_href: String,
    ts: i64,
    snippet: String,
}

pub async fn user_profile_page(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let canon = match parse_username(&username) {
        Ok(u) => u,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };

    let (rows, strip) = {
        let reduced = state.reduced.read().await;
        let viewer = optional_principal(&headers, &jar, &reduced);
        let ids = reduced.visible_posts_for_actor(&canon, viewer.as_deref());
        let mut rows: Vec<ProfilePostRow> = Vec::new();
        for id in ids {
            if reduced.redacted_posts.contains(&id) {
                continue;
            }
            let Some(ing) = reduced.ingests_by_id.get(&id).cloned() else {
                continue;
            };
            let Some(nav) = thread_nav_for_ingest(&ing) else {
                continue;
            };
            let Some(post_idx) = thread_post_index_in_scope(&reduced, &ing) else {
                continue;
            };
            let tag = canonicalize_tag(&ing.thread_tag);
            let post_href = nav.post_url(&tag, post_idx);
            let raw_one_line = ing.raw.lines().next().unwrap_or("").trim();
            let snippet: String = raw_one_line.chars().take(120).collect();
            rows.push(ProfilePostRow {
                thread_tag: tag,
                post_idx,
                post_href,
                ts: ing.ts,
                snippet,
            });
        }
        rows.sort_by(|a, b| b.ts.cmp(&a.ts));
        let strip = auth_strip(&headers, &jar, &reduced);
        (rows, strip)
    };

    let now = now_ms();
    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout(
        &format!("@{canon}"),
        "view-thread",
        html! {
            (strip)
            nav class="breadcrumb" {
                a href="/" { "slug.social" }
                (bc_segment(&format!("@{canon}"), &profile_href(&canon), true))
            }
            h2 { "@" (canon) }
            p class="muted" { "posts (newest first)" }
            @if rows.is_empty() {
                p class="muted" { "no public posts yet" }
            } @else {
                ul class="profile-post-list" {
                    @for r in &rows {
                        @let hover = timeago::rfc3339_utc(r.ts);
                        @let ago = timeago::timeago(now, r.ts);
                        @let ts_style = recency_color_style(now, r.ts);
                        li {
                            a href=(r.post_href.as_str()) {
                                "#" (r.thread_tag)
                                " / #"
                                (r.post_idx)
                            }
                            span class="muted" { " · " }
                            span class="ts-recency" style=(ts_style.as_str()) title=(hover) { (ago) }
                            @if !r.snippet.is_empty() {
                                p class="profile-post-snippet muted" { (r.snippet) }
                            }
                        }
                    }
                }
            }
            (cli_panel(&["npx slugsocial public forum list".to_string()]))
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string()).into_response()
}
