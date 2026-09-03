use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::{
    api::optional_principal,
    html::{
        bc_path, bc_path_external,
        breadcrumb_path::{ExternalOntologyPath, OntologyPath},
        cli_panel,
        forum::ThreadNav,
        layout, theme_from_jar, theme_next_from_uri,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    scope_rank::{
        build_children_rankings, build_rankings_for_item_set, external_root_host_items,
        resolve_scope_recursive,
    },
    state::AppState,
};

use super::{
    access::{
        content_for_garden_view, room_not_found_page, room_scope_has_garden_content,
        user_can_view_room,
    },
    browse::{scoped_bc_path_external, GardenBrowsePath},
    copy::garden_rank_copy_button_markup,
    item::{child_depth_from_uri, garden_depth_select_markup},
    item_page::aspect_rankings_for_parent,
    pin::pinned_item_from_jar,
    render::{aspect_ranking_sections_markup, ont_ranking_lists_markup, render_scope_view},
};

/// 308 to `canonical`, keeping a non-empty query string.
fn redirect_permanent_preserving_query(canonical: &str, uri: &Uri) -> axum::response::Redirect {
    match uri.query() {
        Some(q) if !q.is_empty() => {
            axum::response::Redirect::permanent(&format!("{canonical}?{q}"))
        }
        _ => axum::response::Redirect::permanent(canonical),
    }
}

/// `GET /~/` → `/~`, `GET /-/` → `/-`, and the room-scoped twins. Query is preserved.
pub async fn redirect_strip_trailing_slash(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_end_matches('/');
    let canonical = if path.is_empty() { "/" } else { path };
    redirect_permanent_preserving_query(canonical, &uri)
}

pub async fn garden_index(
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let nav = ThreadNav::public();
    let child_depth = child_depth_from_uri(&uri);
    let (child_rankings, aspect_rankings) = {
        let reduced = state.reduced.read().await;
        let content = reduced.public();
        let root = ItemId::ontology_root();
        let child_rankings = if child_depth > 1 {
            let items = resolve_scope_recursive(content, &[root.as_str().to_string()], child_depth);
            build_rankings_for_item_set(content, &items)
        } else {
            build_children_rankings(content, &root)
        };
        let aspect_rankings = aspect_rankings_for_parent(content, &root);
        (child_rankings, aspect_rankings)
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    // Shared ranking-list markup (same helper as item pages): scores + pin/vote
    // affordances, so the index never drifts from the scope view.
    let pin_ref = pinned_item_from_jar(&jar);
    let next_for_pin = match uri.query() {
        Some(q) if !q.is_empty() => format!("/~?{q}"),
        _ => "/~".to_string(),
    };
    let (rankings_markup, aspects_markup) = {
        let reduced = state.reduced.read().await;
        let scope_content = content_for_garden_view(&reduced, &nav.scope());
        (
            ont_ranking_lists_markup(
                &child_rankings,
                &nav,
                pin_ref.as_ref(),
                scope_content,
                &next_for_pin,
                "",
            ),
            aspect_ranking_sections_markup(
                &aspect_rankings,
                &nav,
                pin_ref.as_ref(),
                scope_content,
                &next_for_pin,
            ),
        )
    };

    let page = layout(
        "~/",
        "view-ontology view-ontology-light",
        html! {
            @let root_path = OntologyPath::root();
            nav class="breadcrumb" { (bc_path(&root_path)) }
            h2 {
                "paths"
                " "
                (garden_depth_select_markup(child_depth))
                @if !(child_rankings.component_rankings.is_empty() && child_rankings.unranked_items.is_empty()) {
                    " "
                    (garden_rank_copy_button_markup(&nav, "~/", child_depth, false))
                }
            }
            @if child_rankings.component_rankings.is_empty() && child_rankings.unranked_items.is_empty() {
                p class="muted" { "no items yet" }
            } @else {
                (rankings_markup)
            }
            (aspects_markup)
            (cli_panel(&["npx slugsocial garden tree"]))
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        Some("public"),
        Some("/~"),
    );
    Html(page.into_string())
}

/// Single public handler for all `/~/*path` routes.
pub async fn ontology_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    if let Some(canonical) = OntologyPath::nested_redirect_target(&path) {
        return redirect_permanent_preserving_query(&canonical, &uri).into_response();
    }
    let path = OntologyPath::from_input(&path);
    render_scope_view(
        state,
        GardenBrowsePath::Tilde(path),
        ThreadNav::public(),
        jar,
        uri,
    )
    .await
    .into_response()
}

/// External ontology index (`/-/`).
pub async fn external_garden_index(
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let nav = ThreadNav::public();
    let ext_path = ExternalOntologyPath::from_input("");
    let child_rankings = {
        let reduced = state.reduced.read().await;
        let content = reduced.public();
        let hosts = external_root_host_items(content);
        build_rankings_for_item_set(content, &hosts)
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let pin_ref = pinned_item_from_jar(&jar);
    let next_for_pin = match uri.query() {
        Some(q) if !q.is_empty() => format!("/-/?{q}"),
        _ => "/-/".to_string(),
    };
    let rankings_markup = {
        let reduced = state.reduced.read().await;
        let scope_content = content_for_garden_view(&reduced, &nav.scope());
        ont_ranking_lists_markup(
            &child_rankings,
            &nav,
            pin_ref.as_ref(),
            scope_content,
            &next_for_pin,
            "",
        )
    };

    let page = layout(
        "-/",
        "view-ontology view-ontology-light",
        html! {
            nav class="breadcrumb" { (bc_path_external(&ext_path)) }
            h2 {
                "external paths"
                @if !(child_rankings.component_rankings.is_empty() && child_rankings.unranked_items.is_empty()) {
                    " "
                    (garden_rank_copy_button_markup(&nav, "", 1, true))
                }
            }
            p class="muted" { "Items outside slug.social use the " code { "-/" } " prefix followed by the full " code { "https://…" } " URL (legacy " code { "-/host/path" } " still works)." }
            @if child_rankings.component_rankings.is_empty() && child_rankings.unranked_items.is_empty() {
                p class="muted" { "no external items indexed yet" }
            } @else {
                (rankings_markup)
            }
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        Some("public"),
        Some("/~"),
    );
    Html(page.into_string())
}

/// Single public handler for all `/-/…` routes.
pub async fn external_ontology_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    if let Some(canonical) = ExternalOntologyPath::legacy_redirect_target(&path) {
        return redirect_permanent_preserving_query(&canonical, &uri).into_response();
    }
    let path = ExternalOntologyPath::from_input(&path);
    render_scope_view(
        state,
        GardenBrowsePath::External(path),
        ThreadNav::public(),
        jar,
        uri,
    )
    .await
    .into_response()
}

pub async fn room_garden_index(
    State(state): State<AppState>,
    Path(room_key): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let Some(room_id) = slug_types::room_id_from_route_segment(&room_key) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    if !room_scope_has_garden_content(&reduced, &nav) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    drop(reduced);
    render_scope_view(
        state,
        GardenBrowsePath::Tilde(OntologyPath::root()),
        nav,
        jar,
        uri,
    )
    .await
}

pub async fn room_external_garden_index(
    State(state): State<AppState>,
    Path(room_key): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let Some(room_id) = slug_types::room_id_from_route_segment(&room_key) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    if !room_scope_has_garden_content(&reduced, &nav) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    let ext_path = ExternalOntologyPath::from_input("");
    let child_rankings = {
        let content = content_for_garden_view(&reduced, &nav.scope());
        let hosts = external_root_host_items(content);
        build_rankings_for_item_set(content, &hosts)
    };
    drop(reduced);

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let pin_ref = pinned_item_from_jar(&jar);
    let next_for_pin = match uri.query() {
        Some(q) if !q.is_empty() => format!("{}-/?{q}", nav.garden_root_url().trim_end_matches('~')),
        _ => format!("{}-/", nav.garden_root_url().trim_end_matches('~')),
    };
    let rankings_markup = {
        let reduced = state.reduced.read().await;
        let scope_content = content_for_garden_view(&reduced, &nav.scope());
        ont_ranking_lists_markup(
            &child_rankings,
            &nav,
            pin_ref.as_ref(),
            scope_content,
            &next_for_pin,
            "",
        )
    };

    let page = layout(
        "-/",
        "view-ontology view-ontology-light",
        html! {
            nav class="breadcrumb" { (scoped_bc_path_external(&ext_path, &nav)) }
            h2 {
                "external paths"
                @if !(child_rankings.component_rankings.is_empty() && child_rankings.unranked_items.is_empty()) {
                    " "
                    (garden_rank_copy_button_markup(&nav, "", 1, true))
                }
            }
            @if child_rankings.component_rankings.is_empty() && child_rankings.unranked_items.is_empty() {
                p class="muted" { "no external items indexed yet" }
            } @else {
                (rankings_markup)
            }
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        Some(nav.room_wire.as_str()),
        Some(nav.garden_root_url()),
    );
    Html(page.into_string()).into_response()
}

pub async fn room_external_ontology_path(
    State(state): State<AppState>,
    Path((room_key, path)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let Some(room_id) = slug_types::room_id_from_route_segment(&room_key) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    if !room_scope_has_garden_content(&reduced, &nav) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    drop(reduced);
    if let Some(canonical_tail) = ExternalOntologyPath::legacy_redirect_target(&path) {
        // canonical_tail is `/-/https://…`; under a room it becomes `/r/{seg}/-/https://…`
        let tail = canonical_tail
            .strip_prefix("/-/")
            .unwrap_or(canonical_tail.trim_start_matches('/'));
        let base = format!("{}-/{tail}", nav.garden_root_url().trim_end_matches('~'));
        return redirect_permanent_preserving_query(&base, &uri).into_response();
    }
    let path = ExternalOntologyPath::from_input(&path);
    render_scope_view(state, GardenBrowsePath::External(path), nav, jar, uri)
        .await
        .into_response()
}

pub async fn room_ontology_path(
    State(state): State<AppState>,
    Path((room_key, path)): Path<(String, String)>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let Some(room_id) = slug_types::room_id_from_route_segment(&room_key) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let Some(nav) = ThreadNav::from_room_id(&room_id) else {
        return (StatusCode::NOT_FOUND, "bad room path").into_response();
    };
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if !user_can_view_room(&reduced, &room_id, user.as_deref()) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    if !room_scope_has_garden_content(&reduced, &nav) {
        drop(reduced);
        return room_not_found_page(&jar, &uri).into_response();
    }
    drop(reduced);
    if let Some(canonical) = OntologyPath::nested_redirect_target(&path) {
        let loc = if let Some(rest) = canonical.strip_prefix("/~") {
            format!("{}{rest}", nav.garden_root_url())
        } else {
            canonical
        };
        return redirect_permanent_preserving_query(&loc, &uri).into_response();
    }
    let path = OntologyPath::from_input(&path);
    render_scope_view(state, GardenBrowsePath::Tilde(path), nav, jar, uri)
        .await
        .into_response()
}
