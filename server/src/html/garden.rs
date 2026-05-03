use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64_ENGINE};

use crate::{
    api::optional_principal,
    canonical_path::{canonicalize_item, canonicalize_tag},
    middleware::canonical_view_url,
    events::ThreadCapability,
    form_template::template_json_compact,
    html::{JsBuilder, ui_action::UI_RPC_FIELD, user_can_post_room},
    path_types::ItemId,
    reducer::{ContentState, ReducerState, ScopeId, scope_from_room_wire},
    scope_rank::{ChildrenRankings, build_children_rankings},
    state::AppState,
    timeago,
};

use super::{
    bc_path, bc_path_external, bc_segment,
    breadcrumb_path::{ExternalOntologyPath, OntologyPath},
    cli_panel,
    forum::{ThreadNav, ingest_entry_markup},
    layout, layout_full_bleed_chromeless, now_ms, ratio_pct, render_linkified_with_embeds_in_scope,
    theme_from_jar, theme_next_from_uri,
};

/// `GET /vote/compare` — pairs `left` / `right` query params with optional `thread`.
pub(crate) const GARDEN_PIN_COOKIE: &str = "slug_garden_pin";
const PIN_COOKIE_SEP: char = '\x1f';

fn garden_layout_meta(nav: &ThreadNav) -> (String, String) {
    (nav.room_wire.clone(), nav.garden_root_url().to_string())
}

/// Parse `slug_garden_pin`: base64(`room` + US + item storage), or legacy unencoded form.
fn pinned_item_from_jar(jar: &CookieJar) -> Option<(String, ItemId)> {
    let c = jar.get(GARDEN_PIN_COOKIE)?;
    let v = c.value();
    let raw: String = if let Ok(bytes) = B64_ENGINE.decode(v) {
        String::from_utf8(bytes).ok()?
    } else {
        v.to_string()
    };
    let (room, rest) = raw.split_once(PIN_COOKIE_SEP)?;
    let item = ItemId::parse(rest.trim())?;
    Some((room.trim().to_string(), item.normalized_storage()))
}

pub(crate) fn encode_pin_cookie_value(room: &str, item_storage: &str) -> String {
    let raw = format!("{room}{PIN_COOKIE_SEP}{item_storage}");
    B64_ENGINE.encode(raw.as_bytes())
}

fn pick_autothread_for_vote_pair(content: &ContentState, a: &ItemId, b: &ItemId) -> String {
    let cands: HashSet<String> = content
        .item_threads
        .get(a)
        .into_iter()
        .chain(content.item_threads.get(b))
        .flat_map(|s| s.iter().cloned())
        .collect();
    if cands.is_empty() {
        return "vote".to_string();
    }
    let mut v: Vec<String> = cands.into_iter().collect();
    v.sort();
    canonicalize_tag(&v[0])
}

/// Canonical unordered pair: lexicographic by storage string (stable edge identity).
fn canonical_edge_items(a: &ItemId, b: &ItemId) -> (ItemId, ItemId) {
    let ac = a.clone().normalized_storage();
    let bc = b.clone().normalized_storage();
    if ac.as_str() <= bc.as_str() {
        (ac, bc)
    } else {
        (bc, ac)
    }
}

/// All votes whose endpoints are exactly this unordered pair (unsorted).
fn edge_vote_entries_for_pair(
    content: &ContentState,
    a: &ItemId,
    b: &ItemId,
) -> Vec<crate::reducer::VoteData> {
    let (lo, hi) = canonical_edge_items(a, b);
    let lo_s = lo.as_str();
    let hi_s = hi.as_str();
    content
        .item_votes
        .get(&lo)
        .into_iter()
        .flat_map(|q| q.iter())
        .filter(|v| {
            (v.a.as_str() == lo_s && v.b.as_str() == hi_s)
                || (v.a.as_str() == hi_s && v.b.as_str() == lo_s)
        })
        .cloned()
        .collect()
}

fn ratios_for_compare_page(
    v: &crate::reducer::VoteData,
    page_left: &ItemId,
    page_right: &ItemId,
) -> (i32, i32) {
    let pl = page_left.as_str();
    let pr = page_right.as_str();
    match (v.a.as_str(), v.b.as_str()) {
        (a, b) if a == pl && b == pr => (v.ratio_left, v.ratio_right),
        (a, b) if a == pr && b == pl => (v.ratio_right, v.ratio_left),
        _ => (v.ratio_left, v.ratio_right),
    }
}

fn left_share_normalized(ratio_left: i32, ratio_right: i32) -> f64 {
    let l = ratio_left.max(0) as f64;
    let r = ratio_right.max(0) as f64;
    let sum = l + r;
    if sum <= 0.0 { 0.5 } else { l / sum }
}

/// Stronger preference for **`page_left` first**; ties **newer first**.
fn sort_votes_for_compare_display(
    mut votes: Vec<crate::reducer::VoteData>,
    page_left: &ItemId,
    page_right: &ItemId,
) -> Vec<crate::reducer::VoteData> {
    votes.sort_by(|va, vb| {
        let (ratio_left_a, ratio_right_a) = ratios_for_compare_page(va, page_left, page_right);
        let (ratio_left_b, ratio_right_b) = ratios_for_compare_page(vb, page_left, page_right);
        let sa = left_share_normalized(ratio_left_a, ratio_right_a);
        let sb = left_share_normalized(ratio_left_b, ratio_right_b);
        match sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal) {
            std::cmp::Ordering::Equal => vb.ts.cmp(&va.ts),
            o => o,
        }
    });
    votes
}

/// Number of vote ingests recorded for this unordered pair in `content` (same scope as ranking).
fn edge_vote_count_for_pair(content: &ContentState, a: &ItemId, b: &ItemId) -> usize {
    let (lo, hi) = canonical_edge_items(a, b);
    let lo_s = lo.as_str();
    let hi_s = hi.as_str();
    content
        .item_votes
        .get(&lo)
        .into_iter()
        .flat_map(|q| q.iter())
        .filter(|v| {
            (v.a.as_str() == lo_s && v.b.as_str() == hi_s)
                || (v.a.as_str() == hi_s && v.b.as_str() == lo_s)
        })
        .count()
}

fn vote_thread_tags_for_pair(content: &ContentState, a: &ItemId, b: &ItemId) -> Vec<String> {
    let set: HashSet<String> = content
        .item_threads
        .get(a)
        .into_iter()
        .chain(content.item_threads.get(b))
        .flat_map(|s| s.iter().cloned())
        .collect();
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v.into_iter().map(|t| canonicalize_tag(&t)).collect()
}

fn vote_edge_history_markup(content: &ContentState, left: &ItemId, right: &ItemId) -> maud::Markup {
    let votes = edge_vote_entries_for_pair(content, left, right);
    let votes = sort_votes_for_compare_display(votes, left, right);
    let legend_left = item_display_path(left.as_str());
    let legend_right = item_display_path(right.as_str());
    html! {
        @if votes.is_empty() {
            p class="muted vote-edge-empty" { "no votes on this pair in this scope yet" }
        } @else {
            h3 class="vote-edge-history-title" {
                "votes on this edge"
                span class="vote-edge-history-axis muted" { " · " (legend_left) " : " (legend_right) }
            }
            ul class="vote-edge-history" {
                @for v in &votes {
                    @let (r_left, r_right) = ratios_for_compare_page(v, left, right);
                    @let pct = ratio_pct(r_left, r_right);
                    @let row_tip = format!(
                        "{}:{} counts toward {} (left of bar) vs {} (right of bar); #{} · @{}",
                        r_left,
                        r_right,
                        legend_left,
                        legend_right,
                        v.thread_tag,
                        v.principal,
                    );
                    li class="vote-edge-history-row" title=(row_tip) {
                        div class="vote-edge-meta" {
                            span class="vote-edge-ratio" { (format!("{}:{}", r_left, r_right)) }
                            span class="muted" { " · #" (v.thread_tag) " · @" (v.principal) }
                        }
                        div class="ratio-bar vote-edge-bar" aria-hidden="true" {
                            div class="ratio-left" style={(format!("width: {:.3}%;", pct))} {}
                            div class="ratio-right" style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                        }
                        @if !v.body.trim().is_empty() {
                            div class="vote-edge-reason muted" { (v.body.trim()) }
                        }
                    }
                }
            }
        }
    }
}

/// After a successful vote post: morph the new card into `#vote-compare-preview` and refresh edge history.
pub(crate) async fn vote_compare_post_success_js(
    state: &AppState,
    nav: &ThreadNav,
    room_wire: &str,
    thread_tag: &str,
    left: &ItemId,
    right: &ItemId,
    post_id: &str,
    post_idx: Option<usize>,
) -> String {
    let reduced = state.reduced.read().await;
    let scope = scope_from_room_wire(room_wire);
    let Some(ing) = reduced.ingests_by_id.get(post_id).cloned() else {
        drop(reduced);
        return "console.warn('vote compare: new post not found');".to_string();
    };
    let idx = match post_idx {
        Some(i) => i,
        None => reduced
            .try_thread_post_index_chronological(&scope, thread_tag, post_id)
            .unwrap_or(0),
    };
    let viewer = None::<&str>;
    let now = now_ms();
    let content = content_for_garden_view(&reduced, &nav.scope());
    let edge_history = vote_edge_history_markup(content, left, right);
    let card = ingest_entry_markup(nav, thread_tag, idx, &ing, viewer, now, &reduced);
    drop(reduced);
    let mut b = JsBuilder::new();
    b = b.morph_inner_selector("#vote-compare-preview", card);
    b = b.morph_inner_selector("#vote-edge-history-region", edge_history);
    b.build()
}

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

fn vote_compare_href(
    nav: &ThreadNav,
    left: &ItemId,
    right: &ItemId,
    thread_override: Option<&str>,
) -> String {
    let left_q = urlencoding::encode(left.as_str());
    let right_q = urlencoding::encode(right.as_str());
    let base = format!(
        "{}/vote/compare?left={}&right={}",
        nav.room_path_prefix_for_vote_compare(),
        left_q,
        right_q
    );
    if let Some(t) = thread_override.filter(|s| !s.is_empty()) {
        format!("{}&thread={}", base, urlencoding::encode(t))
    } else {
        base
    }
}

fn ont_pin_vote_controls(
    nav: &ThreadNav,
    current_storage: &str,
    pinned_room_and_item: Option<&(String, ItemId)>,
    next_path: &str,
) -> maud::Markup {
    let room_wire = nav.room_wire.clone();
    let current = ItemId::parse(current_storage)
        .unwrap_or_else(|| ItemId::opaque(current_storage.to_string()));
    let pin_matches_scope = pinned_room_and_item
        .map(|(r, _)| r == nav.room_wire.as_str())
        .unwrap_or(false);
    let pinned_item = pinned_room_and_item
        .filter(|_| pin_matches_scope)
        .map(|(_, i)| i);

    let pin_rpc = template_json_compact(&json!({
        "action": "set_garden_pin",
        "clear": false,
        "room_wire": room_wire,
        "item_storage": current.as_str(),
        "next": next_path,
        "form_action": "/ui",
    }))
    .expect("pin rpc json");
    let unpin_rpc = template_json_compact(&json!({
        "action": "set_garden_pin",
        "clear": true,
        "room_wire": "",
        "next": next_path,
        "form_action": "/ui",
    }))
    .expect("unpin rpc json");

    html! {
        div class="ont-item-pin-zone" data-garden-room=(room_wire.as_str()) {
            @if let Some(pi) = pinned_item {
                @if pi == &current {
                    form method="POST" action="/ui" data-navigate="full" class="ont-pin-form" {
                        input type="hidden" name=(UI_RPC_FIELD) value=(unpin_rpc);
                        button type="submit" class="ont-pin-btn ont-pin-btn-active" title="Unpin" aria-label="Unpin from HUD" {
                            span class="ont-pin-glyph" aria-hidden="true" { "📌" }
                        }
                    }
                } @else {
                    a class="ont-vote-compare-btn" href=(vote_compare_href(nav, pi, &current, None)) title="Compare and vote" {
                        span class="ont-vote-glyph" aria-hidden="true" { "⚖" }
                        span { "vote" }
                    }
                }
            } @else {
                form method="POST" action="/ui" data-navigate="full" class="ont-pin-form" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(pin_rpc);
                    button type="submit" class="ont-pin-btn" title="Pin to HUD" aria-label="Pin to corner" {
                        span class="ont-pin-glyph" aria-hidden="true" { "📌" }
                    }
                }
            }
        }
    }
}

fn child_row_pin_or_vote(
    nav: &ThreadNav,
    row_item: &ItemId,
    pinned_room_and_item: Option<&(String, ItemId)>,
    scope_content: &ContentState,
) -> maud::Markup {
    let pin_matches_scope = pinned_room_and_item
        .map(|(r, _)| r == nav.room_wire.as_str())
        .unwrap_or(false);
    let pinned_item = pinned_room_and_item
        .filter(|_| pin_matches_scope)
        .map(|(_, i)| i);

    html! {
        @if let Some(pi) = pinned_item {
            span class="ont-garden-child-actions" data-garden-room=(nav.room_wire.as_str()) {
                @if pi == row_item {
                    span class="ont-garden-pinned-here" title="Pinned" aria-label="Pinned" { "📌" }
                } @else {
                    @let nv = edge_vote_count_for_pair(scope_content, pi, row_item);
                    @let tip = format!(
                        "Compare and vote — {nv} pairwise vote{} in this scope for pinned vs this row",
                        if nv == 1 { "" } else { "s" },
                    );
                    @let aria = format!("Vote; {} pairwise {}", nv, if nv == 1 { "vote" } else { "votes" });
                    a class="ont-garden-vote-ico" href=(vote_compare_href(nav, pi, row_item, None)) title=(tip) aria-label=(aria) {
                        span class="ont-garden-vote-glyph" aria-hidden="true" { "⚖" }
                        span class="ont-garden-vote-count" { (format!("{}", nv)) }
                    }
                }
            }
        }
    }
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
        None,
        None,
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
        ScopeId::Room(_) => reduced
            .content_for_scope(scope)
            .expect("room garden only renders after room_scope_has_garden_content returned true"),
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

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

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
    let parent = ItemId::parse("https://.").unwrap();
    let child_rankings = {
        let reduced = state.reduced.read().await;
        build_children_rankings(reduced.public(), &parent)
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

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
    let child_rankings =
        build_children_rankings(content_for_garden_view(&reduced, &nav.scope()), &parent);
    drop(reduced);

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

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

/// One sibling link in the breadcrumb nav row (label is 1-based index within its ranking group).
#[derive(Debug, Clone)]
struct SiblingNavLink {
    path: String,
}

#[derive(Debug, Clone)]
struct SiblingNavGroup {
    links: Vec<SiblingNavLink>,
}

/// Siblings under the same parent, grouped like child rankings (components then isolates).
#[derive(Debug, Clone)]
struct SiblingNavBar {
    groups: Vec<SiblingNavGroup>,
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
    sibling_nav: Option<SiblingNavBar>,
    /// False at the tilde ontology root (`~/`): sibling-rank footnote does not apply.
    item_has_parent: bool,
    child_rankings: ChildrenRankings,
    rank_history: Vec<RankHistoryEntryView>,
    /// Forum threads that mention or vote on this item.
    threads: Vec<String>,
}

fn build_sibling_nav(
    reduced: &crate::reducer::ReducerState,
    scope: &ScopeId,
    current: &ItemId,
) -> Option<SiblingNavBar> {
    let current = current.clone().normalized_storage();
    let parent = current.parent()?.normalized_storage();
    let content = content_for_garden_view(reduced, scope);
    let rankings = build_children_rankings(content, &parent);
    let mut groups: Vec<SiblingNavGroup> = Vec::new();
    for comp in &rankings.component_rankings {
        let links: Vec<SiblingNavLink> = comp
            .ranked
            .iter()
            .map(|r| SiblingNavLink {
                path: r.item.clone().normalized_storage().to_storage_string(),
            })
            .collect();
        if !links.is_empty() {
            groups.push(SiblingNavGroup { links });
        }
    }
    if !rankings.unranked_items.is_empty() {
        let links: Vec<SiblingNavLink> = rankings
            .unranked_items
            .iter()
            .map(|u| SiblingNavLink {
                path: u.clone().normalized_storage().to_storage_string(),
            })
            .collect();
        groups.push(SiblingNavGroup { links });
    }
    let sibling_total: usize = groups.iter().map(|g| g.links.len()).sum();
    if sibling_total <= 1 {
        return None;
    }
    Some(SiblingNavBar { groups })
}

fn sibling_nav_markup(nav: &ThreadNav, bar: &SiblingNavBar, current_item: &str) -> maud::Markup {
    html! {
        nav class="breadcrumb ont-sibling-nav" aria-label="siblings under same parent" {
            @for (gi, group) in bar.groups.iter().enumerate() {
                @if gi > 0 {
                    span class="ont-sibling-nav-group-sep" aria-hidden="true" { "·" }
                }
                span class="ont-sibling-nav-group" {
                    @for (i, link) in group.links.iter().enumerate() {
                        @if i > 0 {
                            span class="bc-sep ont-sibling-nav-intra" aria-hidden="true" { " " }
                        }
                        @let n = i + 1;
                        @let href = item_href(&link.path, nav);
                        @let tip = item_display_path(&link.path);
                        @let is_current = link.path == current_item;
                        @if is_current {
                            a href=(href) title=(tip) aria-current="page" { "[" (n) "]" }
                        } @else {
                            a href=(href) title=(tip) { (n) }
                        }
                    }
                }
            }
        }
    }
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
    entries
        .iter()
        .map(|e| {
            // Resolve caused_by: votes from this ingest that directly touched this item.
            let caused_by: Vec<crate::reducer::VoteData> = reduced
                .ingests_by_id
                .get(&e.post_id)
                .and_then(|ing| crate::dsl::parse_full(&ing.raw).ok())
                .map(|doc| {
                    doc.statements
                        .into_iter()
                        .filter_map(|s| {
                            if let crate::dsl::Stmt::Vote {
                                item1,
                                item2,
                                ratio_left,
                                ratio_right,
                                explanation,
                            } = s
                            {
                                let a_str = crate::canonical_path::canonicalize_item(&item1);
                                let b_str = crate::canonical_path::canonicalize_item(&item2);
                                if a_str == item || b_str == item {
                                    Some(crate::reducer::VoteData {
                                        ts: e.ts,
                                        a: ItemId::parse(&a_str)
                                            .unwrap_or_else(|| ItemId::opaque(a_str)),
                                        b: ItemId::parse(&b_str)
                                            .unwrap_or_else(|| ItemId::opaque(b_str)),
                                        ratio_left,
                                        ratio_right,
                                        body: explanation,
                                        principal: reduced
                                            .ingests_by_id
                                            .get(&e.post_id)
                                            .map(|ing| ing.principal.clone())
                                            .unwrap_or_default(),
                                        delegate: reduced
                                            .ingests_by_id
                                            .get(&e.post_id)
                                            .and_then(|ing| ing.delegate.clone()),
                                        thread_tag: e.thread.clone(),
                                    })
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect()
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
        })
        .collect()
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
    let sibling_nav = build_sibling_nav(reduced, scope, &item_key);

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
        sibling_nav,
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
    let pin_ref = pinned_item_from_jar(&jar);
    let reduced = state.reduced.read().await;
    let model = build_item_page_view_model(&reduced, &scope, browse.item());
    let scope_content = content_for_garden_view(&reduced, &scope);
    let thread_href = |tag: &str| nav.thread_url(tag);
    let external_empty_body = browse.is_external() && model.body.is_none();
    let cli_path_arg = item_display_path(&model.item);
    let (garden_room, garden_prefix) = garden_layout_meta(&nav);
    let next_for_pin = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string());

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout(
        &item_display_path(&model.item),
        "view-ontology view-ontology-light",
        html! {
            nav class="breadcrumb" { (scoped_bc_path_for(&browse, &nav)) }
            @if let Some(ref bar) = model.sibling_nav {
                (sibling_nav_markup(&nav, bar, &model.item))
            }
            section class="ont-item-shell" data-garden-item=(model.item.as_str()) {
                header class="ont-item-meta" {
                    span class="ont-item-title" { (item_display_path(&model.item)) }
                    @if model.sibling_nav.is_none() && model.item_has_parent {
                        span class="muted ont-item-unranked-note" { "unranked among siblings" }
                    }
                    (ont_pin_vote_controls(&nav, &model.item, pin_ref.as_ref(), &next_for_pin))
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
                                    li data-garden-item=(r.item.as_str()) {
                                        (child_row_pin_or_vote(&nav, &r.item, pin_ref.as_ref(), scope_content))
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
                                li data-garden-item=(name.as_str()) {
                                    (child_row_pin_or_vote(&nav, name, pin_ref.as_ref(), scope_content))
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
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        Some(&garden_room),
        Some(&garden_prefix),
    );

    Html(page.into_string()).into_response()
}

#[derive(Debug, Deserialize)]
pub struct VoteCompareQuery {
    pub left: String,
    pub right: String,
    #[serde(default)]
    pub thread: Option<String>,
}

/// Public pairwise vote UI — `/vote/compare?left=&right=&thread=`.
pub async fn vote_compare_page(
    State(state): State<AppState>,
    Query(q): Query<VoteCompareQuery>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let nav = ThreadNav::public();
    vote_compare_inner(state, q, nav, headers, jar, uri).await
}

pub async fn room_vote_compare_page(
    State(state): State<AppState>,
    Path(room_key): Path<String>,
    Query(q): Query<VoteCompareQuery>,
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
    vote_compare_inner(state, q, nav, headers, jar, uri).await
}

async fn vote_compare_inner(
    state: AppState,
    q: VoteCompareQuery,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    let left = match ItemId::parse(q.left.trim()) {
        Some(i) => i.normalized_storage(),
        None => return (StatusCode::NOT_FOUND, "bad left item").into_response(),
    };
    let right = match ItemId::parse(q.right.trim()) {
        Some(i) => i.normalized_storage(),
        None => return (StatusCode::NOT_FOUND, "bad right item").into_response(),
    };
    if left == right {
        return (StatusCode::BAD_REQUEST, "items must differ").into_response();
    }

    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    let viewer = optional_principal(&headers, &jar, &reduced);
    let can_post = match &nav.scope() {
        ScopeId::Public => viewer.is_some(),
        ScopeId::Room(rid) => viewer
            .as_ref()
            .map(|u| user_can_post_room(&reduced, rid, u))
            .unwrap_or(false),
    };
    let auto_thread = q
        .thread
        .as_ref()
        .map(|t| canonicalize_tag(t))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| pick_autothread_for_vote_pair(content, &left, &right));
    let thread_tags = vote_thread_tags_for_pair(content, &left, &right);
    let edge_history = vote_edge_history_markup(content, &left, &right);
    drop(reduced);

    let title = format!(
        "vote — {} vs {}",
        item_display_path(left.as_str()),
        item_display_path(right.as_str())
    );
    let next_path = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/vote/compare".into());

    let rpc_json = template_json_compact(&json!({
        "action": "vote_compare_post",
        "room": nav.room_wire,
        "thread_tag": {"$form": "thread_tag"},
        "left_item": left.as_str(),
        "right_item": right.as_str(),
        "ratio_left": {"$form": "ratio_left"},
        "ratio_right": {"$form": "ratio_right"},
        "explanation": {"$form": "explanation"},
        "next": next_path,
        "form_action": "/ui",
    }))
    .expect("vote compare rpc json");

    let body = html! {
    h2 { "compare" }
    div class="vote-compare-pair" {
        a class="vote-compare-item" href=(nav.garden_item_href(&left)) {
            code { (item_display_path(left.as_str())) }
        }
        span class="vote-compare-vs" { "vs" }
        a class="vote-compare-item" href=(nav.garden_item_href(&right)) {
            code { (item_display_path(right.as_str())) }
        }
    }
    div id="vote-edge-history-region" {
        (edge_history)
    }
    @if can_post {
        form id="vote-compare-form" method="POST" action="/ui" {
            input type="hidden" name=(UI_RPC_FIELD) value=(rpc_json);
            div class="vote-thread-picker" {
                label class="vote-thread-picker-label" { "thread" }
                select id="vote-thread-select" name="thread_tag" aria-label="Thread to post vote into" {
                    @if thread_tags.is_empty() {
                        option value="vote" selected { "#vote" }
                    }
                    @for t in &thread_tags {
                        @if *t == auto_thread {
                            option value=(t) selected { "#" (t) }
                        } @else {
                            option value=(t) { "#" (t) }
                        }
                    }
                }
            }
            input type="hidden" name="ratio_left" id="vote-ratio-left" value="50";
            input type="hidden" name="ratio_right" id="vote-ratio-right" value="50";
            label class="vote-compare-slider-label" {
                span id="vote-slider-left-label" { (item_display_path(left.as_str())) }
                input type="range" id="vote-preference-slider" min="0" max="100" value="50"
                    aria-valuemin="0" aria-valuemax="100";
                span id="vote-slider-right-label" { (item_display_path(right.as_str())) }
            }
            label class="vote-explain-label" { "reason (required)" }
            textarea name="explanation" id="vote-explain" rows="5" placeholder="why this split?" required {}
            div id="vote-compare-errors" {}
            p { button type="submit" { "post vote" } }
        }
    } @else {
        p class="muted" { a href="/login" { "log in" } " to post this vote." }
    }
    };

    let page = layout_full_bleed_chromeless(
        &title,
        "view-ontology view-ontology-light view-vote-compare view-vote-compare-fullscreen",
        body,
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
    fn vote_edge_compare_sorts_rows_by_strength_for_page_left() {
        use super::{
            content_for_garden_view, edge_vote_entries_for_pair, ratios_for_compare_page,
            sort_votes_for_compare_display,
        };
        use crate::path_types::ItemId;
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             ~/topic {root}\n\
             ~/topic/a {alpha}\n\
             ~/topic/b {beta}\n\
             ~/topic/a 1:9 ~/topic/b {weak for a}\n\
             ~/topic/a 8:2 ~/topic/b {strong for a}\n",
        );
        let content = content_for_garden_view(&reduced, &ScopeId::Public);
        let page_left = ItemId::parse("~/topic/a").unwrap().normalized_storage();
        let page_right = ItemId::parse("~/topic/b").unwrap().normalized_storage();
        let raw = edge_vote_entries_for_pair(content, &page_left, &page_right);
        assert_eq!(raw.len(), 2);
        let sorted = sort_votes_for_compare_display(raw, &page_left, &page_right);
        let (r0, _) = ratios_for_compare_page(&sorted[0], &page_left, &page_right);
        assert_eq!(r0, 8, "stronger left weight should sort first");
        let (r1, _) = ratios_for_compare_page(&sorted[1], &page_left, &page_right);
        assert_eq!(r1, 1);
    }

    #[test]
    fn edge_vote_count_for_pair_matches_votes_for_edge_len() {
        use super::{
            content_for_garden_view, edge_vote_count_for_pair, edge_vote_entries_for_pair,
        };
        use crate::path_types::ItemId;
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             ~/topic {root}\n\
             ~/topic/a {alpha}\n\
             ~/topic/b {beta}\n\
             ~/topic/a 3:2 ~/topic/b {first vote}\n\
             ~/topic/b 2:3 ~/topic/a {second vote}\n",
        );
        let content = content_for_garden_view(&reduced, &ScopeId::Public);
        let a = ItemId::parse("~/topic/a").unwrap().normalized_storage();
        let b = ItemId::parse("~/topic/b").unwrap().normalized_storage();
        assert_eq!(
            edge_vote_count_for_pair(content, &a, &b),
            edge_vote_entries_for_pair(content, &a, &b).len()
        );
        assert_eq!(edge_vote_entries_for_pair(content, &a, &b).len(), 2);
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
        assert!(model.sibling_nav.is_some());
        assert!(model.child_rankings.component_rankings.is_empty());
    }

    #[test]
    fn item_page_model_computes_sibling_nav_in_component() {
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
        let nav = model.sibling_nav.expect("expected sibling nav");
        assert_eq!(nav.groups.len(), 2);
        assert_eq!(nav.groups[0].links.len(), 2);
        assert_eq!(nav.groups[1].links.len(), 1);
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
        assert_eq!(
            names,
            vec![
                "https://slug.social/~/topic/a",
                "https://slug.social/~/topic/b"
            ]
        );
        use crate::path_types::ItemId;
        assert!(
            model
                .child_rankings
                .unranked_items
                .contains(&ItemId::parse("https://slug.social/~/topic/kid1").unwrap())
                || model
                    .child_rankings
                    .unranked_items
                    .contains(&ItemId::parse("https://slug.social/~/topic/kid2").unwrap())
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
        let model =
            build_item_page_view_model(&reduced, &ScopeId::Public, "https://slug.social/~/");
        assert_eq!(model.child_rankings.unranked_items.len(), 1);
        assert_eq!(
            model.child_rankings.unranked_items[0].as_str(),
            "https://slug.social/~/x"
        );
    }
}
