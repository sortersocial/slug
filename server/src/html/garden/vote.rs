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
        format_ratio, forum::ThreadNav, layout_full_bleed_chromeless, now_ms, ratio_pct,
        render_item_body_in_scope, theme_from_jar, theme_next_from_uri, ui_action::UI_RPC_FIELD,
        user_can_post_room, JsBuilder,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    reducer::{ContentState, ForumThreadState, ScopeId, VoteSkipEntry},
    scope_rank::{comparable_items, suggest_next_pair_in_pool_excluding},
    state::AppState,
    timeago,
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

    pub(super) fn skip_form_id(&self) -> String {
        self.suffixed("vote-compare-skip-form")
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
    principal: &str,
    left: &ItemId,
    right: &ItemId,
    pool: Option<&ItemId>,
    aspect: Option<&str>,
    next_path: &str,
    dom_suffix: Option<&str>,
) -> String {
    let ids = VoteCompareDomIds::with_suffix(dom_suffix.unwrap_or("").to_string());
    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    let edge_history = vote_edge_history_markup(content, left, right);
    let excluded = reduced.skipped_pairs(principal, &nav.scope(), aspect);
    let group = vote_ranking_group(content, pool, aspect);
    let next_pair = suggest_next_vote_pair(content, left, right, pool, group, Some(&excluded));
    let nav_markup = vote_compare_nav_markup(&VoteCompareNavView {
        nav,
        left,
        right,
        next_pair: next_pair.as_ref(),
        pool,
        aspect,
        nav_id: &ids.nav_id(),
        skip_form_id: &ids.skip_form_id(),
        logged_in: true,
        next_path,
    });
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
    aspect: Option<&str>,
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
    if let Some(a) = aspect.filter(|s| !s.is_empty()) {
        base = format!("{}&aspect={}", base, urlencoding::encode(a));
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

pub(super) fn vote_skipped_href(nav: &ThreadNav) -> String {
    format!("{}/vote/skipped", nav.room_path_prefix_for_vote_compare())
}

fn vote_ranking_group<'a>(
    content: &'a ContentState,
    pool: Option<&ItemId>,
    aspect: Option<&str>,
) -> &'a crate::reducer::GroupState {
    match (pool, aspect) {
        (Some(scope), Some(slug)) => content
            .aspect_group(scope, slug)
            .unwrap_or(&content.ranking_group),
        _ => &content.ranking_group,
    }
}

struct VoteCompareNavView<'a> {
    nav: &'a ThreadNav,
    left: &'a ItemId,
    right: &'a ItemId,
    next_pair: Option<&'a (ItemId, ItemId)>,
    pool: Option<&'a ItemId>,
    aspect: Option<&'a str>,
    nav_id: &'a str,
    skip_form_id: &'a str,
    logged_in: bool,
    next_path: &'a str,
}

fn vote_compare_nav_markup(p: &VoteCompareNavView<'_>) -> maud::Markup {
    let next_pair_href = p
        .next_pair
        .map(|(nl, nr)| vote_compare_href(p.nav, nl, nr, None, p.pool, p.aspect));
    let skipped_href = vote_skipped_href(p.nav);
    let mut skip_rpc = json!({
        "action": "vote_compare_skip",
        "room": p.nav.room_wire,
        "left_item": p.left.as_str(),
        "right_item": p.right.as_str(),
        "pool": p.pool.map(|q| q.as_str()),
        "form_action": "/ui",
    });
    if p.aspect.is_some() {
        skip_rpc["aspect"] = json!(p.aspect);
    }
    let skip_json = template_json_compact(&skip_rpc).expect("vote skip rpc json");
    html! {
        div id=(p.nav_id) class="vote-compare-nav" {
            @if p.logged_in {
                form id=(p.skip_form_id) class="vote-compare-skip-form" method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(skip_json);
                    button type="submit" class="vote-compare-skip" data-testid="vote-skip" { "skip" }
                }
            } @else {
                a class="vote-compare-skip" data-testid="vote-skip" href=(login_href_with_next(p.next_path)) { "skip" }
            }
            a class="vote-compare-skipped-link" data-testid="vote-skipped" href=(
                if p.logged_in {
                    skipped_href.clone()
                } else {
                    login_href_with_next(&skipped_href)
                }
            ) { "skipped" }
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
    group: &crate::reducer::GroupState,
    excluded: Option<&HashSet<(ItemId, ItemId)>>,
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
    suggest_next_pair_in_pool_excluding(group, &pool, Some((current_left, current_right)), excluded)
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
    let skip_form_id = p.ids.skip_form_id();
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
            (vote_compare_nav_markup(&VoteCompareNavView {
                nav: p.nav,
                left: p.left,
                right: p.right,
                next_pair: p.next_pair,
                pool: p.pool,
                aspect: p.aspect_slug,
                nav_id: &nav_id,
                skip_form_id: &skip_form_id,
                logged_in: p.logged_in,
                next_path: p.next_path,
            }))
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
                        // 0–100; slug_ui.js snaps to human ratios 100:1 … 1:1 … 1:100.
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

/// Showcase question for the `/vote` landing: the most-compared open
/// question across canonical scopes and voted aspect groups. Popularity is
/// judged by distinct voted pairs, so winners keep winning: dealing a
/// popular scope's stragglers grows it further. Fully-judged questions are
/// skipped (nothing left to deal); `None` means everything is judged.
#[derive(Debug, Clone)]
pub(super) struct LandingQuestion {
    pub scope: ItemId,
    /// `None` = canonical ranking, `Some(slug)` = that aspect group.
    pub aspect: Option<String>,
    pub voted: usize,
    pub possible: usize,
    pub members: usize,
    pub last_vote_ts: i64,
}

/// Density of voted pairs among `members` inside one vote graph.
fn voted_density(
    item_to_idx: &std::collections::HashMap<ItemId, usize>,
    voted_pairs: &std::collections::HashSet<(usize, usize)>,
    members: &[ItemId],
) -> (usize, usize) {
    let possible = members.len() * (members.len() - 1) / 2;
    let idx: Vec<usize> = members
        .iter()
        .filter_map(|m| item_to_idx.get(m).copied())
        .collect();
    let mut voted = 0usize;
    for (i, &a) in idx.iter().enumerate() {
        for &b in &idx[i + 1..] {
            let key = if a < b { (a, b) } else { (b, a) };
            if voted_pairs.contains(&key) {
                voted += 1;
            }
        }
    }
    (voted, possible)
}

/// Every open question (canonical + aspect groups with ≥1 unvoted pair),
/// unsorted. Powers both the landing deal and the heat index below it.
fn open_questions(content: &ContentState) -> Vec<LandingQuestion> {
    let mut out = Vec::new();
    let mut scopes: Vec<&ItemId> = content
        .members_by_scope
        .keys()
        .filter(|id| matches!(id.tilde_tail(), Some(t) if !t.is_empty() && !t.contains('/')))
        .collect();
    scopes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let mut aspect_keys: Vec<&(ItemId, String)> = content.aspect_groups.keys().collect();
    aspect_keys.sort();
    for scope in &scopes {
        let members = comparable_items(content, content.members_of(scope));
        if members.len() < 2 {
            continue;
        }
        let (voted, possible) = voted_density(
            &content.ranking_group.item_to_idx,
            &content.ranking_group.voted_pairs,
            &members,
        );
        if voted < possible {
            out.push(LandingQuestion {
                scope: (*scope).clone(),
                aspect: None,
                voted,
                possible,
                members: members.len(),
                last_vote_ts: last_canonical_vote_ts(content, &members),
            });
        }
        for (ascope, slug) in aspect_keys
            .iter()
            .filter(|(s, _)| s.as_str() == scope.as_str())
        {
            let Some(group) = content.aspect_groups.get(&(ascope.clone(), slug.clone())) else {
                continue;
            };
            let (voted, possible) = voted_density(&group.item_to_idx, &group.voted_pairs, &members);
            if voted < possible {
                out.push(LandingQuestion {
                    scope: (*scope).clone(),
                    aspect: Some(slug.clone()),
                    voted,
                    possible,
                    members: members.len(),
                    last_vote_ts: group.recent_votes.front().map(|v| v.ts).unwrap_or(0),
                });
            }
        }
    }
    out
}

/// Newest canonical vote touching two members (0 when none). The global
/// recent-votes deque is newest-first, so the first in-electorate hit wins.
fn last_canonical_vote_ts(content: &ContentState, members: &[ItemId]) -> i64 {
    let set: std::collections::HashSet<&ItemId> = members.iter().collect();
    content
        .ranking_group
        .recent_votes
        .iter()
        .find(|v| set.contains(&v.a) && set.contains(&v.b))
        .map(|v| v.ts)
        .unwrap_or(0)
}

/// One row of the heat index: an open question plus its thread heartbeat.
pub(super) struct OpenRow {
    pub question: LandingQuestion,
    pub thread_ts: i64,
}

const OPEN_INDEX_CAP: usize = 10;

/// Open questions hottest first: newest vote wins, then newest thread post.
/// Votes always outrank mere discussion (a voted question has
/// `last_vote_ts > 0`; a talked-about one has 0), so judging heat beats
/// talking heat by construction.
pub(super) fn rank_open_questions(
    content: &ContentState,
    threads: &HashMap<(ScopeId, String), ForumThreadState>,
    scope: &ScopeId,
) -> Vec<OpenRow> {
    let mut rows: Vec<OpenRow> = open_questions(content)
        .into_iter()
        .map(|question| {
            let leaf = canonicalize_tag(question.scope.last_segment());
            let thread_ts = threads
                .get(&(scope.clone(), leaf))
                .map(|t| t.last_activity_ts)
                .unwrap_or(0);
            OpenRow {
                question,
                thread_ts,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        (b.question.last_vote_ts, b.thread_ts).cmp(&(a.question.last_vote_ts, a.thread_ts))
    });
    rows
}

pub(super) fn pick_landing_question(content: &ContentState) -> Option<LandingQuestion> {
    // Most voted pairs wins; ties prefer more members, then canonical over an
    // aspect, then lex order (`open_questions` is lex-sorted with canonical
    // first per scope, and only strictly-better replaces the incumbent).
    let mut best: Option<LandingQuestion> = None;
    for q in open_questions(content) {
        let take = match &best {
            None => true,
            Some(incumbent) => {
                q.voted.cmp(&incumbent.voted) == std::cmp::Ordering::Greater
                    || (q.voted == incumbent.voted && q.members > incumbent.members)
            }
        };
        if take {
            best = Some(q);
        }
    }
    best
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
            let viewer = optional_principal(&headers, &jar, &reduced);
            let excluded = viewer
                .as_deref()
                .map(|p| reduced.skipped_pairs(p, &nav.scope(), aspect_slug.as_deref()))
                .unwrap_or_default();
            let group = vote_ranking_group(content, Some(pool), aspect_slug.as_deref());
            let pair = suggest_next_pair_in_pool_excluding(group, &children, None, Some(&excluded));
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
    below: Option<maud::Markup>,
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
    let excluded = viewer
        .as_deref()
        .map(|p| reduced.skipped_pairs(p, &nav.scope(), aspect_slug.as_deref()))
        .unwrap_or_default();
    let group = vote_ranking_group(content, pool_id.as_ref(), aspect_slug.as_deref());
    let next_pair = suggest_next_vote_pair(
        content,
        &left,
        &right,
        pool_id.as_ref(),
        group,
        Some(&excluded),
    );
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
        @if let Some(below) = below {
            (below)
        }
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

/// `GET /vote` with no pair: deal the neediest open question first — a scope
/// or one of its aspect groups. First run is three steps; every later visit
/// is one pair.
async fn vote_landing(
    state: AppState,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    let scope_id = nav.scope();
    let (dealt, pair, index, skip_count) = {
        let reduced = state.reduced.read().await;
        let content = content_for_garden_view(&reduced, &nav.scope());
        let viewer = optional_principal(&headers, &jar, &reduced);
        let skip_count = viewer
            .as_deref()
            .map(|p| reduced.skipped_entries(p, &scope_id).len())
            .unwrap_or(0);
        let mut ranked = rank_open_questions(content, &reduced.forum_threads, &scope_id);
        let mut dealt: Option<LandingQuestion> = None;
        let mut pair: Option<(ItemId, ItemId)> = None;
        // Prefer the usual popularity deal, but walk hotter→cooler if this
        // viewer has skipped every remaining pair in the winner.
        let mut order: Vec<LandingQuestion> = Vec::new();
        if let Some(first) = pick_landing_question(content) {
            order.push(first);
        }
        for row in &ranked {
            if !order
                .iter()
                .any(|q| q.scope == row.question.scope && q.aspect == row.question.aspect)
            {
                order.push(LandingQuestion {
                    scope: row.question.scope.clone(),
                    aspect: row.question.aspect.clone(),
                    voted: row.question.voted,
                    possible: row.question.possible,
                    members: row.question.members,
                    last_vote_ts: row.question.last_vote_ts,
                });
            }
        }
        for q in order {
            let members = comparable_items(content, content.members_of(&q.scope));
            let group = match &q.aspect {
                None => &content.ranking_group,
                Some(slug) => match content.aspect_group(&q.scope, slug) {
                    Some(group) => group,
                    None => continue,
                },
            };
            let excluded = viewer
                .as_deref()
                .map(|p| reduced.skipped_pairs(p, &scope_id, q.aspect.as_deref()))
                .unwrap_or_default();
            if let Some(p) =
                suggest_next_pair_in_pool_excluding(group, &members, None, Some(&excluded))
            {
                dealt = Some(q);
                pair = Some(p);
                break;
            }
        }
        if let Some(d) = &dealt {
            ranked
                .retain(|row| !(row.question.scope == d.scope && row.question.aspect == d.aspect));
        }
        ranked.truncate(OPEN_INDEX_CAP);
        (dealt, pair, ranked, skip_count)
    };
    let Some(dealt) = dealt else {
        return landing_empty_page(&state, &jar, &uri, skip_count, &nav).await;
    };
    let Some((left, right)) = pair else {
        return landing_empty_page(&state, &jar, &uri, skip_count, &nav).await;
    };
    let (scope, aspect, voted, possible) = (dealt.scope, dealt.aspect, dealt.voted, dealt.possible);
    let scope_href = match &aspect {
        None => item_href(scope.as_str(), &nav),
        Some(slug) => format!("{}#aspect-{slug}", item_href(scope.as_str(), &nav)),
    };
    let skipped_href = vote_skipped_href(&nav);
    let intro = html! {
        header class="vote-landing" {
            h1 class="vote-landing-title" { "judge one pair" }
            p class="vote-landing-need" {
                @if let Some(slug) = &aspect {
                    ":" (slug) " in "
                }
                (item_display_path(scope.as_str()))
                " — "
                (format!("{voted} of {possible}"))
                " pairs judged, the garden's most compared open question."
                @if skip_count > 0 {
                    " "
                    a href=(skipped_href) { (skip_count) " skipped" }
                }
            }
            ol class="vote-landing-steps" {
                li { "compare the two items below" }
                li { "drag the slider, then write why (required)" }
                li {
                    "post — your vote ranks them in "
                    a href=(scope_href) {
                        @if let Some(slug) = &aspect {
                            ":" (slug) " in "
                        }
                        (item_display_path(scope.as_str()))
                    }
                }
            }
        }
    };
    let below = if index.is_empty() {
        None
    } else {
        Some(open_index_markup(&nav, &index, now_ms()))
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
        aspect,
        None,
        Some(intro),
        below,
    )
    .await
}

/// Heat index below the dealt pair: more open questions, hottest first. Each
/// row deals straight into its pool — a menu, not a ranking, so it carries
/// judged counts and activity instead of scores.
fn open_index_markup(nav: &ThreadNav, rows: &[OpenRow], now: i64) -> maud::Markup {
    html! {
        section class="vote-open-index ont-tab-panel" {
            h3 { "more open questions" }
            ul class="vote-open-list" {
                @for row in rows {
                    @let q = &row.question;
                    @let judge_href = match &q.aspect {
                        None => vote_pool_href(nav, q.scope.as_str()),
                        Some(slug) => format!(
                            "{}&aspect={}",
                            vote_pool_href(nav, q.scope.as_str()),
                            urlencoding::encode(slug)
                        ),
                    };
                    @let garden_href = match &q.aspect {
                        None => item_href(q.scope.as_str(), nav),
                        Some(slug) => {
                            format!("{}#aspect-{slug}", item_href(q.scope.as_str(), nav))
                        }
                    };
                    @let leaf = canonicalize_tag(q.scope.last_segment());
                    @let active_ts = row.question.last_vote_ts.max(row.thread_ts);
                    li class="vote-open-row" {
                        a class="vote-open-judge" href=(judge_href) { "judge" }
                        span class="vote-open-name" {
                            @if let Some(slug) = &q.aspect {
                                span class="vote-open-aspect" { ":" (slug) " in " }
                            }
                            a href=(garden_href) { (item_display_path(q.scope.as_str())) }
                        }
                        span class="muted vote-open-meta" {
                            (format!("{} of {} judged", q.voted, q.possible))
                            @if active_ts > 0 {
                                @let hover = timeago::rfc3339_utc(active_ts);
                                @let ago = timeago::timeago(now, active_ts);
                                " · "
                                span title=(hover) { (ago) }
                            }
                        }
                        span class="vote-open-links" {
                            a href=(nav.thread_url(&leaf)) { "thread" }
                        }
                    }
                }
            }
        }
    }
}

/// Nothing left to judge: every scope with two comparable members is fully compared
/// (or this viewer has skipped the rest).
async fn landing_empty_page(
    state: &AppState,
    jar: &CookieJar,
    uri: &Uri,
    skip_count: usize,
    nav: &ThreadNav,
) -> axum::response::Response {
    let url_key = canonical_view_url(uri);
    let view_count = state.views.get_views(&url_key);
    let skipped_href = vote_skipped_href(nav);
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
                    @if skip_count > 0 {
                        " "
                        a href=(skipped_href) { (skip_count) " skipped" }
                    }
                }
            }
        },
        Some(view_count),
        theme_from_jar(jar),
        &theme_next_from_uri(uri),
    );
    Html(page.into_string()).into_response()
}

/// After skip: URL of the next unskipped pair (same pool/aspect), or `/vote` landing.
pub(crate) fn vote_skip_redirect_href(
    nav: &ThreadNav,
    content: &ContentState,
    left: &ItemId,
    right: &ItemId,
    pool: Option<&ItemId>,
    aspect: Option<&str>,
    excluded: &HashSet<(ItemId, ItemId)>,
) -> String {
    let group = vote_ranking_group(content, pool, aspect);
    match suggest_next_vote_pair(content, left, right, pool, group, Some(excluded)) {
        Some((nl, nr)) => vote_compare_href(nav, &nl, &nr, None, pool, aspect),
        None => format!("{}/vote", nav.room_path_prefix_for_vote_compare()),
    }
}

pub(crate) async fn vote_compare_skip_redirect(
    state: &AppState,
    nav: &ThreadNav,
    principal: &str,
    left: &ItemId,
    right: &ItemId,
    pool: Option<&ItemId>,
    aspect: Option<&str>,
) -> String {
    let reduced = state.reduced.read().await;
    let content = content_for_garden_view(&reduced, &nav.scope());
    let excluded = reduced.skipped_pairs(principal, &nav.scope(), aspect);
    vote_skip_redirect_href(nav, content, left, right, pool, aspect, &excluded)
}

fn vote_unskip_rpc_json(nav: &ThreadNav, e: &VoteSkipEntry) -> String {
    let mut val = json!({
        "action": "vote_compare_unskip",
        "room": nav.room_wire,
        "left_item": e.lo.as_str(),
        "right_item": e.hi.as_str(),
        "form_action": "/ui",
    });
    if let Some(a) = &e.aspect {
        val["aspect"] = json!(a);
    }
    template_json_compact(&val).expect("unskip rpc json")
}

fn vote_skipped_list_markup(nav: &ThreadNav, entries: &[VoteSkipEntry], now: i64) -> maud::Markup {
    html! {
        div id="vote-skipped-region" {
            @if entries.is_empty() {
                p class="muted vote-skipped-empty" { "no skipped pairs" }
            } @else {
                ul class="vote-skipped-list" {
                    @for e in entries {
                        @let pair_href = vote_compare_href(
                            nav,
                            &e.lo,
                            &e.hi,
                            None,
                            e.pool.as_ref(),
                            e.aspect.as_deref(),
                        );
                        @let unskip_json = vote_unskip_rpc_json(nav, e);
                        li class="vote-skipped-row" data-testid="vote-skipped-row" {
                            a class="vote-skipped-pair" href=(pair_href) {
                                code { (item_display_path(e.lo.as_str())) }
                                " vs "
                                code { (item_display_path(e.hi.as_str())) }
                            }
                            span class="muted vote-skipped-meta" {
                                @if let Some(slug) = &e.aspect {
                                    ":" (slug)
                                    @if e.pool.is_some() { " in " }
                                }
                                @if let Some(pool) = &e.pool {
                                    (item_display_path(pool.as_str()))
                                }
                                @if e.ts > 0 {
                                    @let hover = timeago::rfc3339_utc(e.ts);
                                    @let ago = timeago::timeago(now, e.ts);
                                    " · "
                                    span title=(hover) { (ago) }
                                }
                            }
                            form class="vote-unskip-form" method="POST" action="/ui" {
                                input type="hidden" name=(UI_RPC_FIELD) value=(unskip_json);
                                button type="submit" class="vote-unskip" data-testid="vote-unskip" { "unskip" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Markup for the skipped list after an unskip (morph target `#vote-skipped-region`).
pub(crate) fn vote_skipped_region_markup(
    nav: &ThreadNav,
    entries: &[VoteSkipEntry],
    now: i64,
) -> maud::Markup {
    vote_skipped_list_markup(nav, entries, now)
}

pub async fn vote_skipped_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let nav = ThreadNav::public();
    vote_skipped_inner(state, nav, headers, jar, uri).await
}

pub async fn room_vote_skipped_page(
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
    vote_skipped_inner(state, nav, headers, jar, uri).await
}

async fn vote_skipped_inner(
    state: AppState,
    nav: ThreadNav,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    let reduced = state.reduced.read().await;
    let viewer = optional_principal(&headers, &jar, &reduced);
    let next_path = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| vote_skipped_href(&nav));
    let entries = viewer
        .as_deref()
        .map(|p| reduced.skipped_entries(p, &nav.scope()))
        .unwrap_or_default();
    drop(reduced);
    let now = now_ms();
    let vote_home = format!("{}/vote", nav.room_path_prefix_for_vote_compare());
    let body = html! {
        header class="vote-landing" {
            h1 class="vote-landing-title" { "skipped pairs" }
            p class="vote-landing-need" {
                a href=(vote_home) { "back to vote" }
            }
        }
        @if viewer.is_none() {
            p {
                a class="vote-compare-login-cta" href=(login_href_with_next(&next_path)) { "log in" }
                " to see pairs you've skipped."
            }
        } @else {
            (vote_skipped_list_markup(&nav, &entries, now))
        }
    };
    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);
    let page = layout_full_bleed_chromeless(
        "skipped pairs — vote",
        "view-ontology view-ontology-light view-vote-compare view-vote-compare-fullscreen view-vote-skipped",
        body,
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
    );
    Html(page.into_string()).into_response()
}
