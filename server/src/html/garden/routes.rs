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
        breadcrumb_path::{ExternalOntologyPath, OntologyPath},
        bc_path, bc_path_external,
        cli_panel,
        forum::ThreadNav,
        layout,
        theme_from_jar,
        theme_next_from_uri,
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
    access::{content_for_garden_view, room_not_found_page, room_scope_has_garden_content, user_can_view_room},
    browse::{GardenBrowsePath, scoped_bc_path_external},
    copy::garden_rank_copy_button_markup,
    item::{child_depth_from_uri, garden_depth_select_markup, item_display_path, item_href},
    item_page::aspect_rankings_for_parent,
    render::render_scope_view,
};

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
            let items =
                resolve_scope_recursive(content, &[root.as_str().to_string()], child_depth);
            build_rankings_for_item_set(content, &items)
        } else {
            build_children_rankings(content, &root)
        };
        let aspect_rankings = aspect_rankings_for_parent(content, &root);
        (child_rankings, aspect_rankings)
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

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
                @for (ci, comp) in child_rankings.component_rankings.iter().enumerate() {
                    div class="ont-group-shell" {
                        div class="ont-group-meta" {
                            (format!("ordering {} items={} pairs={}", ci + 1, comp.ranked.len(), comp.pairs))
                        }
                        ol class="ont-ranking-list" {
                            @for r in comp.ranked.iter() {
                                @let href = item_href(r.item.as_str(), &nav);
                                li {
                                    a href=(href) { (item_display_path(r.item.as_str())) }
                                }
                            }
                        }
                    }
                }
                @if !child_rankings.unranked_items.is_empty() {
                    div class="ont-group-shell ont-group-unsorted" {
                        div class="ont-group-meta" { "unranked" }
                        ul class="ont-group-list" {
                            @for name in &child_rankings.unranked_items {
                                li {
                                    @let href = item_href(name.as_str(), &nav);
                                    a href=(href) { (item_display_path(name.as_str())) }
                                }
                            }
                        }
                    }
                }
            }
            @for aspect in &aspect_rankings {
                section class="ont-tab-panel ont-tab-panel-aspect" {
                    h4 class="ont-aspect-heading" { ":" (aspect.slug) }
                    @if let Some(prompt) = &aspect.prompt {
                        p class="muted ont-aspect-prompt" { (prompt) }
                    }
                    @for (ci, comp) in aspect.rankings.component_rankings.iter().enumerate() {
                        div class="ont-group-shell" {
                            div class="ont-group-meta" {
                                (format!("ordering {} items={} pairs={}", ci + 1, comp.ranked.len(), comp.pairs))
                            }
                            ol class="ont-ranking-list" {
                                @for r in comp.ranked.iter() {
                                    @let href = item_href(r.item.as_str(), &nav);
                                    li {
                                        a href=(href) { (item_display_path(r.item.as_str())) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
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
    let path = OntologyPath::from_input(&path);
    render_scope_view(
        state,
        GardenBrowsePath::Tilde(path),
        ThreadNav::public(),
        jar,
        uri,
    )
    .await
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
                @for (ci, comp) in child_rankings.component_rankings.iter().enumerate() {
                    div class="ont-group-shell" {
                        div class="ont-group-meta" {
                            (format!("ordering {} items={} pairs={}", ci + 1, comp.ranked.len(), comp.pairs))
                        }
                        ol class="ont-ranking-list" {
                            @for r in comp.ranked.iter() {
                                @let href = item_href(r.item.as_str(), &nav);
                                li {
                                    a href=(href) { (item_display_path(r.item.as_str())) }
                                }
                            }
                        }
                    }
                }
                @if !child_rankings.unranked_items.is_empty() {
                    div class="ont-group-shell ont-group-unsorted" {
                        div class="ont-group-meta" { "unranked" }
                        ul class="ont-group-list" {
                            @for name in &child_rankings.unranked_items {
                                li {
                                    @let href = item_href(name.as_str(), &nav);
                                    a href=(href) { (item_display_path(name.as_str())) }
                                }
                            }
                        }
                    }
                }
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
        let loc = match uri.query() {
            Some(q) if !q.is_empty() => format!("{canonical}?{q}"),
            _ => canonical,
        };
        return axum::response::Redirect::permanent(&loc).into_response();
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
                @for (ci, comp) in child_rankings.component_rankings.iter().enumerate() {
                    div class="ont-group-shell" {
                        div class="ont-group-meta" {
                            (format!("ordering {} items={} pairs={}", ci + 1, comp.ranked.len(), comp.pairs))
                        }
                        ol class="ont-ranking-list" {
                            @for r in comp.ranked.iter() {
                                @let href = item_href(r.item.as_str(), &nav);
                                li {
                                    a href=(href) { (item_display_path(r.item.as_str())) }
                                }
                            }
                        }
                    }
                }
                @if !child_rankings.unranked_items.is_empty() {
                    div class="ont-group-shell ont-group-unsorted" {
                        div class="ont-group-meta" { "unranked" }
                        ul class="ont-group-list" {
                            @for name in &child_rankings.unranked_items {
                                li {
                                    @let href = item_href(name.as_str(), &nav);
                                    a href=(href) { (item_display_path(name.as_str())) }
                                }
                            }
                        }
                    }
                }
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
        let loc = match uri.query() {
            Some(q) if !q.is_empty() => format!("{base}?{q}"),
            _ => base,
        };
        return axum::response::Redirect::permanent(&loc).into_response();
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
    let path = OntologyPath::from_input(&path);
    render_scope_view(state, GardenBrowsePath::Tilde(path), nav, jar, uri).await
}

