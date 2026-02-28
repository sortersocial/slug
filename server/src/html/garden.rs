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

#[derive(Debug, Clone)]
struct ScopedComponent {
    pairs: usize,
    ranked: Vec<crate::ranking::RankedItem>,
}

#[derive(Debug, Clone)]
struct ScopeViewModel {
    parent_scope: String,
    parent_body: Option<String>,
    component_rankings: Vec<ScopedComponent>,
    isolate_items: Vec<String>,
    no_vote_items: Vec<String>,
    recent_votes: Vec<crate::reducer::VoteData>,
}

fn build_scope_view_model(
    reduced: &crate::reducer::ReducerState,
    parent_scope: &str,
    recent_vote_limit: usize,
) -> ScopeViewModel {
    let group = &reduced.ranking_group;
    let mut items_in_scope: Vec<String> = reduced
        .item_children
        .get(parent_scope)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    items_in_scope.sort();

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

    // Items in the scope that have never been voted.
    let mut no_vote_items: Vec<String> = items_in_scope
        .iter()
        .filter(|it| !group.item_to_idx.contains_key(*it))
        .cloned()
        .collect();
    no_vote_items.sort();

    // Sort components by size descending, then by item indices for stability.
    comps_local.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let component_rankings: Vec<ScopedComponent> = comps_local
        .iter()
        .map(|comp_local| {
            let comp_global: Vec<usize> = comp_local
                .iter()
                .filter_map(|li| local_to_global.get(*li).copied())
                .collect();
            let comp_set: std::collections::HashSet<usize> = comp_global.iter().copied().collect();
            let ranked = ranked_items_subset(group, &comp_global, 10000, 1e-8);
            let pairs = group
                .voted_pairs
                .iter()
                .filter(|(i, j)| comp_set.contains(i) && comp_set.contains(j))
                .count();
            ScopedComponent { pairs, ranked }
        })
        .collect();

    let mut isolate_items: Vec<String> = isolate_local_idxs
        .into_iter()
        .filter_map(|li| local_to_global.get(li).copied())
        .filter_map(|idx| group.idx_to_item.get(idx).cloned())
        .collect();
    isolate_items.sort();

    let scope_children: std::collections::HashSet<String> = items_in_scope.iter().cloned().collect();
    let recent_votes: Vec<crate::reducer::VoteData> = group
        .recent_votes
        .iter()
        .filter(|v| scope_children.contains(&v.a) && scope_children.contains(&v.b))
        .take(recent_vote_limit)
        .cloned()
        .collect();

    ScopeViewModel {
        parent_scope: parent_scope.to_string(),
        parent_body: if parent_scope.is_empty() {
            None
        } else {
            reduced.item_bodies.get(parent_scope).cloned()
        },
        component_rankings,
        isolate_items,
        no_vote_items,
        recent_votes,
    }
}

async fn render_scope_view(state: AppState, path: OntologyPath) -> axum::response::Response {
    let model = {
        let reduced = state.reduced.read().await;
        build_scope_view_model(&reduced, path.as_str(), 50)
    };

    let page = layout(
        &format!("~/{}", path.as_str()),
        "view-ontology",
        html! {
            nav class="breadcrumb" { (bc_path(&path)) }
            h2 { "~/" (model.parent_scope) }
            @if let Some(body) = &model.parent_body {
                div class="item-card" {
                    div class="item-card-header" { "body" }
                    div class="item-card-body" { (body) }
                }
            }
            h3 { "ranked child groups" }
            @if model.component_rankings.is_empty() {
                p class="muted" { "no voted pairs yet in this scope" }
            } @else {
                @for (ci, comp) in model.component_rankings.iter().enumerate() {
                    div class="component" {
                        div class="component-header" {
                            (format!("ordering {} items={} pairs={}", ci + 1, comp.ranked.len(), comp.pairs))
                        }
                        ol class="ranking" {
                            @for r in comp.ranked.iter() {
                                @let item_url = item_href(&r.item);
                                li {
                                    a class="item-link" href=(item_url) { code { "/" (item_display_path(&r.item)) } }
                                }
                            }
                        }
                    }
                }
            }

            @if !model.isolate_items.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "isolates" }
                    ul {
                        @for name in &model.isolate_items {
                            li {
                                @let href = item_href(&name);
                                a class="item-link" href=(href) { code { "/" (item_display_path(&name)) } }
                            }
                        }
                    }
                }
            }

            @if !model.no_vote_items.is_empty() {
                div class="component unsorted" {
                    div class="component-header" { "not yet compared" }
                    ul {
                        @for it in &model.no_vote_items {
                            li {
                                @let href = item_href(it);
                                a class="item-link" href=(href) { code { "/" (item_display_path(it)) } }
                            }
                        }
                    }
                }
            }

            @if !model.recent_votes.is_empty() {
                details {
                    summary { "recent votes in this scope " span class="muted" { (format!("({})", model.recent_votes.len())) } }
                    @let now = now_ms();
                    @for v in model.recent_votes.iter() {
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

#[cfg(test)]
mod tests {
    use super::build_scope_view_model;
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
    fn scope_model_includes_parent_body_without_votes() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             /topic {topic body}\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n",
        );

        let model = build_scope_view_model(&reduced, "topic", 50);
        assert_eq!(model.parent_body.as_deref(), Some("topic body"));
        assert!(model.component_rankings.is_empty());
        assert!(model.isolate_items.is_empty());
        assert_eq!(model.no_vote_items, vec!["topic/a".to_string(), "topic/b".to_string()]);
    }

    #[test]
    fn scope_model_filters_recent_votes_to_direct_scope_children() {
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

        let model = build_scope_view_model(&reduced, "topic", 50);
        assert_eq!(model.recent_votes.len(), 1);
        assert_eq!(model.recent_votes[0].a, "topic/a");
        assert_eq!(model.recent_votes[0].b, "topic/b");
    }

    #[test]
    fn scope_model_retains_isolates_when_child_only_votes_outside_scope() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             /topic {topic body}\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n\
             /other/x {x}\n\
             /topic/a 2:1 /other/x {cross-scope vote}\n",
        );

        let model = build_scope_view_model(&reduced, "topic", 50);
        assert!(model.component_rankings.is_empty());
        assert_eq!(model.isolate_items, vec!["topic/a".to_string()]);
        assert_eq!(model.no_vote_items, vec!["topic/b".to_string()]);
    }

    #[test]
    fn scope_model_builds_ranked_components_for_in_scope_votes() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "@00000000-0000-0000-0000-000000000000:test:local/test\n\
             /topic/a {alpha}\n\
             /topic/b {beta}\n\
             /topic/a 3:1 /topic/b {a beats b}\n",
        );

        let model = build_scope_view_model(&reduced, "topic", 50);
        assert_eq!(model.component_rankings.len(), 1);
        assert_eq!(model.component_rankings[0].pairs, 1);
        let names: Vec<&str> = model.component_rankings[0]
            .ranked
            .iter()
            .map(|r| r.item.as_str())
            .collect();
        assert_eq!(names, vec!["topic/a", "topic/b"]);
    }
}
