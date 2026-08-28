use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::{
    api::optional_principal,
    canonical_path::canonicalize_tag,
    form_template::template_json_compact,
    html::{
        forum::ThreadNav,
        layout_full_bleed_chromeless,
        format_ratio, ratio_pct, render_item_body_in_scope,
        theme_from_jar, theme_next_from_uri,
        user_can_post_room,
        ui_action::UI_RPC_FIELD,
        JsBuilder,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    reducer::{ContentState, ScopeId},
    scope_rank::{comparable_items, suggest_next_pair_in_pool},
    state::AppState,
};

use super::{
    access::{content_for_garden_view, room_not_found_page, room_scope_has_garden_content, user_can_view_room},
    item::{item_display_path, login_href_with_next},
};

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
pub(super) fn canonical_edge_items(a: &ItemId, b: &ItemId) -> (ItemId, ItemId) {
    let ac = a.clone().normalized_storage();
    let bc = b.clone().normalized_storage();
    if ac.as_str() <= bc.as_str() {
        (ac, bc)
    } else {
        (bc, ac)
    }
}

/// All votes whose endpoints are exactly this unordered pair (unsorted).
pub(super) fn edge_vote_entries_for_pair(
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

pub(super) fn ratios_for_compare_page(
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
    if sum <= 0.0 {
        0.5
    } else {
        l / sum
    }
}

/// Stronger preference for **`page_left` first**; ties **newer first**.
pub(super) fn sort_votes_for_compare_display(
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
pub(super) fn edge_vote_count_for_pair(content: &ContentState, a: &ItemId, b: &ItemId) -> usize {
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
                    @let ratio_label = format_ratio(r_left, r_right);
                    @let pct = ratio_pct(r_left, r_right);
                    @let row_tip = format!(
                        "{} counts toward {} (left of bar) vs {} (right of bar); #{} · @{}",
                        ratio_label,
                        legend_left,
                        legend_right,
                        v.thread_tag,
                        v.principal,
                    );
                    li class="vote-edge-history-row" title=(row_tip) {
                        div class="vote-edge-meta" {
                            span class="vote-edge-ratio" { (ratio_label) }
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

/// After a successful vote post: refresh edge history (no in-page preview card).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn vote_compare_post_success_js(
    state: &AppState,
    nav: &ThreadNav,
    _room_wire: &str,
    _thread_tag: &str,
    left: &ItemId,
    right: &ItemId,
    pool: Option<&ItemId>,
    _post_id: &str,
    _post_idx: Option<usize>,
) -> String {
    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    let edge_history = vote_edge_history_markup(content, left, right);
    let next_pair = suggest_next_vote_pair(content, left, right, pool);
    let nav_markup = vote_compare_nav_markup(nav, next_pair.as_ref(), pool);
    drop(reduced);
    JsBuilder::new()
        .morph_inner_selector("#vote-edge-history-region", edge_history)
        .morph_selector(".vote-compare-nav", nav_markup)
        .qs("#vote-compare-form")
        .reset()
        .build()
}

pub(super) fn vote_compare_href(
    nav: &ThreadNav,
    left: &ItemId,
    right: &ItemId,
    thread_override: Option<&str>,
    pool: Option<&ItemId>,
) -> String {
    let left_dp = left.display_path();
    let right_dp = right.display_path();
    let left_q = urlencoding::encode(&left_dp);
    let right_q = urlencoding::encode(&right_dp);
    let mut base = format!(
        "{}/vote?left={}&right={}",
        nav.room_path_prefix_for_vote_compare(),
        left_q,
        right_q
    );
    if let Some(t) = thread_override.filter(|s| !s.is_empty()) {
        base = format!("{}&thread={}", base, urlencoding::encode(t));
    }
    if let Some(p) = pool {
        let pool_dp = p.display_path();
        base = format!("{}&pool={}", base, urlencoding::encode(&pool_dp));
    }
    base
}

pub(super) fn vote_pool_href(nav: &ThreadNav, pool_item_str: &str) -> String {
    let display = ItemId::parse(pool_item_str)
        .map(|i| i.display_path())
        .unwrap_or_else(|| pool_item_str.to_string());
    format!(
        "{}/vote?pool={}",
        nav.room_path_prefix_for_vote_compare(),
        urlencoding::encode(&display)
    )
}

fn vote_compare_nav_markup(
    nav: &ThreadNav,
    next_pair: Option<&(ItemId, ItemId)>,
    pool: Option<&ItemId>,
) -> maud::Markup {
    let next_pair_href = next_pair.map(|(nl, nr)| vote_compare_href(nav, nl, nr, None, pool));
    html! {
        div class="vote-compare-nav" {
            @if let Some(href) = &next_pair_href {
                a class="vote-compare-next" data-testid="vote-next-pair" href=(href) { "next pair" }
            } @else {
                span class="vote-compare-next is-disabled" { "no next pair" }
            }
        }
    }
}

pub(super) fn suggest_next_vote_pair(
    content: &ContentState,
    current_left: &ItemId,
    current_right: &ItemId,
    pool_parent: Option<&ItemId>,
) -> Option<(ItemId, ItemId)> {
    let pool: Vec<ItemId> = if let Some(parent) = pool_parent {
        content.members_of(&parent.ontology_leaf())
    } else {
        content
            .shared_scopes(
                &current_left.ontology_leaf(),
                &current_right.ontology_leaf(),
            )
            .into_iter()
            .next()
            .map(|scope| content.members_of(&scope))
            .unwrap_or_default()
    };
    let pool = comparable_items(content, pool);
    if pool.len() < 2 {
        return None;
    }
    suggest_next_pair_in_pool(
        &content.ranking_group,
        &pool,
        Some((current_left, current_right)),
    )
}

pub(super) fn vote_compare_item_card(
    nav: &ThreadNav,
    item: &ItemId,
    body: Option<&String>,
    side_class: &str,
    item_bodies: Option<&HashMap<ItemId, String>>,
) -> maud::Markup {
    html! {
        div class=(format!("vote-compare-side {side_class}")) {
            a class=(format!("vote-compare-item {side_class}")) href=(nav.garden_item_href(item)) {
                code { (item_display_path(item.as_str())) }
            }
            @if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
                div class="vote-compare-item-body" {
                    (render_item_body_in_scope(
                        body,
                        nav.garden_root_url(),
                        item_bodies,
                    ))
                }
            } @else {
                p class="muted vote-compare-item-body-empty" { "no body yet" }
            }
        }
    }
}
#[derive(Debug, Deserialize)]
pub struct VoteCompareQuery {
    #[serde(default)]
    pub left: Option<String>,
    #[serde(default)]
    pub right: Option<String>,
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
}

/// Public pairwise vote UI — `/vote?left=&right=&thread=`.
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
    let pool_id: Option<ItemId> = match q.pool.as_deref() {
        Some(p) => match ItemId::parse(p.trim()) {
            Some(i) => Some(i.normalized_storage().ontology_leaf()),
            None => return (StatusCode::BAD_REQUEST, "bad pool item").into_response(),
        },
        None => None,
    };

    let (left, right) = match (q.left.as_deref(), q.right.as_deref()) {
        (Some(l), Some(r)) => {
            let left = match ItemId::parse(l.trim()) {
                Some(i) => i.normalized_storage().ontology_leaf(),
                None => return (StatusCode::NOT_FOUND, "bad left item").into_response(),
            };
            let right = match ItemId::parse(r.trim()) {
                Some(i) => i.normalized_storage().ontology_leaf(),
                None => return (StatusCode::NOT_FOUND, "bad right item").into_response(),
            };
            if left == right {
                return (StatusCode::BAD_REQUEST, "items must differ").into_response();
            }
            (left, right)
        }
        (None, None) => {
            let Some(pool) = pool_id.as_ref() else {
                return (StatusCode::BAD_REQUEST, "provide left+right or pool").into_response();
            };
            let reduced = state.reduced.read().await;
            let content = content_for_garden_view(&reduced, &nav.scope());
            let children: Vec<ItemId> = content.members_of(&pool.ontology_leaf());
            let children = comparable_items(content, children);
            if children.len() < 2 {
                drop(reduced);
                return (
                    StatusCode::BAD_REQUEST,
                    "pool needs at least 2 direct items with bodies; folder paths are scopes, not vote targets",
                )
                    .into_response();
            }
            let pair = suggest_next_pair_in_pool(&content.ranking_group, &children, None);
            drop(reduced);
            match pair {
                Some(p) => p,
                None => return (StatusCode::BAD_REQUEST, "no pairs available in pool").into_response(),
            }
        }
        _ => return (StatusCode::BAD_REQUEST, "provide both left and right, or just pool").into_response(),
    };

    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    let viewer = optional_principal(&headers, &jar, &reduced);
    let logged_in = viewer.is_some();
    let can_post = match &nav.scope() {
        ScopeId::Public => logged_in,
        ScopeId::Room(rid) => viewer
            .as_ref()
            .map(|u| user_can_post_room(&reduced, rid, u))
            .unwrap_or(false),
    };
    // Guests see the same compose UI; submitting VoteComparePost redirects to
    // `/login?next=<this pair URL>` so OAuth returns them to the shared matchup.
    let show_vote_form = can_post || !logged_in;
    let auto_thread = q
        .thread
        .as_ref()
        .map(|t| canonicalize_tag(t))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| pick_autothread_for_vote_pair(content, &left, &right));
    let thread_tags = vote_thread_tags_for_pair(content, &left, &right);
    let edge_history = vote_edge_history_markup(content, &left, &right);
    let left_body = content.item_bodies.get(&left).cloned();
    let right_body = content.item_bodies.get(&right).cloned();
    let item_bodies_for_cards = content.item_bodies.clone();
    let next_pair = suggest_next_vote_pair(content, &left, &right, pool_id.as_ref());
    drop(reduced);

    let title = format!(
        "vote — {} vs {}",
        item_display_path(left.as_str()),
        item_display_path(right.as_str())
    );
    let next_path = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/vote".into());

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
        "pool": pool_id.as_ref().map(|p| p.as_str()),
        "form_action": "/ui",
    }))
    .expect("vote compare rpc json");

    let body = html! {
    section class="vote-compare-shell" {
        h2 { "compare" }
        div class="vote-compare-pair" {
            (vote_compare_item_card(
                &nav,
                &left,
                left_body.as_ref(),
                "vote-compare-left",
                Some(&item_bodies_for_cards),
            ))
            span class="vote-compare-vs" { "vs" }
            (vote_compare_item_card(
                &nav,
                &right,
                right_body.as_ref(),
                "vote-compare-right",
                Some(&item_bodies_for_cards),
            ))
        }
        (vote_compare_nav_markup(&nav, next_pair.as_ref(), pool_id.as_ref()))
        div id="vote-edge-history-region" {
            (edge_history)
        }
        @if show_vote_form && logged_in {
            form id="vote-compare-form" method="POST" action="/ui" data-draft-key=(format!("vote:{}/{}/{}", nav.room_wire, left.as_str(), right.as_str())) {
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
                input type="hidden" name="ratio_left" id="vote-ratio-left" value="1";
                input type="hidden" name="ratio_right" id="vote-ratio-right" value="1";
                p class="vote-ratio-readout-wrap" {
                    span class="vote-ratio-readout-label muted" { "ratio" }
                    " "
                    span id="vote-ratio-readout" class="vote-ratio-readout" aria-live="polite" { "1:1" }
                }
                label class="vote-compare-slider-label" {
                    span id="vote-slider-left-label" { (item_display_path(left.as_str())) }
                    input type="range" id="vote-preference-slider" min="0" max="100" value="50"
                        aria-valuemin="0" aria-valuemax="100" aria-valuetext="1:1";
                    span id="vote-slider-right-label" { (item_display_path(right.as_str())) }
                }
                label class="vote-explain-label" { "reason (required)" }
                textarea name="explanation" id="vote-explain" rows="5" placeholder="why this split?" required {}
                div id="vote-compare-errors" {}
                p { button type="submit" { "post vote" } }
            }
        } @else if show_vote_form {
            // Guest CTA is outside any form so click is a normal navigation to login.
            div id="vote-compare-form" class="vote-compare-guest" {
                p {
                    a class="vote-compare-login-cta" href=(login_href_with_next(&next_path)) { "post vote" }
                }
                p class="muted" { "you’ll log in, then return to this pair to cast your vote." }
            }
        } @else {
            p class="muted" { "you need post access in this room to vote on this pair." }
        }
    }
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout_full_bleed_chromeless(
        &title,
        "view-ontology view-ontology-light view-vote-compare view-vote-compare-fullscreen",
        body,
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}

