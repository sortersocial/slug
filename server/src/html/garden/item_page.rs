use maud::html;

use crate::{
    html::forum::ThreadNav,
    path_types::ItemId,
    reducer::{ContentState, ScopeId},
    scope_rank::{
        build_children_rankings, build_children_rankings_in_group, build_rankings_for_item_set,
        resolve_scope_recursive, ChildrenRankings,
    },
};

use super::{
    access::content_for_garden_view,
    item::{item_display_path, item_href},
};

#[derive(Debug, Clone)]
pub(super) struct SiblingNavLink {
    pub(super) path: String,
}

#[derive(Debug, Clone)]
pub(super) struct SiblingNavGroup {
    pub(super) links: Vec<SiblingNavLink>,
}

/// Siblings under the same parent: one group per ranking component (ordered list), then one
/// group per isolated unranked sibling (each shows rank `1`, separated like components).
#[derive(Debug, Clone)]
pub(super) struct SiblingNavBar {
    pub(super) groups: Vec<SiblingNavGroup>,
    /// 1-based rank within the largest ranking component, when the current item is in it.
    pub(super) largest_group_rank: Option<(usize, usize)>,
    /// For the winner of the largest ranking group: percentile among that group's size
    /// (`((n - 1) * 100) / n`, e.g. 99 for n=100).
    pub(super) winner_percentile: Option<u32>,
}

/// Ordinal suffix for percentile display (`1st`, `2nd`, `3rd`, `99th`, …).
pub(super) fn ordinal_suffix(n: u32) -> &'static str {
    let mod100 = n % 100;
    if (11..=13).contains(&mod100) {
        return "th";
    }
    match n % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Percentile for the winner of a ranking group of size `n` (requires n ≥ 2).
pub(super) fn winner_percentile_for_group_size(n: usize) -> Option<u32> {
    if n < 2 {
        return None;
    }
    Some((((n - 1) * 100) / n) as u32)
}

#[derive(Debug, Clone)]
pub(super) struct RankHistoryEntryView {
    pub(super) ts: i64,
    pub(super) scope_rank: usize,
    pub(super) scope_total: usize,
    pub(super) scope_rank_delta: i32,
    pub(super) thread: String,
    /// 0-based index as [`crate::html::forum::ingest::thread_post_index_in_scope`] / `/t/tag/N`.
    pub(super) thread_post_index: usize,
    pub(super) caused_by: Vec<crate::reducer::VoteData>,
}

#[derive(Debug, Clone)]
pub(super) struct AspectRankingView {
    pub(super) slug: String,
    pub(super) prompt: Option<String>,
    pub(super) rankings: ChildrenRankings,
}

#[derive(Debug, Clone)]
pub(super) struct ItemPageViewModel {
    pub(super) item: String,
    pub(super) body: Option<String>,
    pub(super) sibling_nav: Option<SiblingNavBar>,
    /// False at the tilde ontology root (`~/`): sibling-rank footnote does not apply.
    pub(super) item_has_parent: bool,
    pub(super) child_rankings: ChildrenRankings,
    pub(super) aspect_rankings: Vec<AspectRankingView>,
    pub(super) child_depth: usize,
    pub(super) rank_history: Vec<RankHistoryEntryView>,
    /// Forum threads that mention or vote on this item.
    pub(super) threads: Vec<String>,
}

pub(super) fn aspect_rankings_for_parent(
    content: &ContentState,
    parent: &ItemId,
) -> Vec<AspectRankingView> {
    let parent = parent.clone().normalized_storage();
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
        .filter_map(|slug| {
            let group = content.aspect_group(&parent, &slug)?;
            if group.voted_pairs.is_empty() {
                return None;
            }
            let rankings = build_children_rankings_in_group(content, &parent, group);
            if rankings.component_rankings.is_empty() {
                return None;
            }
            Some(AspectRankingView {
                prompt: content.aspect_prompt(&slug).map(str::to_string),
                slug,
                rankings,
            })
        })
        .collect()
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
    for u in &rankings.unranked_items {
        groups.push(SiblingNavGroup {
            links: vec![SiblingNavLink {
                path: u.clone().normalized_storage().to_storage_string(),
            }],
        });
    }
    let sibling_total: usize = groups.iter().map(|g| g.links.len()).sum();
    if sibling_total <= 1 {
        return None;
    }

    // Components are already sorted largest-first; only the first is the "largest ranking group".
    let current_path = current.to_storage_string();
    let (largest_group_rank, winner_percentile) =
        match rankings.component_rankings.first() {
            Some(largest) if largest.ranked.len() >= 2 => {
                let of = largest.ranked.len();
                match largest.ranked.iter().position(|r| {
                    r.item.clone().normalized_storage().as_str() == current_path
                }) {
                    Some(idx) => {
                        let rank = idx + 1;
                        let pct = if rank == 1 {
                            winner_percentile_for_group_size(of)
                        } else {
                            None
                        };
                        (Some((rank, of)), pct)
                    }
                    None => (None, None),
                }
            }
            _ => (None, None),
        };

    Some(SiblingNavBar {
        groups,
        largest_group_rank,
        winner_percentile,
    })
}

pub(super) fn sibling_nav_markup(nav: &ThreadNav, bar: &SiblingNavBar, current_item: &str) -> maud::Markup {
    html! {
        div class="ont-sibling-nav-wrap" {
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
            @if let Some((rank, of)) = bar.largest_group_rank {
                div class="ont-sibling-nav-score muted" {
                    span class="ont-sibling-nav-rank" {
                        (format!("rank: {}/{}", rank, of))
                    }
                    @if let Some(pct) = bar.winner_percentile {
                        span class="ont-sibling-nav-percentile" {
                            (format!("top {}{} percentile", pct, ordinal_suffix(pct)))
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
                                aspect: None,
                                ..
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

#[cfg(test)]
mod unit_tests {
    use super::{ordinal_suffix, winner_percentile_for_group_size};

    #[test]
    fn ordinal_suffix_handles_teens_and_units() {
        assert_eq!(ordinal_suffix(1), "st");
        assert_eq!(ordinal_suffix(2), "nd");
        assert_eq!(ordinal_suffix(3), "rd");
        assert_eq!(ordinal_suffix(4), "th");
        assert_eq!(ordinal_suffix(11), "th");
        assert_eq!(ordinal_suffix(12), "th");
        assert_eq!(ordinal_suffix(13), "th");
        assert_eq!(ordinal_suffix(21), "st");
        assert_eq!(ordinal_suffix(99), "th");
    }

    #[test]
    fn winner_percentile_matches_group_size() {
        assert_eq!(winner_percentile_for_group_size(1), None);
        assert_eq!(winner_percentile_for_group_size(2), Some(50));
        assert_eq!(winner_percentile_for_group_size(5), Some(80));
        assert_eq!(winner_percentile_for_group_size(100), Some(99));
    }
}

pub(super) fn build_item_page_view_model(
    reduced: &crate::reducer::ReducerState,
    scope: &ScopeId,
    item: &str,
    child_depth: usize,
) -> ItemPageViewModel {
    let content = content_for_garden_view(reduced, scope);
    let item_key = ItemId::parse(item)
        .unwrap_or_else(|| ItemId::parse("~/").unwrap())
        .normalized_storage();
    let item_has_parent = item_key.parent().is_some();
    let child_depth = child_depth.max(1);
    let child_rankings = if child_depth > 1 {
        let items = resolve_scope_recursive(content, &[item_key.as_str().to_string()], child_depth);
        build_rankings_for_item_set(content, &items)
    } else {
        build_children_rankings(content, &item_key)
    };
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
        aspect_rankings: aspect_rankings_for_parent(content, &item_key),
        child_depth,
        rank_history,
        threads,
    }
}

