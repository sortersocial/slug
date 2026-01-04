use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::state::{AppState, RateWindow};

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

#[derive(Debug, Clone)]
pub struct AuthedKey {
    pub id: String,
}

pub async fn require_key(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<AuthedKey, (StatusCode, String)> {
    let provided = headers
        .get("x-slug-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, "missing x-slug-key".to_string()))?;

    let key = state
        .cfg
        .keys
        .iter()
        .find(|k| k.secret == provided)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, "invalid key".to_string()))?;

    // Naive fixed-window rate limit per key id.
    let limit = state.cfg.rate_limit_per_minute.max(1);
    let now = now_ms();
    let window_ms = 60_000i64;

    {
        let mut rate = state.rate.write().await;
        let entry = rate.entry(key.id.clone()).or_insert(RateWindow {
            window_start_ms: now,
            count: 0,
        });
        if now - entry.window_start_ms >= window_ms {
            entry.window_start_ms = now;
            entry.count = 0;
        }
        entry.count += 1;
        if entry.count > limit {
            return Err((StatusCode::TOO_MANY_REQUESTS, "rate limited".to_string()));
        }
    }

    Ok(AuthedKey { id: key.id.clone() })
}

pub fn auth_error_response(err: (StatusCode, String)) -> impl IntoResponse {
    err
}


