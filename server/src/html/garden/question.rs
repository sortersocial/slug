//! Shareable question pages: `GET /q/:collection` and `GET /q/:collection/:aspect`.
//!
//! A rendering of pairwise compare seeded by a garden scope (and optional aspect).
//! No new POST; votes use the existing `VoteComparePost` UI action.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::{
    api::optional_principal,
    dsl::is_valid_aspect_slug,
    html::{
        forum::ThreadNav, layout_full_bleed_chromeless, render_item_body_in_scope, theme_from_jar,
        theme_next_from_uri,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    reducer::ContentState,
    scope_rank::comparable_items,
    state::AppState,
};

use super::{
    access::{
        content_for_garden_view, room_not_found_page, room_scope_has_garden_content,
        user_can_view_room,
    },
    vote::{vote_compare_inner, QuestionCtx, QuestionHeadline, VoteCompareQuery},
};

/// `psalms` → leaf item `~psalms`. Rejects root and multi-segment tails.
pub(super) fn parse_collection_leaf(collection: &str) -> Option<ItemId> {
    let raw = collection.trim().trim_start_matches('/');
    if raw.is_empty() || raw.contains('/') {
        return None;
    }
    let spec = if raw.starts_with('~') {
        raw.to_string()
    } else {
        format!("~{raw}")
    };
    let id = ItemId::parse(&spec)?.normalized_storage().ontology_leaf();
    if id.tilde_tail() == Some("") {
        return None;
    }
    Some(id)
}

pub(super) fn collection_is_known(content: &ContentState, id: &ItemId) -> bool {
    content.items.contains(id)
        || content.item_bodies.contains_key(id)
        || !content.members_of(id).is_empty()
}

pub(super) fn question_headline(
    content: &ContentState,
    collection: &ItemId,
    aspect: Option<&str>,
) -> QuestionHeadline {
    if let Some(slug) = aspect {
        if let Some(p) = content
            .aspect_prompt(slug)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return QuestionHeadline::Body(p.to_string());
        }
        return QuestionHeadline::Fallback(format!(":{slug} — which wins?"));
    }
    if let Some(body) = content
        .item_bodies
        .get(collection)
        .map(|b| b.trim())
        .filter(|s| !s.is_empty())
    {
        return QuestionHeadline::Body(body.to_string());
    }
    QuestionHeadline::Fallback(format!("Which is greater: {}?", collection.last_segment()))
}

fn question_title(headline: &QuestionHeadline) -> String {
    match headline {
        QuestionHeadline::Fallback(s) => s.clone(),
        QuestionHeadline::Body(b) => {
            let line = b.lines().next().unwrap_or("").trim();
            if line.is_empty() {
                "question".into()
            } else if line.chars().count() > 80 {
                format!("{}…", line.chars().take(79).collect::<String>())
            } else {
                line.to_string()
            }
        }
    }
}

fn ranking_href(nav: &ThreadNav, collection: &ItemId, aspect: Option<&str>) -> String {
    let base = nav.garden_item_href(collection);
    match aspect.filter(|s| !s.is_empty()) {
        Some(a) => format!("{base}#aspect-{a}"),
        None => base,
    }
}

fn build_question_ctx(
    nav: &ThreadNav,
    collection: &ItemId,
    aspect: Option<&str>,
    headline: QuestionHeadline,
) -> QuestionCtx {
    let leaf = collection.last_segment().to_string();
    QuestionCtx {
        aspect: aspect.map(str::to_string),
        question_path: nav.question_href(&leaf, aspect),
        garden_href: ranking_href(nav, collection, aspect),
        thread_href: nav.thread_url(&leaf),
        title: question_title(&headline),
        headline,
    }
}

fn question_empty_page(
    state: &AppState,
    nav: &ThreadNav,
    uri: &Uri,
    jar: &CookieJar,
    ctx: QuestionCtx,
) -> axum::response::Response {
    let url_key = canonical_view_url(uri);
    let view_count = state.views.get_views(&url_key);
    let body = html! {
        section class="vote-compare-shell" {
            header class="vote-question" {
                @match &ctx.headline {
                    QuestionHeadline::Body(text) => {
                        div class="vote-question-headline" {
                            (render_item_body_in_scope(text, nav.garden_root_url(), None))
                        }
                    }
                    QuestionHeadline::Fallback(text) => {
                        h1 class="vote-question-headline" { (text) }
                    }
                }
                nav class="vote-question-links muted" {
                    a data-testid="question-ranking-link" href=(ctx.garden_href) { "ranking" }
                    " · "
                    a data-testid="question-thread-link" href=(ctx.thread_href) { "thread" }
                }
            }
            p class="muted vote-question-empty" {
                "nothing to compare yet — this scope needs at least two items with bodies."
            }
        }
    };
    let page = layout_full_bleed_chromeless(
        &ctx.title,
        "view-ontology view-ontology-light view-vote-compare view-vote-compare-fullscreen view-vote-question",
        body,
        Some(view_count),
        theme_from_jar(jar),
        &theme_next_from_uri(uri),
    );
    Html(page.into_string()).into_response()
}

async fn question_inner(
    state: AppState,
    collection: String,
    aspect: Option<String>,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    if let Some(slug) = aspect.as_deref() {
        if !is_valid_aspect_slug(slug) {
            return room_not_found_page(&jar, &uri).into_response();
        }
    }
    let Some(collection_id) = parse_collection_leaf(&collection) else {
        return room_not_found_page(&jar, &uri).into_response();
    };

    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    if !collection_is_known(content, &collection_id) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    let headline = question_headline(content, &collection_id, aspect.as_deref());
    let members = comparable_items(content, content.members_of(&collection_id));
    let ctx = build_question_ctx(&nav, &collection_id, aspect.as_deref(), headline);
    let leaf = collection_id.last_segment().to_string();
    let pool_display = collection_id.display_path();
    drop(reduced);

    if members.len() < 2 {
        return question_empty_page(&state, &nav, &uri, &jar, ctx);
    }

    let q = VoteCompareQuery {
        left: None,
        right: None,
        thread: Some(leaf),
        pool: Some(pool_display),
    };
    vote_compare_inner(state, q, nav, headers, jar, uri, Some(ctx)).await
}

/// `GET /q/:collection`
pub async fn question_page(
    State(state): State<AppState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    question_inner(
        state,
        collection,
        None,
        ThreadNav::public(),
        headers,
        jar,
        uri,
    )
    .await
}

/// `GET /q/:collection/:aspect`
pub async fn question_aspect_page(
    State(state): State<AppState>,
    Path((collection, aspect)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    question_inner(
        state,
        collection,
        Some(aspect),
        ThreadNav::public(),
        headers,
        jar,
        uri,
    )
    .await
}

async fn room_question_gate(
    state: AppState,
    room_key: &str,
    headers: &HeaderMap,
    jar: &CookieJar,
    uri: &Uri,
) -> Result<(AppState, ThreadNav), axum::response::Response> {
    let Some(room_id) = slug_types::room_id_from_route_segment(room_key) else {
        return Err((StatusCode::NOT_FOUND, "bad room path").into_response());
    };
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return Err((StatusCode::NOT_FOUND, "bad room path").into_response());
    };
    let reduced = state.reduced.read().await;
    let user = optional_principal(headers, jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return Err(room_not_found_page(jar, uri).into_response());
    }
    if !room_scope_has_garden_content(&reduced, &nav) {
        drop(reduced);
        return Err(room_not_found_page(jar, uri).into_response());
    }
    drop(reduced);
    Ok((state, nav))
}

/// `GET /r/:room_key/q/:collection`
pub async fn room_question_page(
    State(state): State<AppState>,
    Path((room_key, collection)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let (state, nav) = match room_question_gate(state, &room_key, &headers, &jar, &uri).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    question_inner(state, collection, None, nav, headers, jar, uri).await
}

/// `GET /r/:room_key/q/:collection/:aspect`
pub async fn room_question_aspect_page(
    State(state): State<AppState>,
    Path((room_key, collection, aspect)): Path<(String, String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let (state, nav) = match room_question_gate(state, &room_key, &headers, &jar, &uri).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    question_inner(state, collection, Some(aspect), nav, headers, jar, uri).await
}
