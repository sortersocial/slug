use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::api::optional_principal;
use crate::canonical_path::canonicalize_tag;
use crate::middleware::canonical_view_url;
use crate::reducer::ScopeId;
use crate::state::AppState;

use super::access::user_can_view_room;
use super::ingest::ingest_entry_markup;
use super::nav::ThreadNav;
use super::paginator::render_post_permalink_nav;
use super::page::bc_room;
use super::views::room_not_found_page;
use crate::html::{bc_threads, layout, now_ms, theme_from_jar, theme_next_from_uri};
use maud::Markup;

async fn thread_post_view_inner(
    state: AppState,
    tag: String,
    index_str: String,
    nav: ThreadNav,
    viewer: Option<String>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let tag = canonicalize_tag(&tag);
    let index: usize = index_str.parse().unwrap_or(0);
    let scope = nav.scope();
    let now = now_ms();
    let (ing, body_markup, permalink_nav) = {
        let reduced = state.reduced.read().await;
        let queue = reduced
            .ingests_by_scope_thread
            .get(&(scope.clone(), tag.clone()));
        let total_posts = queue.map(|q| q.len()).unwrap_or(0);
        let ing = queue
            .and_then(|q| q.iter().rev().nth(index))
            .and_then(|id| reduced.ingests_by_id.get(id).cloned());
        let nav_index = ing
            .as_ref()
            .and_then(|i| reduced.try_thread_post_index_chronological(&scope, &tag, &i.id))
            .unwrap_or(index);
        let body = ing.as_ref().map(|i| {
            ingest_entry_markup(&nav, &tag, nav_index, i, viewer.as_deref(), now, &reduced)
        });
        let permalink_nav = if total_posts > 1 {
            Some(render_post_permalink_nav(&nav, &tag, nav_index, total_posts))
        } else {
            None
        };
        (ing, body, permalink_nav)
    };

    let sc = nav.scope();
    let bc: Markup = match &sc {
        ScopeId::Public => bc_threads(Some(&tag), Some(index)),
        ScopeId::Room(rid) => {
            let slug = if let Some((_, s)) = rid.split_once('/') {
                s
            } else {
                rid.as_str()
            };
            bc_room(&nav, slug, Some(&tag), Some(index))
        }
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout(
        &format!("#{tag} / post #{index}"),
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc) }
            @if let (Some(_ing), Some(bm)) = (&ing, &body_markup) {
                @if let Some(ref pn) = permalink_nav {
                    (pn)
                }
                (bm)
                @if let Some(pn) = permalink_nav {
                    (pn)
                }
            } @else {
                p class="muted" { "post not found" }
            }
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string()).into_response()
}

pub async fn thread_post_view(
    State(state): State<AppState>,
    Path((tag, index_str)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let viewer = optional_principal(&headers, &jar, &reduced);
    drop(reduced);
    thread_post_view_inner(state, tag, index_str, ThreadNav::public(), viewer, jar, uri)
        .await
}

pub async fn room_thread_post_view(
    State(state): State<AppState>,
    Path((room_key, tag, index_str)): Path<(String, String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let Some(room_id) = slug_types::room_id_from_route_segment(&room_key) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    drop(reduced);
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    thread_post_view_inner(state, tag, index_str, nav, user, jar, uri)
        .await
        .into_response()
}
