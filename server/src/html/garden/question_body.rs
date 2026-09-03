//! Shared question-page sections: aspects, compact standings, collection members.
//! Used by `GET /q/…` and the public question index (`GET /`).

use std::collections::HashSet;

use maud::html;

use crate::{
    html::forum::ThreadNav,
    path_types::ItemId,
    reducer::{BorderPairState, ContentState, GroupState, MembershipStatus},
    scope_rank::{build_children_rankings, build_children_rankings_in_group, ChildrenRankings},
};

use super::item::item_display_path;
use super::item_page::pair_weight_label;

pub(super) fn aspects_for_scope(
    content: &ContentState,
    collection: &ItemId,
) -> Vec<(String, Option<String>)> {
    let parent = collection.clone().normalized_storage();
    let mut slugs: Vec<String> = content
        .aspect_groups
        .keys()
        .filter(|(p, _)| p == &parent)
        .map(|(_, slug)| slug.clone())
        .collect();
    slugs.sort();
    slugs.dedup();
    slugs
        .into_iter()
        .map(|slug| {
            let prompt = content
                .aspect_prompt(&slug)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            (slug, prompt)
        })
        .collect()
}

pub(super) fn question_rankings(
    content: &ContentState,
    collection: &ItemId,
    aspect: Option<&str>,
) -> ChildrenRankings {
    if let Some(slug) = aspect.filter(|s| !s.is_empty()) {
        if let Some(group) = content.aspect_group(collection, slug) {
            return build_children_rankings_in_group(content, collection, group);
        }
    }
    build_children_rankings(content, collection)
}

#[allow(dead_code)]
pub(super) fn scoped_comparison_count(group: &GroupState, members: &[ItemId]) -> usize {
    let set: HashSet<&ItemId> = members.iter().collect();
    group
        .voted_pairs
        .iter()
        .filter(|(i, j)| {
            group.idx_to_item.get(*i).is_some_and(|a| set.contains(a))
                && group.idx_to_item.get(*j).is_some_and(|b| set.contains(b))
        })
        .count()
}

#[allow(dead_code)]
pub(super) fn question_comparison_count(
    content: &ContentState,
    collection: &ItemId,
    aspect: Option<&str>,
) -> usize {
    let members = content.members_of(collection);
    if let Some(slug) = aspect.filter(|s| !s.is_empty()) {
        if let Some(group) = content.aspect_group(collection, slug) {
            return scoped_comparison_count(group, &members);
        }
        return 0;
    }
    scoped_comparison_count(&content.ranking_group, &members)
}

#[allow(dead_code)]
pub(super) fn question_vote_count(
    content: &ContentState,
    collection: &ItemId,
    aspect: Option<&str>,
) -> usize {
    let members = content.members_of(collection);
    let set: HashSet<ItemId> = members.iter().cloned().collect();
    if let Some(slug) = aspect.filter(|s| !s.is_empty()) {
        return content
            .aspect_group(collection, slug)
            .map(|g| {
                g.recent_votes
                    .iter()
                    .filter(|v| set.contains(&v.a) && set.contains(&v.b))
                    .count()
            })
            .unwrap_or(0);
    }
    let mut n = 0usize;
    for m in &members {
        if let Some(votes) = content.item_votes.get(m) {
            n += votes
                .iter()
                .filter(|v| set.contains(&v.a) && set.contains(&v.b))
                .count();
        }
    }
    n / 2
}

fn suspended_in_scope(content: &ContentState, parent: &ItemId) -> Vec<BorderPairState> {
    let parent = parent.ontology_leaf();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (c, p) in content.containment.keys().chain(content.borders.keys()) {
        if p != &parent || !seen.insert(c.clone()) {
            continue;
        }
        if let Some(st) = content.border_state(c, p) {
            if st.status == MembershipStatus::Suspended {
                out.push(st);
            }
        }
    }
    out.sort_by(|a, b| a.child.as_str().cmp(b.child.as_str()));
    out
}

/// Compact ranking: position, leaf href, score. `limit` truncates ranked rows (home preview).
pub(super) fn compact_standings_markup(
    rankings: &ChildrenRankings,
    nav: &ThreadNav,
    limit: Option<usize>,
) -> maud::Markup {
    let mut rows: Vec<(usize, ItemId, f64, usize)> = Vec::new();
    for comp in &rankings.component_rankings {
        for r in &comp.ranked {
            let pos = rows.len() + 1;
            rows.push((pos, r.item.clone(), r.score, comp.pairs));
            if limit.is_some_and(|n| rows.len() >= n) {
                break;
            }
        }
        if limit.is_some_and(|n| rows.len() >= n) {
            break;
        }
    }
    html! {
        @if rows.is_empty() && rankings.unranked_items.is_empty() {
            p class="muted" { "no standings yet" }
        } @else {
            @if !rows.is_empty() {
                ol class="question-standings-list ont-ranking-list" {
                    @for (pos, item, score, pairs) in &rows {
                        li data-garden-item=(item.as_str()) {
                            span class="question-standings-rank" { (pos) }
                            " "
                            a class="item-link" href=(nav.garden_item_href(item)) {
                                code { (item_display_path(item.as_str())) }
                            }
                            span class="ont-rank-score muted" {
                                (format!("{:.3}", score))
                                " · "
                                (format!("{pairs}p"))
                            }
                        }
                    }
                }
            }
            @if limit.is_none() && !rankings.unranked_items.is_empty() {
                ul class="question-standings-unranked ont-group-list" {
                    @for item in &rankings.unranked_items {
                        li data-garden-item=(item.as_str()) {
                            a class="item-link" href=(nav.garden_item_href(item)) {
                                code { (item_display_path(item.as_str())) }
                            }
                            span class="muted" { " · unranked" }
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn question_standings_region(
    rankings: &ChildrenRankings,
    nav: &ThreadNav,
) -> maud::Markup {
    html! {
        section id="question-standings-region" class="question-standings" data-testid="question-standings" {
            h2 { "standings" }
            (compact_standings_markup(rankings, nav, None))
        }
    }
}

pub(super) fn question_aspects_markup(
    nav: &ThreadNav,
    collection: &ItemId,
    current: Option<&str>,
    aspects: &[(String, Option<String>)],
) -> maud::Markup {
    if aspects.is_empty() && current.is_none() {
        return html! {};
    }
    let leaf = collection.last_segment();
    html! {
        section class="question-aspects" data-testid="question-aspects" {
            h2 { "aspects" }
            @if current.is_some() {
                p class="question-aspects-canonical" {
                    a data-testid="question-canonical-link" href=(nav.question_href(&leaf, None)) {
                        "canonical question"
                    }
                }
            }
            ul class="question-aspects-list" {
                @for (slug, prompt) in aspects {
                    li {
                        @if current == Some(slug.as_str()) {
                            span class="question-aspect-current" { ":" (slug) }
                        } @else {
                            a href=(nav.question_href(&leaf, Some(slug))) { ":" (slug) }
                        }
                        @if let Some(p) = prompt {
                            span class="muted" { " — " (p) }
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn question_members_markup(
    content: &ContentState,
    collection: &ItemId,
    nav: &ThreadNav,
) -> maud::Markup {
    let active = content.members_of(collection);
    let suspended = suspended_in_scope(content, collection);
    html! {
        section class="question-members" data-testid="question-members" {
            h2 { "members" }
            @if active.is_empty() && suspended.is_empty() {
                p class="muted" { "no members yet" }
            } @else {
                ul class="question-members-list ont-group-list" {
                    @for m in &active {
                        li {
                            a href=(nav.garden_item_href(m)) {
                                (item_display_path(m.as_str()))
                            }
                        }
                    }
                    @for st in &suspended {
                        li class="muted ont-border-suspended" {
                            a href=(nav.garden_item_href(&st.child)) {
                                (item_display_path(st.child.as_str()))
                            }
                            " "
                            span { "suspended" }
                            " "
                            span {
                                (pair_weight_label(st))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Aspects + standings + members (thread link stays in the question header).
pub(super) fn question_context_sections(
    content: &ContentState,
    nav: &ThreadNav,
    collection: &ItemId,
    aspect: Option<&str>,
) -> maud::Markup {
    let aspects = aspects_for_scope(content, collection);
    let rankings = question_rankings(content, collection, aspect);
    html! {
        (question_aspects_markup(nav, collection, aspect, &aspects))
        (question_standings_region(&rankings, nav))
        (question_members_markup(content, collection, nav))
    }
}
