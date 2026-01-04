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
    dsl,
    events::{canonicalize_actor, canonicalize_aspect, canonicalize_item, canonicalize_tag, Event, VoteCast},
    ranking::ranked_items,
    reducer::GroupKey,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    /// Hashtag namespace (required).
    pub tag: String,
    pub aspect: String,
    pub a: String,
    pub b: String,
    /// Ratio string like "3:1" (preferred).
    #[serde(default)]
    pub ratio: Option<String>,
    /// Legacy scalar score in [-50, 50] (deprecated).
    #[serde(default)]
    pub score: Option<i32>,
    /// Optional self-declared actor (e.g. "@tommy").
    #[serde(default)]
    pub actor: Option<String>,
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

    let actor_c = req.actor.as_ref().map(|a| canonicalize_actor(a));
    let tag = canonicalize_tag(&req.tag);
    let aspect = canonicalize_aspect(&req.aspect);
    let a = canonicalize_item(&req.a);
    let b = canonicalize_item(&req.b);

    fn parse_ratio(s: &str) -> Option<(i32, i32)> {
        let t = s.trim();
        let (l, r) = t.split_once(':')?;
        let left: i32 = l.trim().parse().ok()?;
        let right: i32 = r.trim().parse().ok()?;
        Some((left, right))
    }

    let (ratio_left, ratio_right, score) = match (req.ratio.as_deref(), req.score) {
        (Some(r), _) => {
            let Some((l, rr)) = parse_ratio(r) else {
                return (
                    StatusCode::BAD_REQUEST,
                    "invalid ratio (expected like 3:1)".to_string(),
                )
                    .into_response();
            };
            (l.max(0), rr.max(0), None)
        }
        (None, Some(s)) => (0, 0, Some(s.clamp(-50, 50))),
        (None, None) => return (StatusCode::BAD_REQUEST, "missing ratio".to_string()).into_response(),
    };

    let vote = VoteCast {
        ts: now_ms(),
        tag: tag.clone(),
        aspect: aspect.clone(),
        a: a.clone(),
        b: b.clone(),
        ratio_left,
        ratio_right,
        score,
        body: None,
        voter_key_id,
        actor: actor_c.clone(),
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
                "npx slugsocial vote /{} 2:1 /{} --tag #{} --aspect :{}{}",
                a,
                b,
                tag,
                aspect,
                actor_c
                    .as_ref()
                    .map(|ac| format!(" --as @{}", ac))
                    .unwrap_or_default()
            ),
            rank: format!(
                "npx slugsocial rank #{} --aspect :{}{}",
                tag,
                aspect,
                actor_c
                    .as_ref()
                    .map(|ac| format!(" --as @{}", ac))
                    .unwrap_or_default()
            ),
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

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    /// Raw text containing DSL (and optionally prose).
    pub text: String,
    /// Parsing mode: "full" (default), "lines", or "dsl".
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub tags: Vec<String>,
    pub events_appended: usize,
    pub next: NextMoves,
}

/// Ingest a DSL/prose document, emitting events into the JSONL log.
///
/// Interpretation model:
/// - `#tag` sets the active tag context
/// - `:aspect` sets the active aspect context (default: "default")
/// - `/item {body}` emits `ItemUpsert` (and `TagAdd` if tag context exists)
/// - `/a 2:1 /b {explanation}` emits `VoteCast` (and `TagAdd` for both items if tag context exists)
pub async fn post_ingest(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let AuthedKey { id: voter_key_id } = match require_key(State(state.clone()), headers).await {
        Ok(k) => k,
        Err(e) => return (e.0, e.1).into_response(),
    };

    let mode = req
        .mode
        .clone()
        .unwrap_or_else(|| "full".to_string())
        .to_lowercase();

    let doc = match mode.as_str() {
        "full" => Ok(dsl::parse_full(&req.text)),
        "lines" => dsl::parse_lines(&req.text).map_err(|e| e.to_string()),
        "dsl" => dsl::parse(&req.text).map_err(|e| e.to_string()),
        _ => Err(format!("invalid mode: {}", mode)),
    };

    let doc = match doc {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let mut current_actor: Option<String> = None;
    let mut current_tag: Option<String> = None;
    let mut current_aspect: String = "default".to_string();
    let ts = now_ms();

    let mut events: Vec<Event> = Vec::new();
    let mut tags_seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for s in doc.statements {
        match s {
            dsl::Stmt::Prose { .. } => {}
            dsl::Stmt::Email { .. } => {}
            dsl::Stmt::Actor { name } => {
                let a = canonicalize_actor(&name);
                current_actor = Some(a.clone());
            }
            dsl::Stmt::Hashtag { name } => {
                let t = canonicalize_tag(&name);
                current_tag = Some(t.clone());
                tags_seen.insert(t);
            }
            dsl::Stmt::Attribute { name } => {
                current_aspect = canonicalize_aspect(&name);
            }
            dsl::Stmt::Item { title, body } => {
                let item = canonicalize_item(&title);
                events.push(Event::ItemUpsert(crate::events::ItemUpsert {
                    ts,
                    item: item.clone(),
                    body,
                }));
                if let Some(tag) = current_tag.clone() {
                    events.push(Event::TagAdd(crate::events::TagAdd {
                        ts,
                        tag: tag.clone(),
                        item: item.clone(),
                    }));
                }
            }
            dsl::Stmt::Vote {
                item1,
                item2,
                ratio_left,
                ratio_right,
                explanation,
            } => {
                let Some(tag) = current_tag.clone() else {
                    return (
                        StatusCode::BAD_REQUEST,
                        "vote requires an active #tag context".to_string(),
                    )
                        .into_response();
                };
                let aspect = current_aspect.clone();
                let a = canonicalize_item(&item1);
                let b = canonicalize_item(&item2);
                events.push(Event::VoteCast(VoteCast {
                    ts,
                    tag: tag.clone(),
                    aspect,
                    a: a.clone(),
                    b: b.clone(),
                    ratio_left,
                    ratio_right,
                    score: None,
                    body: explanation,
                    voter_key_id: voter_key_id.clone(),
                    actor: current_actor.clone(),
                }));
                // Ensure both items exist under the tag (helps /pair pool).
                events.push(Event::TagAdd(crate::events::TagAdd {
                    ts,
                    tag: tag.clone(),
                    item: a.clone(),
                }));
                events.push(Event::TagAdd(crate::events::TagAdd {
                    ts,
                    tag,
                    item: b.clone(),
                }));
            }
        }
    }

    let tags_vec: Vec<String> = tags_seen.into_iter().collect();
    events.push(Event::DslIngested(crate::events::DslIngested {
        ts,
        raw: req.text.clone(),
        tags: tags_vec.clone(),
        voter_key_id: voter_key_id.clone(),
        actor: current_actor.clone(),
    }));

    let events_appended = events.len();

    // Persist then reduce.
    for ev in &events {
        if let Err(err) = state.event_log.append(ev).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}")).into_response();
        }
    }
    {
        let mut reduced = state.reduced.write().await;
        for ev in events.into_iter() {
            reduced.apply_event(ev);
        }
    }

    let primary_tag = tags_vec
        .get(0)
        .cloned()
        .unwrap_or_else(|| "untagged".to_string());
    Json(IngestResponse {
        ok: true,
        tags: tags_vec.iter().map(|t| format!("#{t}")).collect(),
        events_appended,
        next: NextMoves {
            vote: format!(
                "npx slugsocial pair #{} --aspect :{}{}",
                primary_tag,
                current_aspect,
                current_actor
                    .as_ref()
                    .map(|a| format!(" --as @{}", a))
                    .unwrap_or_default()
            ),
            rank: format!(
                "npx slugsocial rank #{} --aspect :{}{}",
                primary_tag,
                current_aspect,
                current_actor
                    .as_ref()
                    .map(|a| format!(" --as @{}", a))
                    .unwrap_or_default()
            ),
            web: format!("https://slug.social/t/{}", primary_tag),
        },
    })
    .into_response()
}


