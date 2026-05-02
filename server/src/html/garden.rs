use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::{
    api::optional_principal,
    canonical_path::canonicalize_item,
    events::ThreadCapability,
    path_types::ItemId,
    reducer::{ContentState, ReducerState, ScopeId},
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    scope_rank::{build_children_rankings, ChildrenRankings},
    state::AppState,
    timeago,
};

use super::{
    bc_path, bc_path_external, bc_segment, cli_panel, layout, now_ms, ratio_pct,
    render_linkified_with_embeds_in_scope, theme_from_jar, theme_next_from_uri,
    breadcrumb_path::{ExternalOntologyPath, OntologyPath},
    forum::ThreadNav,
};

/// Display path for an item: `~/…` or `-/…` form.
fn item_display_path(item: &str) -> String {
    ItemId::parse(item)
        .map(|c| c.display_path())
        .unwrap_or_else(|| canonicalize_item(item))
}

/// Garden href for an item path string in this nav scope.
fn item_href(item: &str, nav: &ThreadNav) -> String {
    nav.garden_item_url(item)
}

fn item_code_label(item: &str) -> String {
    item_display_path(item)
}

fn scoped_bc_path_external(path: &ExternalOntologyPath, nav: &ThreadNav) -> maud::Markup {
    match nav.scope() {
        ScopeId::Public => bc_path_external(path),
        ScopeId::Room(rid) => {
            let slug = rid.split_once('/').map(|(_, s)| s).unwrap_or(rid.as_str());
            let ext_root = format!("{}-", nav.garden_root_url().trim_end_matches('~'));
            html! {
                a href="/" { "slug.social" }
                (bc_segment(&format!("room:{slug}"), nav.room_url(), false))
                (bc_segment("-", &ext_root, path.is_root()))
                @for (i, seg) in path.segments().iter().enumerate() {
                    @let href = format!("{}/{}", ext_root, path.segments()[..=i].join("/"));
                    @let is_last = i == path.segments().len() - 1;
                    (bc_segment(seg, &href, is_last))
                }
            }
        }
    }
}

enum GardenBrowsePath {
    Tilde(OntologyPath),
    External(ExternalOntologyPath),
}

impl GardenBrowsePath {
    fn item(&self) -> &str {
        match self {
            GardenBrowsePath::Tilde(p) => p.as_str(),
            GardenBrowsePath::External(p) => p.as_str(),
        }
    }

    fn is_external(&self) -> bool {
        matches!(self, GardenBrowsePath::External(_))
    }
}

fn scoped_bc_path_for(path: &GardenBrowsePath, nav: &ThreadNav) -> maud::Markup {
    match path {
        GardenBrowsePath::Tilde(p) => scoped_bc_path(p, nav),
        GardenBrowsePath::External(p) => scoped_bc_path_external(p, nav),
    }
}

fn scoped_bc_path(path: &OntologyPath, nav: &ThreadNav) -> maud::Markup {
    match nav.scope() {
        ScopeId::Public => bc_path(path),
        ScopeId::Room(rid) => {
            let slug = rid.split_once('/').map(|(_, s)| s).unwrap_or(rid.as_str());
            html! {
                a href="/" { "slug.social" }
                (bc_segment(&format!("room:{slug}"), nav.room_url(), false))
                (bc_segment("~", nav.garden_root_url(), path.is_root()))
                @for (i, seg) in path.segments().iter().enumerate() {
                    @let href = format!("{}/{}", nav.garden_root_url(), path.segments()[..=i].join("/"));
                    @let is_last = i == path.segments().len() - 1;
                    (bc_segment(seg, &href, is_last))
                }
            }
        }
    }
}

fn room_not_found_page(jar: &CookieJar, uri: &Uri) -> impl IntoResponse {
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
    );
    (StatusCode::NOT_FOUND, Html(page.into_string()))
}

fn user_can_view_room(reduced: &ReducerState, room_id: &str, username: Option<&str>) -> bool {
    if !reduced.rooms.contains(room_id) {
        return false;
    }
    let Some(u) = username else {
        return false;
    };
    reduced.user_has_cap(room_id, u, ThreadCapability::View)
}

/// Private `~/` / `-/` garden pages require at least one ingest in that scope (a `content` entry).
fn room_scope_has_garden_content(reduced: &ReducerState, nav: &ThreadNav) -> bool {
    match nav.scope() {
        ScopeId::Public => true,
        ScopeId::Room(_) => reduced.content_for_scope(&nav.scope()).is_some(),
    }
}

fn content_for_garden_view<'a>(reduced: &'a ReducerState, scope: &ScopeId) -> &'a ContentState {
    match scope {
        ScopeId::Public => reduced.public(),
        ScopeId::Room(_) => reduced.content_for_scope(scope).expect(
            "room garden only renders after room_scope_has_garden_content returned true",
        ),
    }
}

/// Ontology index — root-level paths. Private (UUID) roots are excluded.
pub async fn garden_index(
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let nav = ThreadNav::public();
    let child_rankings = {
        let reduced = state.reduced.read().await;
        build_children_rankings(reduced.public(), &ItemId::ontology_root())
    };

    let page = layout(
        "~/",
        "view-ontology view-ontology-light",
        html! {
            @let root_path = OntologyPath::root();
            nav class="breadcrumb" { (bc_path(&root_path)) }
            h2 { "paths" }
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
            (cli_panel(&["npx slugsocial garden tree"]))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
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
    let parent = ItemId::parse("https://.").unwrap();
    let child_rankings = {
        let reduced = state.reduced.read().await;
        build_children_rankings(reduced.public(), &parent)
    };

    let page = layout(
        "-/",
        "view-ontology view-ontology-light",
        html! {
            nav class="breadcrumb" { (bc_path_external(&ext_path)) }
            h2 { "external paths" }
            p class="muted" { "Items outside slug.social use the " code { "-/" } " prefix (same role as " code { "~/" } ")." }
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
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
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
    let path = ExternalOntologyPath::from_input(&path);
    render_scope_view(
        state,
        GardenBrowsePath::External(path),
        ThreadNav::public(),
        jar,
        uri,
    )
    .await
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
    let parent = ItemId::parse("https://.").unwrap();
    let child_rankings = build_children_rankings(
        content_for_garden_view(&reduced, &nav.scope()),
        &parent,
    );
    drop(reduced);

    let page = layout(
        "-/",
        "view-ontology view-ontology-light",
        html! {
            nav class="breadcrumb" { (scoped_bc_path_external(&ext_path, &nav)) }
            h2 { "external paths" }
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
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
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
    let path = ExternalOntologyPath::from_input(&path);
    render_scope_view(state, GardenBrowsePath::External(path), nav, jar, uri).await
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

#[derive(Debug, Clone)]
struct SiblingRank {
    position: usize,
    component_size: usize,
    sibling_total: usize,
}

#[derive(Debug, Clone)]
struct RankHistoryEntryView {
    ts: i64,
    scope_rank: usize,
    scope_total: usize,
    scope_rank_delta: i32,
    thread: String,
    /// 0-based index as [`crate::html::forum::ingest::thread_post_index_in_scope`] / `/t/tag/N`.
    thread_post_index: usize,
    caused_by: Vec<crate::reducer::VoteData>,
}

#[derive(Debug, Clone)]
struct ItemPageViewModel {
    item: String,
    body: Option<String>,
    sibling_rank: Option<SiblingRank>,
    /// False at the tilde ontology root (`~/`): sibling-rank footnote does not apply.
    item_has_parent: bool,
    child_rankings: ChildrenRankings,
    rank_history: Vec<RankHistoryEntryView>,
    /// Forum threads that mention or vote on this item.
    threads: Vec<String>,
}

fn build_sibling_rank(
    reduced: &crate::reducer::ReducerState,
    scope: &ScopeId,
    item: &ItemId,
) -> Option<SiblingRank> {
    let item = item.clone().normalized_storage();
    let content = content_for_garden_view(reduced, scope);
    let group = &content.ranking_group;
    let parent = item.parent()?.normalized_storage();
    let siblings: Vec<ItemId> = content
        .item_children
        .get(&parent)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    if siblings.is_empty() {
        return None;
    }

    let sibling_total = siblings.len();
    let scoped_idxs: Vec<usize> = siblings
        .iter()
        .filter_map(|it| group.item_to_idx.get(it).copied())
        .collect();
    if scoped_idxs.is_empty() {
        return None;
    }
    let current_idx = *group.item_to_idx.get(&item)?;
    if !scoped_idxs.contains(&current_idx) {
        return None;
    }

    let local_to_global: Vec<usize> = scoped_idxs.clone();
    let global_to_local: std::collections::HashMap<usize, usize> = scoped_idxs
        .iter()
        .enumerate()
        .map(|(local, global)| (*global, local))
        .collect();
    let current_local = *global_to_local.get(&current_idx)?;
    let (comps_local, _) = connected_components_from_voted_pairs(
        scoped_idxs.len(),
        group.voted_pairs.iter().filter_map(|(i, j)| {
            let li = global_to_local.get(i).copied()?;
            let lj = global_to_local.get(j).copied()?;
            Some((li, lj))
        }),
    );
    let containing_comp = comps_local
        .iter()
        .find(|comp| comp.contains(&current_local))?;
    let comp_global: Vec<usize> = containing_comp
        .iter()
        .filter_map(|li| local_to_global.get(*li).copied())
        .collect();
    let ranked = ranked_items_subset(group, &comp_global, 10000, 1e-8);
    let position = ranked.iter().position(|r| r.item == item)? + 1;
    Some(SiblingRank {
        position,
        component_size: ranked.len(),
        sibling_total,
    })
}

fn build_rank_history(
    reduced: &crate::reducer::ReducerState,
    scope: &ScopeId,
    item: &str,
) -> Vec<RankHistoryEntryView> {
    let content = content_for_garden_view(reduced, scope);
    let item_key = ItemId::parse(item).unwrap_or_else(|| ItemId::opaque(item.to_string()));
    let entries = match content.rank_history.get(&item_key) {
        None => return vec![],
        Some(e) => e,
    };
    entries.iter().map(|e| {
        // Resolve caused_by: votes from this ingest that directly touched this item.
        let caused_by: Vec<crate::reducer::VoteData> = reduced.ingests_by_id
            .get(&e.post_id)
            .and_then(|ing| crate::dsl::parse_full(&ing.raw).ok())
            .map(|doc| {
                doc.statements.into_iter().filter_map(|s| {
                    if let crate::dsl::Stmt::Vote { item1, item2, ratio_left, ratio_right, explanation } = s {
                        let a_str = crate::canonical_path::canonicalize_item(&item1);
                        let b_str = crate::canonical_path::canonicalize_item(&item2);
                        if a_str == item || b_str == item {
                            Some(crate::reducer::VoteData {
                                ts: e.ts,
                                a: ItemId::parse(&a_str).unwrap_or_else(|| ItemId::opaque(a_str)),
                                b: ItemId::parse(&b_str).unwrap_or_else(|| ItemId::opaque(b_str)),
                                ratio_left, ratio_right,
                                body: explanation,
                                principal: reduced.ingests_by_id.get(&e.post_id)
                                    .map(|ing| ing.principal.clone())
                                    .unwrap_or_default(),
                                delegate: reduced.ingests_by_id.get(&e.post_id).and_then(|ing| ing.delegate.clone()),
                                thread_tag: e.thread.clone(),
                            })
                        } else { None }
                    } else { None }
                }).collect()
            })
            .unwrap_or_default();

        let thread_post_index =
            reduced.thread_post_index_chronological(scope, &e.thread, &e.post_id);

        RankHistoryEntryView {
            ts: e.ts,
            scope_rank: e.scope_rank,
            scope_total: e.scope_total,
            scope_rank_delta: e.scope_rank_delta,
            thread: e.thread.clone(),
            thread_post_index,
            caused_by,
        }
    }).collect()
}

fn build_item_page_view_model(
    reduced: &crate::reducer::ReducerState,
    scope: &ScopeId,
    item: &str,
) -> ItemPageViewModel {
    let content = content_for_garden_view(reduced, scope);
    let item_key = ItemId::parse(item)
        .unwrap_or_else(|| ItemId::parse("~/").unwrap())
        .normalized_storage();
    let item_has_parent = item_key.parent().is_some();
    let child_rankings = build_children_rankings(content, &item_key);

    let rank_history = build_rank_history(reduced, scope, item_key.as_str());

    let mut threads: Vec<String> = content
        .item_threads
        .get(&item_key)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    threads.sort();

    ItemPageViewModel {
        item: item_key.as_str().to_string(),
        body: content
            .item_bodies
            .get(&item_key)
            .cloned()
            .or_else(|| reduced.public().item_bodies.get(&item_key).cloned()),
        sibling_rank: build_sibling_rank(reduced, scope, &item_key),
        item_has_parent,
        child_rankings,
        rank_history,
        threads,
    }
}

async fn render_scope_view(
    state: AppState,
    browse: GardenBrowsePath,
    nav: ThreadNav,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    let scope = nav.scope();
    let model = {
        let reduced = state.reduced.read().await;
        build_item_page_view_model(&reduced, &scope, browse.item())
    };
    let thread_href = |tag: &str| nav.thread_url(tag);
    let external_empty_body = browse.is_external() && model.body.is_none();
    let cli_path_arg = item_display_path(&model.item);

    let page = layout(
        &item_display_path(&model.item),
        "view-ontology view-ontology-light",
        html! {
            nav class="breadcrumb" { (scoped_bc_path_for(&browse, &nav)) }
            section class="ont-item-shell" {
                header class="ont-item-meta" {
                    span class="ont-item-title" { (item_display_path(&model.item)) }
                    @if let Some(rank) = &model.sibling_rank {
                        span class="ont-rank-badge" {
                            (format!("#{} of {}", rank.position, rank.component_size))
                        }
                        span class="muted" { (format!("({} siblings)", rank.sibling_total)) }
                    } @else if model.item_has_parent {
                        span class="muted" { "unranked among siblings" }
                    }
                }
                @if let Some(body) = &model.body {
                    div class="ont-item-content" {
                        (render_linkified_with_embeds_in_scope(body, nav.garden_root_url()))
                    }
                } @else if external_empty_body {
                    div class="ont-item-content ont-external-empty" {
                        p { "This is an external scope." }
                        p class="muted" {
                            button type="button" disabled { "Kick off an Agent Run to import and rank items" }
                        }
                    }
                } @else {
                    div class="ont-item-content" { p class="muted" { "no body yet" } }
                }
            }

            @if !model.rank_history.is_empty() {
                details class="ont-rank-history" {
                    summary {
                        "vote history "
                        span class="muted" { (format!("({} events)", model.rank_history.len())) }
                    }
                    @let now = now_ms();
                    @let n = model.rank_history.len();
                    @for (i, e) in model.rank_history.iter().enumerate() {
                        @let label = if i == 0 { " — entered" } else if i == n - 1 { " — current" } else { "" };
                        @let delta_str = match e.scope_rank_delta.cmp(&0) {
                            std::cmp::Ordering::Less    => format!(" ↑{}", e.scope_rank_delta.unsigned_abs()),
                            std::cmp::Ordering::Greater => format!(" ↓{}", e.scope_rank_delta),
                            std::cmp::Ordering::Equal   => String::new(),
                        };
                        @let hover = timeago::rfc3339_utc(e.ts);
                        @let ago = timeago::timeago(now, e.ts);
                        div class="rank-history-entry" {
                            div class="rank-history-meta" title=(hover) {
                                span class="rank-history-pos" {
                                    (format!("#{} of {}{}", e.scope_rank, e.scope_total, delta_str))
                                }
                                " · "
                                span class="muted" { (ago) (label) }
                                " · "
                                a href=(thread_href(&e.thread)) { "#" (e.thread) }
                                " "
                                a href=(format!("{}/{}", thread_href(&e.thread), e.thread_post_index)) {
                                    span class="muted" { "post #" (e.thread_post_index) }
                                }
                            }
                            @if e.caused_by.is_empty() {
                                div class="rank-history-cause muted" { "transitive — shifted by votes elsewhere in the graph" }
                            } @else {
                                @for v in &e.caused_by {
                                    @let pct = ratio_pct(v.ratio_left, v.ratio_right);
                                    @let left_class = if v.a.as_str() == model.item { "ratio-left current" } else { "ratio-left" };
                                    @let right_class = if v.b.as_str() == model.item { "ratio-right current" } else { "ratio-right" };
                                    div class="rank-history-vote" {
                                        div class="ont-vote-header" {
                                            a class="item-link" href=(item_href(v.a.as_str(), &nav)) { code { (item_code_label(v.a.as_str())) } }
                                            span class="vote-ratio" { (format!("{}:{}", v.ratio_left, v.ratio_right)) }
                                            a class="item-link" href=(item_href(v.b.as_str(), &nav)) { code { (item_code_label(v.b.as_str())) } }
                                        }
                                        div class="ratio-bar" {
                                            div class=(left_class) style={(format!("width: {:.3}%;", pct))} {}
                                            div class=(right_class) style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                                        }
                                        @if !v.body.is_empty() {
                                            div class="ont-vote-body" { (v.body) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            @if !model.threads.is_empty() {
                div class="ont-item-threads" {
                    span class="muted" { "discussed in " }
                    @for (i, tag) in model.threads.iter().enumerate() {
                        @if i > 0 { span class="muted" { " · " } }
                        a href=(thread_href(tag)) { "#" (tag) }
                    }
                }
            }

            section class="ont-tab-panel ont-tab-panel-children" {
                h3 { "ranked child groups" }
                @if model.child_rankings.component_rankings.is_empty() {
                    p class="muted" { "no voted pairs yet in this scope" }
                } @else {
                    @for (ci, comp) in model.child_rankings.component_rankings.iter().enumerate() {
                        div class="ont-group-shell" {
                            div class="ont-group-meta" {
                                (format!("ordering {} items={} pairs={}", ci + 1, comp.ranked.len(), comp.pairs))
                            }
                            ol class="ont-ranking-list" {
                                @for r in comp.ranked.iter() {
                                    @let item_url = item_href(r.item.as_str(), &nav);
                                    @let score_str = format!("{:.3}", r.score);
                                    li {
                                        a class="item-link" href=(item_url) { code { (item_display_path(r.item.as_str())) } }
                                        span class="ont-rank-score" { (score_str) }
                                    }
                                }
                            }
                        }
                    }
                }

                @if !model.child_rankings.unranked_items.is_empty() {
                    div class="ont-group-shell ont-group-unsorted" {
                        div class="ont-group-meta" { "unranked" }
                        ul class="ont-group-list" {
                            @for name in &model.child_rankings.unranked_items {
                                li {
                                    @let href = item_href(name.as_str(), &nav);
                                    a class="item-link" href=(href) { code { (item_display_path(name.as_str())) } }
                                }
                            }
                        }
                    }
                }
            }
            @let cli = match &scope {
                ScopeId::Public => format!("npx slugsocial public garden body {}", cli_path_arg),
                ScopeId::Room(room_id) => format!("npx slugsocial private {room_id} garden body {}", cli_path_arg),
            };
            (cli_panel(std::slice::from_ref(&cli)))
        },
        None,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );

    Html(page.into_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::build_item_page_view_model;
    use crate::{
        events::{Event, Ingest},
        reducer::{ReducerState, ScopeId},
    };

    fn apply_ingest(state: &mut ReducerState, ts: i64, raw: &str) {
        state.apply_event(Event::Ingest(Ingest {
            ts,
            id: format!("ing-{ts}"),
            raw: raw.to_string(),
            principal: "testuser".to_string(),
            delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
            room_id: "public".to_string(),
            thread_tag: String::new(),
        }));
    }

    fn apply_ingest_room(state: &mut ReducerState, ts: i64, room_id: &str, raw: &str) {
        state.apply_event(Event::Ingest(Ingest {
            ts,
            id: format!("ing-{ts}"),
            raw: raw.to_string(),
            principal: "testuser".to_string(),
            delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
            room_id: room_id.to_string(),
            thread_tag: String::new(),
        }));
    }

    #[test]
    fn item_page_model_includes_body_and_unranked_without_votes() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             ~/topic {topic body}\n\
             ~/topic/a {alpha}\n\
             ~/topic/b {beta}\n",
        );

        let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a");
        assert_eq!(model.body.as_deref(), Some("alpha"));
        assert!(model.sibling_rank.is_none());
        assert!(model.child_rankings.component_rankings.is_empty());
    }

    #[test]
    fn item_page_model_computes_sibling_rank_in_component() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             ~/topic {topic body}\n\
             ~/topic/a {alpha}\n\
             ~/topic/b {beta}\n\
             ~/topic/c {gamma}\n\
             ~/topic/a 2:1 ~/topic/b {a beats b}\n",
        );

        let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a");
        let rank = model.sibling_rank.expect("expected sibling rank");
        assert_eq!(rank.position, 1);
        assert_eq!(rank.component_size, 2);
        assert_eq!(rank.sibling_total, 3);
    }

    #[test]
    fn item_page_model_builds_ranked_child_components() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             ~/topic {root}\n\
             ~/topic/a {alpha}\n\
             ~/topic/b {beta}\n\
             ~/topic/a 3:1 ~/topic/b {a beats b}\n\
             ~/topic/kid1 {k1}\n\
             ~/topic/kid2 {k2}\n\
             ~/topic/kid1/leaf {leaf}\n",
        );

        let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic");
        assert_eq!(model.child_rankings.component_rankings.len(), 1);
        assert_eq!(model.child_rankings.component_rankings[0].pairs, 1);
        let names: Vec<&str> = model.child_rankings.component_rankings[0]
            .ranked
            .iter()
            .map(|r| r.item.as_str())
            .collect();
        assert_eq!(names, vec!["https://slug.social/~/topic/a", "https://slug.social/~/topic/b"]);
        use crate::path_types::ItemId;
        assert!(
            model.child_rankings.unranked_items.contains(&ItemId::parse("https://slug.social/~/topic/kid1").unwrap())
                || model.child_rankings.unranked_items.contains(&ItemId::parse("https://slug.social/~/topic/kid2").unwrap())
        );
    }

    #[test]
    fn item_page_room_scope_root_lists_top_level_children() {
        let mut reduced = ReducerState::default();
        apply_ingest_room(
            &mut reduced,
            1,
            "9ab12cd/my-room",
            "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t1 {a}\n~/t2 {b}\n",
        );
        use crate::path_types::ItemId;
        let root = ItemId::ontology_root();
        let model = build_item_page_view_model(
            &reduced,
            &ScopeId::Room("9ab12cd/my-room".to_string()),
            root.as_str(),
        );
        assert!(!model.item_has_parent);
        assert_eq!(model.child_rankings.unranked_items.len(), 2);
        let set: std::collections::HashSet<&str> = model
            .child_rankings
            .unranked_items
            .iter()
            .map(|u| u.as_str())
            .collect();
        assert!(set.contains("https://slug.social/~/t1"));
        assert!(set.contains("https://slug.social/~/t2"));
    }

    /// Top-level `~/a` vs `~/b` votes form one ranked component under the ontology root.
    #[test]
    fn item_page_room_scope_root_shows_ranked_child_group() {
        let mut reduced = ReducerState::default();
        apply_ingest_room(
            &mut reduced,
            1,
            "9ab12cd/my-room",
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             ~/a {a}\n~/b {b}\n~/a 2:1 ~/b {because}\n",
        );
        use crate::path_types::ItemId;
        let root = ItemId::ontology_root();
        let model = build_item_page_view_model(
            &reduced,
            &ScopeId::Room("9ab12cd/my-room".to_string()),
            root.as_str(),
        );
        assert_eq!(model.child_rankings.component_rankings.len(), 1);
        assert_eq!(model.child_rankings.component_rankings[0].pairs, 1);
        let names: Vec<&str> = model.child_rankings.component_rankings[0]
            .ranked
            .iter()
            .map(|r| r.item.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["https://slug.social/~/a", "https://slug.social/~/b"]
        );
        assert!(model.child_rankings.unranked_items.is_empty());
    }

    /// Legacy `https://slug.social/~/` spelling still resolves children under the real root key.
    #[test]
    fn item_page_model_normalizes_legacy_tilde_root_storage_url() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n~/x {x}\n",
        );
        let model = build_item_page_view_model(
            &reduced,
            &ScopeId::Public,
            "https://slug.social/~/",
        );
        assert_eq!(model.child_rankings.unranked_items.len(), 1);
        assert_eq!(
            model.child_rankings.unranked_items[0].as_str(),
            "https://slug.social/~/x"
        );
    }
}
