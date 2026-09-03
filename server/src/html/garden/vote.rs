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
        format_ratio, forum::ThreadNav, layout_full_bleed_chromeless, ratio_pct,
        render_item_body_in_scope, theme_from_jar, theme_next_from_uri, ui_action::UI_RPC_FIELD,
        user_can_post_room, JsBuilder,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    reducer::{ContentState, ScopeId},
    scope_rank::{comparable_items, suggest_next_pair_in_pool},
    state::AppState,
};

use super::{
    access::{
        content_for_garden_view, room_not_found_page, room_scope_has_garden_content,
        user_can_view_room,
    },
    item::{item_display_path, item_href, login_href_with_next},
};

/// Unique element ids for a compare panel. Empty suffix is the `/vote` page.
#[derive(Debug, Clone, Default)]
pub(super) struct VoteCompareDomIds {
    pub suffix: Option<String>,
}

impl VoteCompareDomIds {
    pub(super) fn page() -> Self {
        Self { suffix: None }
    }

    pub(super) fn with_suffix(suffix: impl Into<String>) -> Self {
        let s = suffix.into();
        Self {
            suffix: (!s.is_empty()).then_some(s),
        }
    }

    fn suffixed(&self, base: &str) -> String {
        match &self.suffix {
            Some(s) if !s.is_empty() => format!("{base}-{s}"),
            _ => base.to_string(),
        }
    }

    pub(super) fn form_id(&self) -> String {
        self.suffixed("vote-compare-form")
    }

    pub(super) fn history_id(&self) -> String {
        self.suffixed("vote-edge-history-region")
    }

    pub(super) fn nav_id(&self) -> String {
        self.suffixed("vote-compare-nav")
    }

    pub(super) fn slider_id(&self) -> String {
        self.suffixed("vote-preference-slider")
    }

    pub(super) fn ratio_left_id(&self) -> String {
        self.suffixed("vote-ratio-left")
    }

    pub(super) fn ratio_right_id(&self) -> String {
        self.suffixed("vote-ratio-right")
    }

    pub(super) fn readout_id(&self) -> String {
        self.suffixed("vote-ratio-readout")
    }

    pub(super) fn errors_id(&self) -> String {
        self.suffixed("vote-compare-errors")
    }

    pub(super) fn thread_select_id(&self) -> String {
        self.suffixed("vote-thread-select")
    }

    pub(super) fn slider_left_label_id(&self) -> String {
        self.suffixed("vote-slider-left-label")
    }

    pub(super) fn slider_right_label_id(&self) -> String {
        self.suffixed("vote-slider-right-label")
    }

    pub(super) fn explain_id(&self) -> String {
        self.suffixed("vote-explain")
    }
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
    _next: &str,
    dom_suffix: Option<&str>,
) -> String {
    let ids = VoteCompareDomIds::with_suffix(dom_suffix.unwrap_or("").to_string());
    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    let edge_history = vote_edge_history_markup(content, left, right);
    let next_pair = suggest_next_vote_pair(content, left, right, pool);
    let nav_markup = vote_compare_nav_markup(nav, next_pair.as_ref(), pool, &ids.nav_id());
    drop(reduced);
    JsBuilder::new()
        .morph_inner_selector(&format!("#{}", ids.history_id()), edge_history)
        .morph_selector(&format!("#{}", ids.nav_id()), nav_markup)
        .qs(&format!("#{}", ids.form_id()))
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
    nav_id: &str,
) -> maud::Markup {
    let next_pair_href = next_pair.map(|(nl, nr)| vote_compare_href(nav, nl, nr, None, pool));
    html! {
        div id=(nav_id) class="vote-compare-nav" {
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
pub(super) struct VoteComparePanel<'a> {
    pub nav: &'a ThreadNav,
    pub left: &'a ItemId,
    pub right: &'a ItemId,
    pub left_body: Option<&'a String>,
    pub right_body: Option<&'a String>,
    pub item_bodies: Option<&'a HashMap<ItemId, String>>,
    pub pool: Option<&'a ItemId>,
    pub auto_thread: &'a str,
    pub thread_tags: &'a [String],
    pub edge_history: maud::Markup,
    pub next_pair: Option<&'a (ItemId, ItemId)>,
    pub next_path: &'a str,
    pub aspect_slug: Option<&'a str>,
    pub logged_in: bool,
    pub show_vote_form: bool,
    pub include_heading: bool,
    pub ids: &'a VoteCompareDomIds,
}

/// Pair cards + nav + history + vote form for `/vote`.
pub(super) fn vote_compare_panel_markup(p: VoteComparePanel<'_>) -> maud::Markup {
    let mut rpc_val = json!({
        "action": "vote_compare_post",
        "room": p.nav.room_wire,
        "thread_tag": {"$form": "thread_tag"},
        "left_item": p.left.as_str(),
        "right_item": p.right.as_str(),
        "ratio_left": {"$form": "ratio_left"},
        "ratio_right": {"$form": "ratio_right"},
        "explanation": {"$form": "explanation"},
        "next": p.next_path,
        "pool": p.pool.map(|q| q.as_str()),
        "form_action": "/ui",
    });
    if p.aspect_slug.is_some() {
        rpc_val["aspect"] = json!({"$form": "aspect"});
    }
    if let Some(suffix) = p.ids.suffix.as_deref().filter(|s| !s.is_empty()) {
        rpc_val["dom_suffix"] = json!(suffix);
    }
    let rpc_json = template_json_compact(&rpc_val).expect("vote compare rpc json");
    let form_id = p.ids.form_id();
    let history_id = p.ids.history_id();
    let nav_id = p.ids.nav_id();
    let slider_id = p.ids.slider_id();
    let ratio_left_id = p.ids.ratio_left_id();
    let ratio_right_id = p.ids.ratio_right_id();
    let readout_id = p.ids.readout_id();
    let errors_id = p.ids.errors_id();
    let thread_select_id = p.ids.thread_select_id();
    let slider_left_id = p.ids.slider_left_label_id();
    let slider_right_id = p.ids.slider_right_label_id();
    let explain_id = p.ids.explain_id();
    html! {
        section class="vote-compare-shell" {
            @if p.include_heading {
                h2 { "compare" }
            }
            div class="vote-compare-pair" {
                (vote_compare_item_card(
                    p.nav,
                    p.left,
                    p.left_body,
                    "vote-compare-left",
                    p.item_bodies,
                ))
                span class="vote-compare-vs" { "vs" }
                (vote_compare_item_card(
                    p.nav,
                    p.right,
                    p.right_body,
                    "vote-compare-right",
                    p.item_bodies,
                ))
            }
            (vote_compare_nav_markup(p.nav, p.next_pair, p.pool, &nav_id))
            div id=(history_id) {
                (p.edge_history)
            }
            @if p.show_vote_form && p.logged_in {
                form id=(form_id) class="vote-compare-form" method="POST" action="/ui" data-draft-key=(format!("vote:{}/{}/{}", p.nav.room_wire, p.left.as_str(), p.right.as_str())) {
                    input type="hidden" name=(UI_RPC_FIELD) value=(rpc_json);
                    @if let Some(aspect) = p.aspect_slug {
                        input type="hidden" name="aspect" value=(aspect);
                    }
                    div class="vote-thread-picker" {
                        label class="vote-thread-picker-label" { "thread" }
                        select id=(thread_select_id) name="thread_tag" aria-label="Thread to post vote into" {
                            @if p.thread_tags.is_empty() {
                                option value="vote" selected { "#vote" }
                            }
                            @for t in p.thread_tags {
                                @if t == p.auto_thread {
                                    option value=(t) selected { "#" (t) }
                                } @else {
                                    option value=(t) { "#" (t) }
                                }
                            }
                        }
                    }
                    input type="hidden" name="ratio_left" id=(ratio_left_id) value="1";
                    input type="hidden" name="ratio_right" id=(ratio_right_id) value="1";
                    p class="vote-ratio-readout-wrap" {
                        span class="vote-ratio-readout-label muted" { "ratio" }
                        " "
                        span id=(readout_id) class="vote-ratio-readout" aria-live="polite" { "1:1" }
                    }
                    label class="vote-compare-slider-label" {
                        span id=(slider_left_id) { (item_display_path(p.left.as_str())) }
                        input type="range" id=(slider_id) class="vote-preference-slider" min="0" max="100" value="50"
                            aria-valuemin="0" aria-valuemax="100" aria-valuetext="1:1";
                        span id=(slider_right_id) { (item_display_path(p.right.as_str())) }
                    }
                    label class="vote-explain-label" { "reason (required)" }
                    textarea name="explanation" id=(explain_id) rows="5" placeholder="why this split?" required {}
                    div id=(errors_id) {}
                    p { button type="submit" { "post vote" } }
                }
            } @else if p.show_vote_form {
                // Guest CTA is outside any form so click is a normal navigation to login.
                div id=(form_id) class="vote-compare-form vote-compare-guest" {
                    p {
                        a class="vote-compare-login-cta" href=(login_href_with_next(p.next_path)) { "post vote" }
                    }
                    p class="muted" { "you’ll log in, then return to this pair to cast your vote." }
                }
            } @else {
                p class="muted" { "you need post access in this room to vote on this pair." }
            }
        }
    }
}

/// Build the compare panel for a garden scope (and optional aspect), or empty markup if no pair.
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
    /// Aspect sub-question to vote under (`:slug` group instead of canonical).
    #[serde(default)]
    pub aspect: Option<String>,
}

/// Neediest scope for the `/vote` landing: lowest voted-pair density among
/// tilde scopes with ≥2 comparable members. Returns scope + voted + possible.
/// Fully-judged scopes are skipped; `None` means everything is judged.
pub(super) fn pick_neediest_scope(content: &ContentState) -> Option<(ItemId, usize, usize)> {
    let group = &content.ranking_group;
    // (density_num, density_den, scope): lowest density wins; ties prefer more
    // members, then lex-smallest scope (iteration order is lex-sorted, and
    // only strictly-better candidates replace the incumbent).
    let mut best: Option<(usize, usize, usize, ItemId)> = None;
    let mut scopes: Vec<&ItemId> = content
        .members_by_scope
        .keys()
        .filter(|id| matches!(id.tilde_tail(), Some(t) if !t.is_empty() && !t.contains('/')))
        .collect();
    scopes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for scope in scopes {
        let members = comparable_items(content, content.members_of(scope));
        if members.len() < 2 {
            continue;
        }
        let possible = members.len() * (members.len() - 1) / 2;
        let idx: Vec<usize> = members
            .iter()
            .filter_map(|m| group.item_to_idx.get(m).copied())
            .collect();
        let mut voted = 0usize;
        for (i, &a) in idx.iter().enumerate() {
            for &b in &idx[i + 1..] {
                let key = if a < b { (a, b) } else { (b, a) };
                if group.voted_pairs.contains(&key) {
                    voted += 1;
                }
            }
        }
        if voted >= possible {
            continue;
        }
        // Compare voted/possible without floats: a/b < c/d ⟺ a·d < c·b.
        // strictly-less keeps the lex-first scope on exact ties; larger member
        // count wins on equal density.
        let take = match &best {
            None => true,
            Some((bv, bp, bn, _)) => {
                (voted * bp).cmp(&(bv * possible)) == std::cmp::Ordering::Less
                    || (voted * bp == bv * possible && members.len() > *bn)
            }
        };
        if take {
            best = Some((voted, possible, members.len(), (*scope).clone()));
        }
    }
    best.map(|(voted, possible, _, scope)| (scope, voted, possible))
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

pub(super) async fn vote_compare_inner(
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
                return vote_landing(state, nav, headers, jar, uri).await;
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
                None => {
                    return (StatusCode::BAD_REQUEST, "no pairs available in pool").into_response()
                }
            }
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "provide both left and right, or just pool",
            )
                .into_response()
        }
    };

    let aspect_slug = q
        .aspect
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(slug) = &aspect_slug {
        if !crate::dsl::is_valid_aspect_slug(slug) {
            return (StatusCode::BAD_REQUEST, "bad aspect slug").into_response();
        }
    }

    render_compare_page(
        state,
        nav,
        headers,
        jar,
        uri,
        left,
        right,
        pool_id,
        aspect_slug,
        q.thread.clone(),
        None,
    )
    .await
}

/// Render one judged pair: intro header (landing only) + compare panel.
#[allow(clippy::too_many_arguments)]
async fn render_compare_page(
    state: AppState,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
    left: ItemId,
    right: ItemId,
    pool_id: Option<ItemId>,
    aspect_slug: Option<String>,
    query_thread: Option<String>,
    intro: Option<maud::Markup>,
) -> axum::response::Response {
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
    let auto_thread = query_thread
        .as_deref()
        .map(canonicalize_tag)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| pick_autothread_for_vote_pair(content, &left, &right));
    let mut thread_tags = vote_thread_tags_for_pair(content, &left, &right);
    if !auto_thread.is_empty() && !thread_tags.iter().any(|t| t == &auto_thread) {
        thread_tags.insert(0, auto_thread.clone());
    }
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

    let view_class =
        "view-ontology view-ontology-light view-vote-compare view-vote-compare-fullscreen";

    let panel = vote_compare_panel_markup(VoteComparePanel {
        nav: &nav,
        left: &left,
        right: &right,
        left_body: left_body.as_ref(),
        right_body: right_body.as_ref(),
        item_bodies: Some(&item_bodies_for_cards),
        pool: pool_id.as_ref(),
        auto_thread: &auto_thread,
        thread_tags: &thread_tags,
        edge_history,
        next_pair: next_pair.as_ref(),
        next_path: &next_path,
        aspect_slug: aspect_slug.as_deref(),
        logged_in,
        show_vote_form,
        include_heading: true,
        ids: &VoteCompareDomIds::page(),
    });
    let body = html! {
        @if let Some(intro) = intro {
            (intro)
        }
        (panel)
    };

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout_full_bleed_chromeless(
        &title,
        view_class,
        body,
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}

/// `GET /vote` with no pair: deal the neediest scope first — the judgment
/// entry point. First run is three steps; every later visit is one pair.
async fn vote_landing(
    state: AppState,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    let (scope, voted, possible) = {
        let reduced = state.reduced.read().await;
        let content = content_for_garden_view(&reduced, &nav.scope());
        match pick_neediest_scope(content) {
            Some(pick) => pick,
            None => {
                drop(reduced);
                return landing_empty_page(&state, &jar, &uri).await;
            }
        }
    };
    let pair = {
        let reduced = state.reduced.read().await;
        let content = content_for_garden_view(&reduced, &nav.scope());
        let members = comparable_items(content, content.members_of(&scope));
        suggest_next_pair_in_pool(&content.ranking_group, &members, None)
    };
    let Some((left, right)) = pair else {
        return landing_empty_page(&state, &jar, &uri).await;
    };
    let intro = html! {
        header class="vote-landing" {
            h1 class="vote-landing-title" { "judge one pair" }
            p class="vote-landing-need" {
                (item_display_path(scope.as_str()))
                " has "
                (format!("{voted} of {possible}"))
                " comparisons so far — your vote counts here more than anywhere else."
            }
            ol class="vote-landing-steps" {
                li { "compare the two items below" }
                li { "drag the slider, then write why (required)" }
                li {
                    "post — your vote ranks them in "
                    a href=(item_href(scope.as_str(), &nav)) {
                        (item_display_path(scope.as_str()))
                    }
                }
            }
        }
    };
    render_compare_page(
        state,
        nav,
        headers,
        jar,
        uri,
        left,
        right,
        Some(scope),
        None,
        None,
        Some(intro),
    )
    .await
}

/// Nothing left to judge: every scope with two comparable members is fully compared.
async fn landing_empty_page(
    state: &AppState,
    jar: &CookieJar,
    uri: &Uri,
) -> axum::response::Response {
    let url_key = canonical_view_url(uri);
    let view_count = state.views.get_views(&url_key);
    let page = layout_full_bleed_chromeless(
        "vote",
        "view-ontology view-ontology-light view-vote-compare view-vote-compare-fullscreen",
        html! {
            header class="vote-landing" {
                h1 class="vote-landing-title" { "judge one pair" }
                p class="vote-landing-need" {
                    "everything with two comparable members has been fully compared. "
                    "Browse the "
                    a href="/~" { "garden" }
                    ", or start a "
                    a href="/" { "thread" }
                    " to open a new question."
                }
            }
        },
        Some(view_count),
        theme_from_jar(jar),
        &theme_next_from_uri(uri),
    );
    Html(page.into_string()).into_response()
}
