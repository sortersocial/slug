use axum::{
    http::{StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::{
    events::ThreadCapability,
    html::{
        forum::ThreadNav,
        layout,
        theme_from_jar,
        theme_next_from_uri,
    },
    reducer::{ContentState, ReducerState, ScopeId},
};

pub(super) fn room_not_found_page(jar: &CookieJar, uri: &Uri) -> impl IntoResponse {
    let body = html! {
        nav class="breadcrumb" { a href="/" { "slug.social" } }
        h1 { "not found" }
        p { "The requested page could not be found." }
        p { a href="/" { "home" } }
    };
    let page = layout(
        "not found — slug.social",
        "view-thread",
        body,
        None,
        theme_from_jar(jar),
        &theme_next_from_uri(uri),
        None,
        None,
    );
    (StatusCode::NOT_FOUND, Html(page.into_string()))
}

pub(super) fn user_can_view_room(reduced: &ReducerState, room_id: &str, username: Option<&str>) -> bool {
    if !reduced.rooms.contains(room_id) {
        return false;
    }
    let Some(u) = username else {
        return false;
    };
    reduced.user_has_cap(room_id, u, ThreadCapability::View)
}

/// Private `~/` / `-/` garden pages require at least one ingest in that scope (a `content` entry).
pub(super) fn room_scope_has_garden_content(reduced: &ReducerState, nav: &ThreadNav) -> bool {
    match nav.scope() {
        ScopeId::Public => true,
        ScopeId::Room(_) => reduced.content_for_scope(&nav.scope()).is_some(),
    }
}

pub(super) fn content_for_garden_view<'a>(
    reduced: &'a ReducerState,
    scope: &ScopeId,
) -> &'a ContentState {
    match scope {
        ScopeId::Public => reduced.public(),
        ScopeId::Room(_) => reduced
            .content_for_scope(scope)
            .expect("room garden only renders after room_scope_has_garden_content returned true"),
    }
}
