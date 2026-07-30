use axum::{
    http::Uri,
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::{
    html::{
        cli_panel,
        forum::ThreadNav,
        layout,
        now_ms,
        format_ratio, ratio_pct,
        recency_color_style,
        render_item_body_in_scope,
        theme_from_jar,
        theme_next_from_uri,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    reducer::ScopeId,
    state::AppState,
    timeago,
};

use super::{
    access::content_for_garden_view,
    browse::{garden_layout_meta, scoped_bc_path_for, GardenBrowsePath},
    copy::garden_rank_copy_button_markup,
    external::{external_frame_allowed, external_source_href, github_resolver_controls},
    item::{child_depth_from_uri, item_code_label, item_display_path, item_href},
    item_page::{build_item_page_view_model, sibling_nav_markup},
    pin::{child_row_pin_or_vote, ont_pin_vote_controls, pinned_item_from_jar},
    vote::vote_pool_href,
};

pub(super) async fn render_scope_view(
    state: AppState,
    browse: GardenBrowsePath,
    nav: ThreadNav,
    jar: CookieJar,
    uri: Uri,
) -> axum::response::Response {
    let scope = nav.scope();
    let pin_ref = pinned_item_from_jar(&jar);
    let reduced = state.reduced.read().await;
    let child_depth = child_depth_from_uri(&uri);
    let model = build_item_page_view_model(&reduced, &scope, browse.item(), child_depth);
    let scope_content = content_for_garden_view(&reduced, &scope);
    let thread_href = |tag: &str| nav.thread_url(tag);
    let external_empty_body = browse.is_external() && model.body.is_none();
    let cli_path_arg = item_display_path(&model.item);
    let external_href = external_source_href(&model.item);
    let external_can_embed = external_frame_allowed(&model.item);
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
                    @let vote_item_href = model.sibling_nav.as_ref().and_then(|_| {
                        ItemId::parse(&model.item)
                            .and_then(|i| i.parent())
                            .map(|p| vote_pool_href(&nav, p.as_str()))
                    });
                    (ont_pin_vote_controls(
                        &nav,
                        &model.item,
                        pin_ref.as_ref(),
                        &next_for_pin,
                        vote_item_href.as_deref(),
                    ))
                }
                @if let Some(body) = &model.body {
                    div class="ont-item-content" {
                        (render_item_body_in_scope(
                            body,
                            nav.garden_root_url(),
                            Some(&scope_content.item_bodies),
                        ))
                    }
                } @else if external_empty_body {
                    div class="ont-item-content ont-external-empty" {
                        p { "This is an external scope." }
                        p class="muted" {
                            "This scope has no imported body yet. Open the source page directly."
                        }
                        p {
                            a href=(external_href.as_str()) target="_blank" rel="noopener noreferrer" {
                                "Open external URL"
                            }
                        }
                        @if external_can_embed {
                            p class="muted" { "Best-effort embedded preview:" }
                            iframe
                                class="ont-external-frame"
                                src=(external_href.as_str())
                                sandbox=""
                                loading="lazy"
                                referrerpolicy="no-referrer"
                                style="width:100%;height:360px;border:1px solid currentColor;" {}
                        } @else {
                            p class="muted" { "Embedded preview is unavailable for this host." }
                        }
                    }
                } @else {
                    div class="ont-item-content" { p class="muted" { "no body yet" } }
                }
            }

            @if let Some(markup) = github_resolver_controls(&model.item, &nav, &next_for_pin) {
                (markup)
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
                        @let ts_style = recency_color_style(now, e.ts);
                        div class="rank-history-entry" {
                            div class="rank-history-meta" title=(hover) {
                                span class="rank-history-pos" {
                                    (format!("#{} of {}{}", e.scope_rank, e.scope_total, delta_str))
                                }
                                " · "
                                span class="ts-recency" style=(ts_style.as_str()) { (ago) }
                                span class="muted" { (label) }
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
                                            span class="vote-ratio" { (format_ratio(v.ratio_left, v.ratio_right)) }
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
                @let total_children = model.child_rankings.component_rankings
                    .iter().map(|c| c.ranked.len()).sum::<usize>()
                    + model.child_rankings.unranked_items.len();
                h3 {
                    "ranked child groups"
                    @if model.child_depth > 1 {
                        " "
                        span class="muted" { (format!("(depth {})", model.child_depth)) }
                    }
                    @if total_children > 0 {
                        " "
                        (garden_rank_copy_button_markup(
                            &nav,
                            &item_display_path(&model.item),
                            model.child_depth,
                            false,
                        ))
                    }
                    @if total_children >= 2 {
                        " "
                        a class="ont-vote-children-btn" href=(vote_pool_href(&nav, &model.item)) {
                            "vote on children"
                        }
                    }
                }
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
                                        (child_row_pin_or_vote(&nav, &r.item, pin_ref.as_ref(), scope_content, &next_for_pin))
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
                                    (child_row_pin_or_vote(&nav, name, pin_ref.as_ref(), scope_content, &next_for_pin))
                                    @let href = item_href(name.as_str(), &nav);
                                    a class="item-link" href=(href) { code { (item_display_path(name.as_str())) } }
                                }
                            }
                        }
                    }
                }
            }
            @let cli = match &scope {
                ScopeId::Public => format!("npx slugsocial public garden body {cli_path_arg}"),
                ScopeId::Room(room_id) => format!("npx slugsocial private {room_id} garden body {cli_path_arg}"),
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

