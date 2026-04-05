use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    api::helpers::{api_error, now_ms},
    events::{Event, GrantAdded, ThreadCapability, ThreadCreated, ThreadVisibility},
    identity::parse_username,
    state::AppState,
};
use super::auth::verify_bearer_principal;

fn gen_short_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..7).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

#[derive(Debug, Deserialize)]
pub struct CreateThreadRequest {
    pub slug: String,
    pub visibility: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateThreadResponse {
    pub ok: bool,
    pub thread_id: String,
}

pub async fn post_create_thread(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateThreadRequest>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let principal = match verify_bearer_principal(&headers, &reduced) {
        Ok(u) => u,
        Err((st, msg)) => return api_error(st, msg, None).into_response(),
    };

    let slug = req.slug.trim().to_lowercase();
    if slug.is_empty() || slug.len() > 64 {
        return api_error(StatusCode::BAD_REQUEST, "slug must be 1-64 characters", None).into_response();
    }
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return api_error(StatusCode::BAD_REQUEST, "slug must be lowercase alphanumeric with hyphens", None).into_response();
    }

    let visibility = match req.visibility.as_deref().unwrap_or("private") {
        "private" => ThreadVisibility::Private,
        "public"  => ThreadVisibility::Public,
        other     => return api_error(StatusCode::BAD_REQUEST, format!("unknown visibility: {other}"), None).into_response(),
    };

    // Generate a collision-free short id.
    let short_id = loop {
        let id = gen_short_id();
        // Threads are keyed by full "shortid/slug" in the map.
        if !reduced.threads.contains_key(&format!("{id}/{slug}")) {
            break id;
        }
    };
    let thread_id = format!("{short_id}/{slug}");

    drop(reduced);

    let ts = now_ms();

    let tc_ev = Event::ThreadCreated(ThreadCreated {
        ts,
        thread_id: thread_id.clone(),
        slug: slug.clone(),
        owner: principal.clone(),
        visibility,
    });

    // Owner gets all capabilities.
    let ga_ev = Event::GrantAdded(GrantAdded {
        ts,
        thread_id: thread_id.clone(),
        username: principal.clone(),
        capabilities: vec![
            ThreadCapability::View,
            ThreadCapability::Post,
            ThreadCapability::Vote,
            ThreadCapability::AddItem,
            ThreadCapability::Manage,
        ],
        granted_by: principal.clone(),
    });

    if let Err(err) = state.event_log.append(&tc_ev).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None).into_response();
    }
    if let Err(err) = state.event_log.append(&ga_ev).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None).into_response();
    }

    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(tc_ev);
        reduced.apply_event(ga_ev);
    }

    Json(CreateThreadResponse { ok: true, thread_id }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct AddGrantRequest {
    pub username: String,
    pub capabilities: Vec<String>,
}

fn parse_capability(s: &str) -> Result<ThreadCapability, String> {
    match s {
        "view"     => Ok(ThreadCapability::View),
        "post"     => Ok(ThreadCapability::Post),
        "vote"     => Ok(ThreadCapability::Vote),
        "add_item" => Ok(ThreadCapability::AddItem),
        "manage"   => Ok(ThreadCapability::Manage),
        other      => Err(format!("unknown capability: {other}")),
    }
}

pub async fn post_thread_grants(
    Path((short_id, slug)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddGrantRequest>,
) -> impl IntoResponse {
    let thread_id = format!("{short_id}/{slug}");
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let principal = match verify_bearer_principal(&headers, &reduced) {
        Ok(u) => u,
        Err((st, msg)) => return api_error(st, msg, None).into_response(),
    };

    if !reduced.user_has_cap(&thread_id, &principal, ThreadCapability::Manage) {
        return api_error(StatusCode::FORBIDDEN, "requires Manage capability", None).into_response();
    }

    let target = match parse_username(&req.username) {
        Ok(u) => u,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid username", Some(msg)).into_response(),
    };
    if !reduced.users_by_provider.values().any(|u| u == &target) {
        return api_error(StatusCode::NOT_FOUND, format!("user @{target} not found"), None).into_response();
    }

    if req.capabilities.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "capabilities must not be empty", None).into_response();
    }
    let capabilities: Result<Vec<_>, _> = req.capabilities.iter().map(|s| parse_capability(s)).collect();
    let capabilities = match capabilities {
        Ok(c) => c,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, msg, None).into_response(),
    };

    drop(reduced);

    let ga_ev = Event::GrantAdded(GrantAdded {
        ts: now_ms(),
        thread_id,
        username: target,
        capabilities,
        granted_by: principal,
    });

    if let Err(err) = state.event_log.append(&ga_ev).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None).into_response();
    }

    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(ga_ev);
    }

    Json(serde_json::json!({"ok": true})).into_response()
}
