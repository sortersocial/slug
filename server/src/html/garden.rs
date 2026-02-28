use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse},
};
use maud::html;

use crate::{
    events::canonicalize_item,
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    state::AppState,
    timeago,
};

use super::{
    actor_label, bc_path, layout, now_ms, ratio_pct,
    breadcrumb_path::OntologyPath,
};

/// Display path for an item. No namespace stripping: paths are first-class.
fn item_display_path(item: &str) -> String {
    canonicalize_item(item)
}

/// Canonical ontology URL for an item path.
fn item_href(item: &str) -> String {
    format!("/~/{}", item_display_path(item))
}

/// Ontology index — root-level paths.
pub async fn garden_index(State(state): State<AppState>) -> impl IntoResponse {
    let roots: Vec<(String, usize)> = {
        let reduced = state.reduced.read().await;
        let mut v: Vec<(String, usize)> = reduced
            .item_children
            .get("")
            .map(|s| {
                s.iter()
                    .map(|path| {
                        let children = reduced.item_children.get(path).map(|c| c.len()).unwrap_or(0);
                        (path.clone(), children)
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };

    let page = layout(
        "~/",
        "view-ontology",
        html! {
            @let root_path = OntologyPath::root();
            nav class="breadcrumb" { (bc_path(&root_path)) }
            h2 { "paths" }
            @if roots.is_empty() {
                p class="muted" { "no items yet" }
            } @else {
                ul {
                    @for (path, children) in &roots {
                        li {
                            a href=(format!("/~/{}", path)) { "~/" (path) }
                            " "
                            span class="muted" { (format!("{}c", children)) }
                        }
                    }
                }
            }
        },
    );
    Html(page.into_string())
}

/// Single public handler for all `/~/*path` routes.
pub async fn ontology_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    let path = OntologyPath::from_input(&path);
    render_scope_view(state, path).await
}

async fn render_scope_view(state: AppState, path: OntologyPath) -> axum::response::Response {
    let parent_scope = path.as_str().to_string();
    let (group_opt, items_in_scope): (Option<crate::reducer::GroupState>, Vec<String>) = {
        let reduced = state.reduced.read().await;
        let group = Some(reduced.ranking_group.clone());
        let items_in_scope = reduced
            .item_children
            .get(&parent_scope)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_else(Vec::new);
        (group, items_in_scope)
    };

    let group = match group_opt {
        Some(g) => g,
        None => {
            // No votes yet at all — show the path with empty state.
            let page = layout(
                &format!("~/{}", path.as_str()),
                "view-ontology",
                html! {
                    nav class="breadcrumb" { (bc_path(&path)) }
                    h2 { "~/" (path.as_str()) }
                    @if items_in_scope.is_empty() {
                        p class="muted" { "no items yet" }
                    } @else {
                        ul {
                            @for it in &items_in_scope {
                                li { a href=(item_href(it)) { code { "/" (item_display_path(it)) } } }
                            }
                        }
                    }
                },
            );
            return Html(page.into_string()).into_response();
        }
    };

    // Build scoped components for direct children under parent_scope only.
    let scoped_idxs: Vec<usize> = items_in_scope
        .iter()
        .filter_map(|it| group.item_to_idx.get(it).copied())
        .collect();
    let local_to_global: Vec<usize> = scoped_idxs.clone();
    let global_to_local: std::collections::HashMap<usize, usize> = scoped_idxs
        .iter()
        .enumerate()
        .map(|(local, global)| (*global, local))
        .collect();
    let (mut comps_local, isolate_local_idxs) = connected_components_from_voted_pairs(
        scoped_idxs.len(),
        group.voted_pairs.iter().filter_map(|(i, j)| {
            let li = global_to_local.get(i).copied()?;
            let lj = global_to_local.get(j).copied()?;
            Some((li, lj))
        }),
    );

    // Items in the scope that have no votes yet.
    let mut no_vote_items: Vec<String> = items_in_scope
        .into_iter()
        .filter(|it| !group.item_to_idx.contains_key(it))
        .collect();
    no_vote_items.sort();

    // Sort components by size descending, then by item name for stability.
    comps_local.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    // Precompute rankings for each component so we can render a TOC and a "meat" section consistently.
    let component_rankings: Vec<(usize, usize, Vec<crate::ranking::RankedItem>)> = comps_local
        .iter()
        .enumerate()
        .map(|(ci, comp_local)| {
            let comp_global: Vec<usize> = comp_local
                .iter()
                .filter_map(|li| local_to_global.get(*li).copied())
                .collect();
            let comp_set: std::collections::HashSet<usize> = comp_global.iter().copied().collect();
            let ranked = ranked_items_subset(&group, &comp_global, 10000, 1e-8);
            let pairs = group
                .voted_pairs
                .iter()
                .filter(|(i, j)| comp_set.contains(i) && comp_set.contains(j))
                .count();
            (ci, pairs, ranked)
        })
        .collect();
    let isolate_idxs: Vec<usize> = isolate_local_idxs
        .into_iter()
        .filter_map(|li| local_to_global.get(li).copied())
        .collect();

    let bodies: std::collections::HashMap<String, String> = {
        let reduced = state.reduced.read().await;
        let mut out = std::collections::HashMap::new();
        for it in group.idx_to_item.iter().chain(no_vote_items.iter()) {
            if let Some(body) = reduced.item_bodies.get(it) {
                out.insert(it.clone(), body.clone());
            }
        }
        out
    };

    let page = layout(
        &format!("~/{}", path.as_str()),
        "view-ontology",
        html! {
            nav class="breadcrumb" { (bc_path(&path)) }

            h2 { "titles" }
            @if component_rankings.is_empty() {
                p class="muted" { "no voted pairs yet" }
            } @else {
                @for (ci, pairs, ranked) in component_rankings.iter() {
                    div class="component" {
                        div class="component-header" {
                            (format!("ordering {} items={} pairs={}", ci + 1, ranked.len(), pairs))
                        }
                        ol class="ranking" {
                            @for r in ranked.iter() {
                                @let item_url = item_href(&r.item);
                                li {
                                    a class="item-link" href=(item_url) { code { "/" (item_display_path(&r.item)) } }
                                }
                            }
                        }
                    }
                }
            }

            @if !isolate_idxs.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "isolates" }
                    ul {
                        @for idx in &isolate_idxs {
                            @let name = group.idx_to_item.get(*idx).cloned().unwrap_or_default();
                            li {
                                @let href = item_href(&name);
                                a class="item-link" href=(href) { code { "/" (item_display_path(&name)) } }
                            }
                        }
                    }
                }
            }

            @if !no_vote_items.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "not yet compared" }
                    ul {
                        @for it in &no_vote_items {
                            li {
                                @let href = item_href(it);
                                a class="item-link" href=(href) { code { "/" (item_display_path(it)) } }
                            }
                        }
                    }
                }
            }

            h2 { "titles + bodies" }
            @if component_rankings.is_empty() && no_vote_items.is_empty() && isolate_idxs.is_empty() {
                p class="muted" { "none yet" }
            } @else {
                @for (ci, pairs, ranked) in component_rankings.iter() {
                    div class="component" {
                        div class="component-header" {
                            (format!("ordering {} items={} pairs={}", ci + 1, ranked.len(), pairs))
                        }
                        ol class="ranking meat" {
                            @for r in ranked.iter() {
                                @let item_url = item_href(&r.item);
                                li {
                                    div class="item-card" {
                                        div class="item-card-header" {
                                            a class="item-link" href=(item_url) { code { "/" (item_display_path(&r.item)) } }
                                            span class="score" { (format!("{:.4}", r.score)) }
                                        }
                                        @if let Some(body) = bodies.get(&r.item) {
                                            div class="item-card-body" { (body) }
                                        } @else {
                                            div class="item-card-body muted" { "no body yet" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                @if !no_vote_items.is_empty() {
                    div class="component unsorted" {
                        div class="component-header" { "not yet compared" }
                        @for it in no_vote_items.iter() {
                            @let href = item_href(it);
                            div class="item-card" {
                                div class="item-card-header" {
                                    a class="item-link" href=(href) { code { "/" (item_display_path(it)) } }
                                    span class="muted" { "unranked" }
                                }
                                @if let Some(body) = bodies.get(it) {
                                    div class="item-card-body" { (body) }
                                } @else {
                                    div class="item-card-body muted" { "no body yet" }
                                }
                            }
                        }
                    }
                }
            }

            @if !group.recent_votes.is_empty() {
                details {
                    summary { "recent votes " span class="muted" { (format!("({})", group.recent_votes.len())) } }
                    @let now = now_ms();
                    @for v in group.recent_votes.iter().take(50) {
                        @let pct = ratio_pct(v.ratio_left, v.ratio_right);
                        @let hover = timeago::rfc3339_utc(v.ts);
                        @let ago = timeago::timeago(now, v.ts);
                        div class="vote" {
                            div class="vote-header" {
                                a class="item-link" href=(format!("/~/{}", v.a)) { code class="vote-left" { "/" (v.a) } }
                                span class="vote-ratio" { (format!("{}:{}", v.ratio_left, v.ratio_right)) }
                                a class="item-link" href=(format!("/~/{}", v.b)) { code class="vote-right" { "/" (v.b) } }
                            }
                            div class="ratio-bar" aria-label={(format!("ratio {}:{}", v.ratio_left, v.ratio_right))} {
                                div class="ratio-left" style={(format!("width: {:.3}%;", pct))} {}
                                div class="ratio-right" style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                            }
                            div class="vote-body" { (v.body) }
                            div class="vote-meta" title=(hover) {
                                span class="address" { "@" (actor_label(&v.actor)) }
                                " · "
                                (ago)
                            }
                        }
                    }
                }
            }
        },
    );

    Html(page.into_string()).into_response()
}
