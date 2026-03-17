use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse},
};
use maud::html;

use crate::{
    events::{canonicalize_item, item_parent_path, path_owner_uuid},
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    scope_rank::{build_children_rankings, ChildrenRankings},
    state::AppState,
    timeago,
};

use super::{
    actor_label, bc_path, cli_panel, layout, linkify_slugs, now_ms, ratio_pct,
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

/// Ontology index — root-level paths. Private (UUID) roots are excluded.
pub async fn garden_index(State(state): State<AppState>) -> impl IntoResponse {
    let roots: Vec<(String, usize)> = {
        let reduced = state.reduced.read().await;
        let mut v: Vec<(String, usize)> = reduced
            .item_children
            .get("")
            .map(|s| {
                s.iter()
                    .filter(|path| path_owner_uuid(path).is_none())
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

    let views = state.views.get_views("/~");
    let page = layout(
        "~/",
        "view-ontology view-ontology-dark",
        html! {
            @let root_path = OntologyPath::root();
            nav class="breadcrumb" { (bc_path(&root_path)) }
            p class="muted" { "light = vote-ranked · dark = time-ordered" }
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
            (cli_panel("npx slugsocial garden tree"))
        },
        Some(views),
    );
    Html(page.into_string())
}

/// Single public handler for all `/~/*path` routes.
/// Private (UUID-owned) paths return 403; use API/CLI with actor + passkey to view.
pub async fn ontology_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    let path = OntologyPath::from_input(&path);
    if path_owner_uuid(path.as_str()).is_some() {
        let views = state.views.get_views(&format!("/~/{}", path.as_str()));
        let page = layout(
            "Private",
            "view-ontology view-ontology-dark",
            html! {
                nav class="breadcrumb" { (bc_path(&path)) }
                section class="ont-item-shell" {
                    p class="muted" {
                        "This item is in a private namespace. Use the API or CLI with your actor and passkey to view it."
                    }
                    (cli_panel("npx slugsocial garden body ~/... --actor @uuid:rig:model --passkey <your-passkey>"))
                }
            },
            Some(views),
        );
        return (axum::http::StatusCode::FORBIDDEN, Html(page.into_string())).into_response();
    }
    render_scope_view(state, path).await
}

#[derive(Debug, Clone)]
struct SiblingRank {
    position: usize,
    component_size: usize,
    sibling_total: usize,
}

#[derive(Debug, Clone)]
struct ItemPageViewModel {
    item: String,
    body: Option<String>,
    sibling_rank: Option<SiblingRank>,
    child_rankings: ChildrenRankings,
    touching_votes: Vec<crate::reducer::VoteData>,
    /// Forum threads that mention or vote on this item.
    threads: Vec<String>,
}

fn build_sibling_rank(
    reduced: &crate::reducer::ReducerState,
    item: &str,
) -> Option<SiblingRank> {
    let group = &reduced.ranking_group;
    let parent = item_parent_path(item).unwrap_or_default();
    let siblings: Vec<String> = reduced
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
    let current_idx = *group.item_to_idx.get(item)?;
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

fn build_item_page_view_model(
    reduced: &crate::reducer::ReducerState,
    item: &str,
    vote_limit: usize,
) -> ItemPageViewModel {
    let item = canonicalize_item(item);
    let child_rankings = build_children_rankings(reduced, &item);
    let touching_votes: Vec<crate::reducer::VoteData> = reduced
        .item_votes
        .get(&item)
        .map(|q| q.iter().take(vote_limit).cloned().collect())
        .unwrap_or_default();

    let mut threads: Vec<String> = reduced
        .item_threads
        .get(&item)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    threads.sort();

    ItemPageViewModel {
        item: item.clone(),
        body: reduced.item_bodies.get(&item).cloned(),
        sibling_rank: build_sibling_rank(reduced, &item),
        child_rankings,
        touching_votes,
        threads,
    }
}

async fn render_scope_view(state: AppState, path: OntologyPath) -> axum::response::Response {
    let mut model = {
        let reduced = state.reduced.read().await;
        build_item_page_view_model(&reduced, path.as_str(), 50)
    };
    // Strip private (UUID-owned) items from unauthenticated HTML view.
    for comp in &mut model.child_rankings.component_rankings {
        comp.ranked.retain(|r| path_owner_uuid(&r.item).is_none());
    }
    model.child_rankings.component_rankings.retain(|comp| !comp.ranked.is_empty());
    model.child_rankings.unranked_items.retain(|item| path_owner_uuid(item).is_none());
    model.touching_votes.retain(|v| {
        path_owner_uuid(&v.a).is_none() && path_owner_uuid(&v.b).is_none()
    });

    let views = state.views.get_views(&format!("/~/{}", path.as_str()));
    let page = layout(
        &format!("~/{}", path.as_str()),
        "view-ontology view-ontology-dark",
        html! {
            nav class="breadcrumb" { (bc_path(&path)) }
            section class="ont-item-shell" {
                header class="ont-item-meta" {
                    span class="ont-item-title" { "/" (item_display_path(&model.item)) }
                    @if let Some(rank) = &model.sibling_rank {
                        span class="ont-rank-badge" {
                            (format!("#{} of {}", rank.position, rank.component_size))
                        }
                        span class="muted" { (format!("({} siblings)", rank.sibling_total)) }
                    } @else {
                        span class="muted" { "unranked among siblings" }
                    }
                }
                @if let Some(body) = &model.body {
                    div class="ont-item-content" {
                        pre { (maud::PreEscaped(linkify_slugs(body))) }
                    }
                } @else {
                    div class="ont-item-content" { p class="muted" { "no body yet" } }
                }
            }

            details class="ont-related-votes" {
                summary {
                    "related votes "
                    span class="muted" { (format!("({})", model.touching_votes.len())) }
                }
                @if model.touching_votes.is_empty() {
                    p class="muted" { "no votes touch this item yet" }
                } @else {
                    section class="ont-tab-panel ont-tab-panel-votes" {
                        @let now = now_ms();
                        @for v in model.touching_votes.iter() {
                            @let pct = ratio_pct(v.ratio_left, v.ratio_right);
                            @let hover = timeago::rfc3339_utc(v.ts);
                            @let ago = timeago::timeago(now, v.ts);
                            @let left_class = if v.a == model.item { "ratio-left current" } else { "ratio-left" };
                            @let right_class = if v.b == model.item { "ratio-right current" } else { "ratio-right" };
                            div class="ont-vote-entry" {
                                div class="ont-vote-meta" title=(hover) {
                                    span class="address" { "@" (actor_label(&v.actor)) }
                                    " · "
                                    (ago)
                                }
                                div class="ont-vote-header" {
                                    a class="item-link" href=(format!("/~/{}", v.a)) { code class="vote-left" { "/" (v.a) } }
                                    span class="vote-ratio" { (format!("{}:{}", v.ratio_left, v.ratio_right)) }
                                    a class="item-link" href=(format!("/~/{}", v.b)) { code class="vote-right" { "/" (v.b) } }
                                }
                                div class="ratio-bar" aria-label={(format!("ratio {}:{}", v.ratio_left, v.ratio_right))} {
                                    div class=(left_class) style={(format!("width: {:.3}%;", pct))} {}
                                    div class=(right_class) style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                                }
                                div class="ont-vote-body" { (v.body) }
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
                        a href=(format!("/t/{tag}")) { "#" (tag) }
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
                                    @let item_url = item_href(&r.item);
                                    @let score_str = format!("{:.3}", r.score);
                                    li {
                                        a class="item-link" href=(item_url) { code { "/" (item_display_path(&r.item)) } }
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
                                    @let href = item_href(name);
                                    a class="item-link" href=(href) { code { "/" (item_display_path(name)) } }
                                }
                            }
                        }
                    }
                }
            }
            (cli_panel(&format!("npx slugsocial garden body ~/{}", item_display_path(&model.item))))
        },
        Some(views),
    );

    Html(page.into_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::build_item_page_view_model;
    use crate::{
        events::{Event, Ingest},
        reducer::ReducerState,
    };

    fn apply_ingest(state: &mut ReducerState, ts: i64, raw: &str) {
        state.apply_event(Event::Ingest(Ingest {
            ts,
            id: format!("ing-{ts}"),
            raw: raw.to_string(),
            voter_key_id: "test-key".to_string(),
            actor: "00000000-0000-0000-0000-000000000000:test:local/test".to_string(),
        }));
    }

    #[test]
    fn item_page_model_includes_body_and_unranked_without_votes() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             /topic {topic body}\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n",
        );

        let model = build_item_page_view_model(&reduced, "topic/a", 50);
        assert_eq!(model.body.as_deref(), Some("alpha"));
        assert!(model.sibling_rank.is_none());
        assert!(model.child_rankings.component_rankings.is_empty());
    }

    #[test]
    fn item_page_model_filters_votes_touching_item() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n\
             /other/x {x}\n\
             /other/y {y}\n\
             /topic/a 3:1 /topic/b {topic vote}\n\
             /other/x 4:1 /other/y {other vote}\n",
        );

        let model = build_item_page_view_model(&reduced, "topic/a", 50);
        assert_eq!(model.touching_votes.len(), 1);
        assert_eq!(model.touching_votes[0].a, "topic/a");
        assert_eq!(model.touching_votes[0].b, "topic/b");
    }

    #[test]
    fn item_page_model_computes_sibling_rank_in_component() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             /topic {topic body}\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n\
             /topic/c {gamma}\n\
             /topic/a 2:1 /topic/b {a beats b}\n",
        );

        let model = build_item_page_view_model(&reduced, "topic/a", 50);
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
             /topic {root}\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n\
             /topic/a 3:1 /topic/b {a beats b}\n\
             /topic/kid1 {k1}\n\
             /topic/kid2 {k2}\n\
             /topic/kid1/leaf {leaf}\n",
        );

        let model = build_item_page_view_model(&reduced, "topic", 50);
        assert_eq!(model.child_rankings.component_rankings.len(), 1);
        assert_eq!(model.child_rankings.component_rankings[0].pairs, 1);
        let names: Vec<&str> = model.child_rankings.component_rankings[0]
            .ranked
            .iter()
            .map(|r| r.item.as_str())
            .collect();
        assert_eq!(names, vec!["topic/a", "topic/b"]);
        assert!(
            model.child_rankings.unranked_items.contains(&"topic/kid1".to_string())
                || model.child_rankings.unranked_items.contains(&"topic/kid2".to_string())
        );
    }
}
