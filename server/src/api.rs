use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::{
    auth::{require_key, AuthedKey},
    events::{canonicalize_aspect, canonicalize_item, canonicalize_tag, Event, VoteCast},
    ranking::ranked_items,
    reducer::GroupKey,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    pub tag: String,
    pub aspect: String,
    pub a: String,
    pub b: String,
    pub score: i32,
}

#[derive(Debug, Serialize)]
pub struct VoteResponse {
    pub ok: bool,
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
    pub next: NextMoves,
}

#[derive(Debug, Serialize)]
pub struct RankRow {
    pub item: String,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct NextMoves {
    pub vote: String,
    pub rank: String,
    pub web: String,
}

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

pub async fn post_vote(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<VoteRequest>,
) -> impl IntoResponse {
    let AuthedKey { id: voter_key_id } = match require_key(State(state.clone()), headers).await {
        Ok(k) => k,
        Err(e) => return (e.0, e.1).into_response(),
    };

    let tag = canonicalize_tag(&req.tag);
    let aspect = canonicalize_aspect(&req.aspect);
    let a = canonicalize_item(&req.a);
    let b = canonicalize_item(&req.b);
    let score = req.score.clamp(-50, 50);

    let vote = VoteCast {
        ts: now_ms(),
        tag: tag.clone(),
        aspect: aspect.clone(),
        a: a.clone(),
        b: b.clone(),
        score,
        voter_key_id,
    };

    let event = Event::VoteCast(vote.clone());
    if let Err(err) = state.event_log.append(&event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}")).into_response();
    }

    {
        let mut reduced = state.reduced.write().await;
        reduced.apply_event(event);
    }

    // Return ranking for this group.
    let ranking = {
        let mut reduced = state.reduced.write().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let group = reduced.groups.get_mut(&key);
        if let Some(g) = group {
            ranked_items(g, 10000, 1e-8)
                .into_iter()
                .take(25)
                .map(|r| RankRow {
                    item: format!("/{}", r.item),
                    score: r.score,
                })
                .collect()
        } else {
            vec![]
        }
    };

    let resp = VoteResponse {
        ok: true,
        tag: format!("#{tag}"),
        aspect: format!(":{aspect}"),
        ranking,
        next: NextMoves {
            vote: format!(
                "npx slugsocial vote /{} 2:1 /{} --tag #{} --aspect :{}",
                a, b, tag, aspect
            ),
            rank: format!("npx slugsocial rank #{} --aspect :{}", tag, aspect),
            web: format!("https://slug.social/t/{}/a/{}", tag, aspect),
        },
    };

    Json(resp).into_response()
}

#[derive(Debug, Deserialize)]
pub struct RankQuery {
    pub tag: String,
    pub aspect: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct RankResponse {
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

pub async fn get_rank(State(state): State<AppState>, Query(q): Query<RankQuery>) -> impl IntoResponse {
    let tag = canonicalize_tag(&q.tag);
    let aspect = canonicalize_aspect(&q.aspect);
    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let ranking = {
        let mut reduced = state.reduced.write().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };
        let group = reduced.groups.get_mut(&key);
        if let Some(g) = group {
            ranked_items(g, 10000, 1e-8)
                .into_iter()
                .take(limit)
                .map(|r| RankRow {
                    item: format!("/{}", r.item),
                    score: r.score,
                })
                .collect()
        } else {
            vec![]
        }
    };

    Json(RankResponse {
        tag: format!("#{tag}"),
        aspect: format!(":{aspect}"),
        ranking,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PairQuery {
    pub tag: String,
    pub aspect: String,
    /// If true, ignore ranking and select a random pair (useful for “skip”).
    #[serde(default)]
    pub random: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PairResponse {
    pub tag: String,
    pub aspect: String,
    pub left: String,
    pub right: String,
}

fn pick_random_distinct(items: &[String]) -> Option<(String, String)> {
    if items.len() < 2 {
        return None;
    }
    let mut rng = rand::thread_rng();
    let left = items.choose(&mut rng)?.clone();
    // Retry a few times to avoid pathological “all same” (shouldn’t happen, but cheap).
    for _ in 0..8 {
        let right = items.choose(&mut rng)?.clone();
        if right != left {
            return Some((left, right));
        }
    }
    // Deterministic fallback.
    let mut right = items[0].clone();
    if right == left {
        right = items[1].clone();
    }
    Some((left, right))
}

fn is_pair_voted(group: &crate::reducer::GroupState, a: &str, b: &str) -> bool {
    let Some(&a_idx) = group.item_to_idx.get(a) else {
        return false;
    };
    let Some(&b_idx) = group.item_to_idx.get(b) else {
        return false;
    };
    let (i, j) = if a_idx < b_idx { (a_idx, b_idx) } else { (b_idx, a_idx) };
    group.voted_pairs.contains(&(i, j))
}

pub async fn get_pair(State(state): State<AppState>, Query(q): Query<PairQuery>) -> impl IntoResponse {
    let tag = canonicalize_tag(&q.tag);
    let aspect = canonicalize_aspect(&q.aspect);
    let force_random = q.random.unwrap_or(false);

    // Pool is “all items under tag”, independent of aspect votes.
    //
    // Note: today many tags are “implicit” (created by votes) and won't have a
    // `TagAdd` event yet, so we fall back to the group's known items.
    let pool: Vec<String> = {
        let reduced = state.reduced.read().await;
        if let Some(s) = reduced.tags.get(&tag) {
            s.iter().cloned().collect()
        } else {
            let key = GroupKey {
                tag: tag.clone(),
                aspect: aspect.clone(),
            };
            reduced
                .groups
                .get(&key)
                .map(|g| g.idx_to_item.clone())
                .unwrap_or_default()
        }
    };

    if pool.len() < 2 {
        return (
            StatusCode::BAD_REQUEST,
            format!("need at least 2 items under tag #{tag}"),
        )
            .into_response();
    }

    // Random mode: ignore ranking and pick any distinct pair from pool.
    if force_random {
        let Some((left, right)) = pick_random_distinct(&pool) else {
            return (
                StatusCode::BAD_REQUEST,
                format!("need at least 2 items under tag #{tag}"),
            )
                .into_response();
        };
        return Json(PairResponse {
            tag: format!("#{tag}"),
            aspect: format!(":{aspect}"),
            left: format!("/{}", left),
            right: format!("/{}", right),
        })
        .into_response();
    }

    // Rank-aware selection (when a group exists for this tag+aspect).
    let selected: Option<(String, String)> = {
        let mut reduced = state.reduced.write().await;
        let key = GroupKey {
            tag: tag.clone(),
            aspect: aspect.clone(),
        };

        match reduced.groups.get_mut(&key) {
            // If no votes yet for this aspect, just pick random from the tag pool.
            None => pick_random_distinct(&pool),
            Some(group) => {
                let mut rng = rand::thread_rng();

                // Compute ranked items (sorted high->low). Group contains only items that have appeared in votes.
                let ranked = ranked_items(group, 10000, 1e-8);
                let ranked_set: std::collections::HashSet<String> =
                    ranked.iter().map(|r| r.item.clone()).collect();

                let unsorted: Vec<String> = pool
                    .iter()
                    .filter(|it| !ranked_set.contains(*it))
                    .cloned()
                    .collect();

                // Strategy 1: introduce unsorted items by pairing with a sorted item (or any other item).
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
                    // Strategy 2: all items are “sorted”; prefer adjacent pairs not yet voted on.
                    for i in 0..(ranked.len().saturating_sub(1)) {
                        let a = &ranked[i].item;
                        let b = &ranked[i + 1].item;
                        if a != b && !is_pair_voted(group, a, b) {
                            pick = Some((a.clone(), b.clone()));
                            break;
                        }
                    }

                    // Otherwise pick any unvoted pair (bounded attempt).
                    if pick.is_none() {
                        for _ in 0..64 {
                            let (Some(a), Some(b)) =
                                (pool.choose(&mut rng).cloned(), pool.choose(&mut rng).cloned())
                            else {
                                break;
                            };
                            if a != b && !is_pair_voted(group, &a, &b) {
                                pick = Some((a, b));
                                break;
                            }
                        }
                    }
                }

                // Saturated/degenerate or selection failure: allow re-voting.
                pick.or_else(|| pick_random_distinct(&pool))
            }
        }
    };

    let Some((left, right)) = selected else {
        return (
            StatusCode::BAD_REQUEST,
            format!("need at least 2 items under tag #{tag}"),
        )
            .into_response();
    };

    Json(PairResponse {
        tag: format!("#{tag}"),
        aspect: format!(":{aspect}"),
        left: format!("/{}", left),
        right: format!("/{}", right),
    })
    .into_response()
}


