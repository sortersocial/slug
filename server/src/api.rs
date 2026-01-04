use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
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
    (t.as_millis() as i64)
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


