use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    Form, Json,
};
use base64::Engine;
use serde::Deserialize;
use slug_types::{PendingSessionPollResponse, PendingSessionStartRequest, PendingSessionStartResponse, WhoamiResponse};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

use crate::{
    api::helpers::{api_error, now_ms, sha256_hex},
    events::{
        canonicalize_agent, canonicalize_username, validate_agent_format, validate_username,
        Event, TokenIssued, UserRegistered,
    },
    state::{AppState, PendingSession},
};

fn pending_sessions(state: &AppState) -> Arc<RwLock<HashMap<String, PendingSession>>> {
    state.pending_sessions.clone()
}

fn parse_bearer(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err((StatusCode::UNAUTHORIZED, "missing Authorization header".to_string()));
    };
    let Ok(s) = value.to_str() else {
        return Err((StatusCode::UNAUTHORIZED, "invalid Authorization header".to_string()));
    };
    let s = s.trim();
    let Some(rest) = s.strip_prefix("Bearer ") else {
        return Err((StatusCode::UNAUTHORIZED, "Authorization must be Bearer".to_string()));
    };
    Ok(rest.trim().to_string())
}

/// Resolve canonical username from `Authorization: Bearer slug_…` using durable token state.
pub fn verify_bearer_principal(
    headers: &axum::http::HeaderMap,
    reduced: &crate::reducer::ReducerState,
) -> Result<String, (StatusCode, String)> {
    let bearer = parse_bearer(headers)?;
    verify_token(reduced, &bearer)
}

fn verify_token(reduced: &crate::reducer::ReducerState, bearer: &str) -> Result<String, (StatusCode, String)> {
    // slug_<token_id>_<secret>
    let Some(rest) = bearer.strip_prefix("slug_") else {
        return Err((StatusCode::UNAUTHORIZED, "invalid token format".to_string()));
    };
    let mut parts = rest.splitn(2, '_');
    let token_id = parts.next().unwrap_or_default();
    let secret = parts.next().unwrap_or_default();
    if token_id.is_empty() || secret.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "invalid token format".to_string()));
    }
    let Some((username, salt, token_hash)) = reduced.tokens_by_id.get(token_id).cloned() else {
        return Err((StatusCode::UNAUTHORIZED, "unknown token".to_string()));
    };
    let check = sha256_hex(&format!("{salt}:{secret}"));
    if check != token_hash {
        return Err((StatusCode::UNAUTHORIZED, "invalid token".to_string()));
    }
    Ok(username)
}

fn issue_token_for_user(username: &str) -> (String, TokenIssued, String) {
    // Returns: (bearer, event, canonical_username)
    let canonical_user = canonicalize_username(username);
    let token_id = {
        let mut id = String::new();
        let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut rng = rand::thread_rng();
        for _ in 0..7 {
            id.push(alphabet[rand::Rng::gen_range(&mut rng, 0..alphabet.len())] as char);
        }
        id
    };
    let secret = {
        let mut bytes = [0u8; 24];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    };
    let salt = {
        let mut bytes = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    };
    let token_hash = sha256_hex(&format!("{salt}:{secret}"));
    let bearer = format!("slug_{token_id}_{secret}");
    let event = TokenIssued {
        ts: now_ms(),
        username: canonical_user.clone(),
        token_id,
        token_hash,
        salt,
        issued_via: "oauth".to_string(),
    };
    (bearer, event, canonical_user)
}

#[derive(Debug, Deserialize)]
pub struct AuthLoginQuery {
    pub session: String,
}

pub async fn get_auth_login(Query(q): Query<AuthLoginQuery>, State(state): State<AppState>) -> impl IntoResponse {
    // Redirect to Google auth endpoint.
    let sessions = pending_sessions(&state);
    let sessions_read = sessions.read().await;
    let Some(_s) = sessions_read.get(&q.session) else {
        return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
    };
    drop(sessions_read);

    let google_base = std::env::var("SLUG_GOOGLE_BASE_URL").unwrap_or_else(|_| "https://accounts.google.com".to_string());
    let client_id = std::env::var("SLUG_GOOGLE_CLIENT_ID").unwrap_or_else(|_| "dev".to_string());
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let redirect_uri = format!("{public_url}/auth/callback");
    let auth_url = format!(
        "{google_base}/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope=openid%20email&state={}",
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&q.session),
    );
    Redirect::temporary(&auth_url).into_response()
}

#[derive(Debug, Deserialize)]
pub struct AuthCallbackQuery {
    pub code: String,
    pub state: String,
}

pub async fn get_auth_callback(Query(q): Query<AuthCallbackQuery>, State(state): State<AppState>) -> impl IntoResponse {
    let sessions = pending_sessions(&state);
    {
        let sessions_read = sessions.read().await;
        if !sessions_read.contains_key(&q.state) {
            return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
        }
    }

    let google_base = std::env::var("SLUG_GOOGLE_BASE_URL").unwrap_or_else(|_| "https://oauth2.googleapis.com".to_string());
    let client_id = std::env::var("SLUG_GOOGLE_CLIENT_ID").unwrap_or_else(|_| "dev".to_string());
    let client_secret = std::env::var("SLUG_GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| "dev".to_string());
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let redirect_uri = format!("{public_url}/auth/callback");

    // Exchange code for access token.
    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
    }
    let token_url = format!("{google_base}/token");
    let client = reqwest::Client::new();
    let tr: TokenResp = match client
        .post(token_url)
        .form(&[
            ("code", q.code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
    {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(err) => return api_error(StatusCode::BAD_GATEWAY, "oauth token exchange failed", Some(format!("{err}"))).into_response(),
        },
        Err(err) => return api_error(StatusCode::BAD_GATEWAY, "oauth token exchange failed", Some(format!("{err}"))).into_response(),
    };

    // Fetch userinfo.
    #[derive(Deserialize)]
    struct UserInfoResp {
        sub: String,
    }
    let userinfo_url = format!("{google_base}/userinfo");
    let ui: UserInfoResp = match client
        .get(userinfo_url)
        .bearer_auth(tr.access_token)
        .send()
        .await
    {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(err) => return api_error(StatusCode::BAD_GATEWAY, "oauth userinfo failed", Some(format!("{err}"))).into_response(),
        },
        Err(err) => return api_error(StatusCode::BAD_GATEWAY, "oauth userinfo failed", Some(format!("{err}"))).into_response(),
    };

    // If user exists, issue token and complete session. Otherwise redirect to choose-username.
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let provider_key = ("google".to_string(), ui.sub.clone());
    let existing = reduced.users_by_provider.get(&provider_key).cloned();
    drop(reduced);

    {
        let mut sessions_write = sessions.write().await;
        let s = sessions_write.get_mut(&q.state).expect("session checked above");
        s.provider = Some("google".to_string());
        s.provider_id = Some(ui.sub.clone());
        if let Some(username) = existing {
            let (bearer, token_event, canon_user) = issue_token_for_user(&username);
            // append token event
            let ev = Event::TokenIssued(token_event);
            if let Err(err) = state.event_log.append(&ev).await {
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to persist token", Some(format!("{err}"))).into_response();
            }
            {
                let mut reduced = reduced_arc.write().await;
                reduced.apply_event(ev);
            }
            s.complete = Some((canon_user, bearer));
            return Redirect::temporary(&format!("{public_url}/auth/complete")).into_response();
        }
    }

    Redirect::temporary(&format!("{public_url}/auth/choose-username?session={}", q.state)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ChooseUsernameQuery {
    pub session: String,
}

pub async fn get_choose_username(Query(q): Query<ChooseUsernameQuery>, State(state): State<AppState>) -> impl IntoResponse {
    let sessions = pending_sessions(&state);
    let sessions_read = sessions.read().await;
    if !sessions_read.contains_key(&q.session) {
        return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
    }
    // Keep HTML minimal for now.
    (
        StatusCode::OK,
        format!(
            "choose username\n\nPOST /auth/choose-username with form fields: session, username\nsession={}",
            q.session
        ),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ChooseUsernameForm {
    pub session: String,
    pub username: String,
}

pub async fn post_choose_username(
    State(state): State<AppState>,
    Form(form): Form<ChooseUsernameForm>,
) -> impl IntoResponse {
    if let Err(msg) = validate_username(&form.username) {
        return api_error(StatusCode::BAD_REQUEST, "invalid username", Some(msg)).into_response();
    }

    let sessions = pending_sessions(&state);
    let (provider, provider_id, agent) = {
        let sessions_read = sessions.read().await;
        let Some(s) = sessions_read.get(&form.session) else {
            return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
        };
        let Some(provider) = s.provider.clone() else {
            return api_error(StatusCode::BAD_REQUEST, "oauth not completed", None).into_response();
        };
        let Some(provider_id) = s.provider_id.clone() else {
            return api_error(StatusCode::BAD_REQUEST, "oauth not completed", None).into_response();
        };
        (provider, provider_id, s.agent.clone())
    };

    if let Err(msg) = validate_agent_format(&agent) {
        return api_error(StatusCode::BAD_REQUEST, "invalid agent format", Some(msg)).into_response();
    }

    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let provider_key = (provider.to_lowercase(), provider_id.clone());
    if reduced.users_by_provider.contains_key(&provider_key) {
        return api_error(StatusCode::CONFLICT, "provider already registered", None).into_response();
    }
    if reduced.users_by_provider.values().any(|u| u == &canonicalize_username(&form.username)) {
        return api_error(StatusCode::CONFLICT, "username not available", None).into_response();
    }
    drop(reduced);

    let ur = Event::UserRegistered(UserRegistered {
        ts: now_ms(),
        username: canonicalize_username(&form.username),
        provider: provider.to_lowercase(),
        provider_id: provider_id.clone(),
    });

    let (bearer, ti, canon_user) = issue_token_for_user(&form.username);
    let ti_ev = Event::TokenIssued(ti);

    // Persist events.
    if let Err(err) = state.event_log.append(&ur).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to persist user", Some(format!("{err}"))).into_response();
    }
    if let Err(err) = state.event_log.append(&ti_ev).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to persist token", Some(format!("{err}"))).into_response();
    }

    // Apply to reducer.
    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(ur.clone());
        reduced.apply_event(ti_ev.clone());
    }

    // Mark complete for polling.
    {
        let mut sessions_write = sessions.write().await;
        let s = sessions_write.get_mut(&form.session).expect("session checked above");
        s.complete = Some((canon_user.clone(), bearer.clone()));
    }

    // Binding is durable on first successful write, not during login.
    // Keep the page simple.
    (
        StatusCode::OK,
        format!("ok\nuser=@{canon_user}\nagent={}\n", canonicalize_agent(&agent)),
    )
        .into_response()
}

pub async fn post_pending_session(
    State(state): State<AppState>,
    Json(req): Json<PendingSessionStartRequest>,
) -> impl IntoResponse {
    if let Err(msg) = validate_agent_format(&req.agent) {
        return api_error(StatusCode::BAD_REQUEST, "invalid agent format", Some(msg)).into_response();
    }
    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let login_url = format!("{public_url}/auth/login?session={}", urlencoding::encode(&session));
    let poll_url = format!("/api/v0/pending-session/{}", session);
    let s = PendingSession {
        agent: req.agent.clone(),
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        complete: None,
    };
    let sessions = pending_sessions(&state);
    sessions.write().await.insert(session.clone(), s);
    Json(PendingSessionStartResponse {
        session,
        login_url,
        poll_url,
    })
    .into_response()
}

pub async fn get_pending_session(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let sessions = pending_sessions(&state);
    let sessions_read = sessions.read().await;
    let Some(s) = sessions_read.get(&id) else {
        return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
    };
    let (complete, user, token) = match &s.complete {
        Some((u, t)) => (true, Some(format!("@{}", u)), Some(t.clone())),
        None => (false, None, None),
    };
    Json(PendingSessionPollResponse {
        ok: true,
        complete,
        agent: s.agent.clone(),
        user,
        token,
    })
    .into_response()
}

pub async fn get_auth_complete() -> impl IntoResponse {
    (
        StatusCode::OK,
        "login complete — you can close this tab and return to the CLI\n",
    )
        .into_response()
}

pub async fn get_whoami(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let bearer = match parse_bearer(&headers) {
        Ok(v) => v,
        Err((st, msg)) => return api_error(st, msg, None).into_response(),
    };
    let reduced = state.reduced.read().await;
    let username = match verify_token(&reduced, &bearer) {
        Ok(u) => u,
        Err((st, msg)) => return api_error(st, msg, None).into_response(),
    };
    let agents_bound = reduced.agent_bindings.values().filter(|u| *u == &username).count();
    Json(WhoamiResponse {
        user: format!("@{}", username),
        agents_bound,
    })
    .into_response()
}

