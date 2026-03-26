use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use rand::seq::SliceRandom;
use serde::Deserialize;
use slug_types::*;
use std::collections::HashSet;

use crate::{
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    state::AppState,
};

use super::auth::verified_actor_uuid;
use super::helpers::{
    api_error, compute_connectivity_stats, is_pair_voted, item_path_for_api, paginate_rankings,
    parse_parent_specs, pick_random_distinct,
};

// ============================================================================
// Rank / Pair -- one-ranking (parent-path-filtered)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RankQuery {
    /// Parent path(s). Comma-separated to merge scopes: ~/a,~/b (e.g. rank ~/models ~/ai-models).
    /// Use `~` alone for global ranking across all items.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Skip the first N items (for pagination).
    #[serde(default)]
    pub offset: Option<usize>,
    /// When true, each RankRow includes a `percent` field: score as % of max score in its component.
    #[serde(default)]
    pub percent: Option<bool>,
    /// How many levels deep to resolve children (default 1 = direct children only).
    #[serde(default)]
    pub depth: Option<usize>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

pub async fn get_rank(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<RankQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());
    let specs = parse_parent_specs(q.parent.as_ref());
    let is_global = q.parent.as_deref().map(|p| p.trim() == "~").unwrap_or(false);
    let depth = q.depth.unwrap_or(1).max(1);

    // 404 when a specific named path is requested but doesn't exist as a known item or parent.
    if !is_global && !specs.is_empty() {
        let none_exist = specs.iter().all(|spec| {
            let canon = crate::events::canonicalize_item(spec);
            !reduced.items.contains(&canon) && !reduced.item_children.contains_key(&canon)
        });
        if none_exist {
            return api_error(
                StatusCode::NOT_FOUND,
                "path not found",
                Some(format!("{} does not exist", specs.join(", "))),
            );
        }
    }

    let rankings = if is_global {
        let all_items: Vec<String> = reduced.items.iter().cloned().collect();
        crate::scope_rank::build_rankings_for_item_set(&reduced, &all_items)
    } else if specs.is_empty() {
        crate::scope_rank::build_children_rankings(&reduced, "")
    } else if depth > 1 {
        let items = crate::scope_rank::resolve_scope_recursive(&reduced, &specs, depth);
        crate::scope_rank::build_rankings_for_item_set(&reduced, &items)
    } else {
        let items = crate::scope_rank::resolve_scope(&reduced, &specs);
        crate::scope_rank::build_rankings_for_item_set(&reduced, &items)
    };

    let want_percent = q.percent.unwrap_or(false);

    let components: Vec<RankComponent> = rankings
        .component_rankings
        .into_iter()
        .map(|c| {
            let max_score = c.ranked.first().map(|r| r.score).unwrap_or(1.0).max(1e-12);
            RankComponent {
                pairs: c.pairs,
                ranking: c
                    .ranked
                    .into_iter()
                    .filter(|r| match crate::events::path_owner_uuid(&r.item) {
                        None => true,
                        Some(owner) => authed_uuid.as_deref() == Some(owner),
                    })
                    .map(|r| RankRow {
                        item: item_path_for_api(&r.item),
                        percent: if want_percent {
                            Some((r.score / max_score) * 100.0)
                        } else {
                            None
                        },
                        score: r.score,
                    })
                    .collect(),
            }
        })
        .collect();

    let prefixed_unranked: Vec<String> = rankings
        .unranked_items
        .into_iter()
        .filter(|p| match crate::events::path_owner_uuid(p) {
            None => true,
            Some(owner) => authed_uuid.as_deref() == Some(owner),
        })
        .map(|s| item_path_for_api(&s))
        .collect();

    let offset = q.offset.unwrap_or(0);
    let (components, unranked_items) = if offset > 0 || q.limit.is_some() {
        paginate_rankings(components, prefixed_unranked, offset, q.limit)
    } else {
        (components, prefixed_unranked)
    };

    Json(RankResponse {
        components,
        unranked_items,
    })
    .into_response()
}

// ============================================================================
// Global rank -- flat, paginated ranking across all items
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct GlobalRankQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
    /// When true, add a `percent` field to each row (top item = 100, bottom = 0).
    #[serde(default)]
    pub percent: Option<bool>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

pub async fn get_global_rank(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<GlobalRankQuery>,
) -> impl IntoResponse {
    const DEFAULT_LIMIT: usize = 50;
    const MAX_LIMIT: usize = 500;

    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = q.offset.unwrap_or(0);
    let want_percent = q.percent.unwrap_or(false);

    let reduced_arc = state.reduced.clone();
    // Write lock needed to update cached_scores when dirty.
    let reduced = reduced_arc.write().await;
    let passkey = headers
        .get("x-slug-passkey")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    // Find connected components in global graph; rank within each; largest component first.
    let n = reduced.ranking_group.idx_to_item.len();
    let (mut comps, _) = connected_components_from_voted_pairs(
        n, reduced.ranking_group.voted_pairs.iter().copied(),
    );
    comps.sort_by(|a, b| b.len().cmp(&a.len()));

    let mut ranked: Vec<RankRow> = Vec::new();
    for comp in &comps {
        let items = ranked_items_subset(&reduced.ranking_group, comp, 10000, 1e-8);
        let top = items.first().map(|r| r.score).unwrap_or(1.0);
        let bot = items.last().map(|r| r.score).unwrap_or(0.0);
        let range = (top - bot).max(1e-12);
        for r in items {
            if let Some(owner) = crate::events::path_owner_uuid(&r.item) {
                if authed_uuid.as_deref() != Some(owner) { continue; }
            }
            let pct = want_percent.then(|| ((r.score - bot) / range * 100.0).clamp(0.0, 100.0));
            ranked.push(RankRow { item: item_path_for_api(&r.item), score: r.score, percent: pct });
        }
    }

    let ranked_total = ranked.len();

    // Items that exist but have never been voted on.
    let mut unranked: Vec<String> = reduced
        .items
        .iter()
        .filter(|it| !reduced.ranking_group.item_to_idx.contains_key(*it))
        .filter(|it| match crate::events::path_owner_uuid(it) {
            None => true,
            Some(owner) => authed_uuid.as_deref() == Some(owner),
        })
        .cloned()
        .collect();
    unranked.sort();
    let unranked_total = unranked.len();

    let page: Vec<RankRow> = ranked
        .into_iter()
        .chain(unranked.into_iter().map(|it| RankRow {
            item: item_path_for_api(&it),
            score: 0.0,
            percent: want_percent.then_some(0.0),
        }))
        .skip(offset)
        .take(limit)
        .collect();

    Json(GlobalRankResponse {
        ranked_total,
        unranked_total,
        offset,
        limit,
        items: page,
    })
    .into_response()
}

// ============================================================================
// Rank history -- per-item rank ledger
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RankHistoryQuery {
    pub item: String,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

pub async fn get_rank_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RankHistoryQuery>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let passkey = headers
        .get("x-slug-passkey")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(q.passkey);
    let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());

    let item = crate::events::canonicalize_item(&q.item);

    // Private item access check.
    if let Some(owner) = crate::events::path_owner_uuid(&item) {
        if authed_uuid.as_deref() != Some(owner) {
            return (axum::http::StatusCode::FORBIDDEN, Json(slug_types::ApiError {
                ok: false,
                error: "forbidden".to_string(),
                hint: None,
            })).into_response();
        }
    }

    let entries = reduced.rank_history.get(&item).cloned().unwrap_or_default();

    // Resolve caused_by at query time: look up the ingest and filter relevant votes.
    let history: Vec<slug_types::RankHistoryRow> = entries.iter().map(|e| {
        let caused_by: Vec<VoteRow> = reduced.ingests_by_id.get(&e.post_id)
            .and_then(|ing| crate::dsl::parse_full(&ing.raw).ok())
            .map(|doc| {
                doc.statements.into_iter().filter_map(|s| {
                    if let crate::dsl::Stmt::Vote { item1, item2, ratio_left, ratio_right, explanation } = s {
                        let a = crate::events::canonicalize_item(&item1);
                        let b = crate::events::canonicalize_item(&item2);
                        if a == item || b == item {
                            Some(VoteRow {
                                ts: e.ts,
                                a: item_path_for_api(&a),
                                b: item_path_for_api(&b),
                                ratio: format!("{}:{}", ratio_left, ratio_right),
                                actor: reduced.ingests_by_id.get(&e.post_id).map(|ing| ing.actor.clone()),
                                body: explanation,
                                thread: Some(format!("#{}", e.thread)),
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }).collect()
            })
            .unwrap_or_default();

        // Compute 1-indexed chronological position within the thread.
        // ingests_by_thread is most-recent-first, so index from back + 1.
        let thread_post_index = reduced.ingests_by_thread
            .get(&e.thread)
            .and_then(|q| q.iter().rev().position(|id| id == &e.post_id))
            .map(|i| i + 1)
            .unwrap_or(0);

        slug_types::RankHistoryRow {
            ts: e.ts,
            scope_rank: e.scope_rank,
            scope_rank_delta: e.scope_rank_delta,
            scope_total: e.scope_total,
            global_rank: e.global_rank,
            global_rank_delta: e.global_rank_delta,
            global_total: e.global_total,
            score: e.score,
            thread: format!("#{}", e.thread),
            thread_post_index,
            caused_by,
        }
    }).collect();

    Json(slug_types::RankHistoryResponse {
        item: item_path_for_api(&item),
        history,
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct PairQuery {
    /// Parent path(s). Comma-separated to merge scopes: ~/a,~/b (e.g. rank ~/models ~/ai-models).
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub random: Option<bool>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub passkey: Option<String>,
}

pub async fn get_pair(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<PairQuery>) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let force_random = q.random.unwrap_or(false);

    let pool: Vec<String> = {
        let reduced = reduced_arc.read().await;
        let passkey = headers.get("x-slug-passkey").and_then(|v| v.to_str().ok()).map(|s| s.to_string()).or_else(|| q.passkey.clone());
        let authed_uuid = verified_actor_uuid(&reduced, q.actor.as_deref(), passkey.as_deref());
        let specs = parse_parent_specs(q.parent.as_ref());
        let raw_pool = if specs.is_empty() {
            reduced.ranking_group.idx_to_item.clone()
        } else {
            crate::scope_rank::resolve_scope(&reduced, &specs)
        };
        raw_pool.into_iter().filter(|it| match crate::events::path_owner_uuid(it) {
            None => true,
            Some(owner) => authed_uuid.as_deref() == Some(owner),
        }).collect()
    };

    if pool.len() < 2 {
        let scope = q.parent.as_deref().unwrap_or("(all)");
        return api_error(
            axum::http::StatusCode::BAD_REQUEST,
            format!("need at least 2 items under parent /{}", scope),
            Some("add items via ingest".to_string()),
        );
    }

    if force_random {
        let Some((left, right)) = pick_random_distinct(&pool) else {
            return api_error(axum::http::StatusCode::BAD_REQUEST, "need at least 2 items", None);
        };
        let (left_body, right_body, threads, connectivity) = {
            let reduced = reduced_arc.read().await;
            let lb = reduced.item_bodies.get(&left).cloned();
            let rb = reduced.item_bodies.get(&right).cloned();
            let th: Vec<String> = reduced
                .item_threads
                .get(&left)
                .into_iter()
                .chain(reduced.item_threads.get(&right))
                .flat_map(|s| s.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            let cs = compute_connectivity_stats(&reduced, &pool);
            (lb, rb, th, cs)
        };
        return Json(PairResponse {
            left: item_path_for_api(&left),
            right: item_path_for_api(&right),
            left_body,
            right_body,
            threads,
            connectivity: Some(connectivity),
        })
        .into_response();
    }

    let selected: Option<(String, String)> = {
        let mut reduced = reduced_arc.write().await;
        let group = &mut reduced.ranking_group;
        if group.idx_to_item.is_empty() {
            pick_random_distinct(&pool)
        } else {
                let mut rng = rand::thread_rng();
                let idxs: Vec<usize> = pool.iter()
                    .filter_map(|it| group.item_to_idx.get(it).copied())
                    .collect();
                let ranked = ranked_items_subset(group, &idxs, 10000, 1e-8);
                let ranked_set: std::collections::HashSet<String> =
                    ranked.iter().map(|r| r.item.clone()).collect();
                let unsorted: Vec<String> = pool.iter()
                    .filter(|it| !ranked_set.contains(*it))
                    .cloned()
                    .collect();

                let mut pick: Option<(String, String)> = None;
                if !unsorted.is_empty() {
                    if let Some(left) = unsorted.choose(&mut rng).cloned() {
                        let mut candidates: Vec<String> = if !ranked.is_empty() {
                            ranked.iter().map(|r| r.item.clone()).collect()
                        } else {
                            pool.clone()
                        };
                        candidates.retain(|c| c != &left);
                        if let Some(right) = candidates.choose(&mut rng).cloned() {
                            pick = Some((left, right));
                        }
                    }
                } else if ranked.len() >= 2 {
                    for i in 0..(ranked.len().saturating_sub(1)) {
                        let a = &ranked[i].item;
                        let b = &ranked[i + 1].item;
                        if a != b && !is_pair_voted(group, a, b) {
                            pick = Some((a.clone(), b.clone()));
                            break;
                        }
                    }
                    if pick.is_none() {
                        for _ in 0..64 {
                            let (Some(a), Some(b)) = (pool.choose(&mut rng).cloned(), pool.choose(&mut rng).cloned()) else { break; };
                            if a != b && !is_pair_voted(group, &a, &b) {
                                pick = Some((a, b));
                                break;
                            }
                        }
                    }
                }
                pick.or_else(|| pick_random_distinct(&pool))
        }
    };

    let Some((left, right)) = selected else {
        return api_error(axum::http::StatusCode::BAD_REQUEST, "need at least 2 items", None);
    };

    let (left_body, right_body, threads, connectivity) = {
        let reduced = reduced_arc.read().await;
        let lb = reduced.item_bodies.get(&left).cloned();
        let rb = reduced.item_bodies.get(&right).cloned();
        let th: Vec<String> = reduced
            .item_threads
            .get(&left)
            .into_iter()
            .chain(reduced.item_threads.get(&right))
            .flat_map(|s| s.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let cs = compute_connectivity_stats(&reduced, &pool);
        (lb, rb, th, cs)
    };
    Json(PairResponse {
        left: item_path_for_api(&left),
        right: item_path_for_api(&right),
        left_body,
        right_body,
        threads,
        connectivity: Some(connectivity),
    })
    .into_response()
}
