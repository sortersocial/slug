use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use axum_extra::extract::cookie::CookieJar;
use base64::Engine;
use serde::Deserialize;
use slug_types::{PendingSessionPollResponse, PendingSessionStartRequest, PendingSessionStartResponse, WhoamiResponse};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

use crate::{
    api::helpers::{api_error, now_ms, sha256_hex},
    events::{Event, GrantAdded, TokenIssued, UserRegistered},
    html::{
        auth_complete_page, auth_signed_in_fragment, choose_username_error_fragment,
        choose_username_page, JsBuilder,
    },
    identity::{parse_agent, parse_username},
    reducer::ReducerState,
    state::{AppState, PendingSession},
};

/// Delegate id for browser users who land via `/join/inv_…` (no CLI agent).
const INVITE_BROWSER_AGENT: &str = "00000000-0000-0000-0000-000000000000:invite:web/join";

/// Agent id for `/login` browser OAuth (no CLI); must pass [`parse_agent`].
const WEB_BROWSER_AGENT: &str = "00000000-0000-0000-0000-000000000001:social:web/browser";

/// HttpOnly cookie storing the same `slug_*` bearer string the CLI uses.
pub const SLUG_SESSION_COOKIE: &str = "slug_session";

/// `Set-Cookie` header value (full attribute string).
pub fn session_cookie_header_value(bearer: &str) -> HeaderValue {
    let s = format!(
        "{SLUG_SESSION_COOKIE}={bearer}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"
    );
    HeaderValue::from_str(&s).expect("session cookie value must be ASCII")
}

fn js_form_error_fragment(session: &str, error: &str) -> Response {
    JsBuilder::new()
        .id("choose-username-form")
        .morph_inner(choose_username_error_fragment(session, error))
        .into_response()
}

fn js_signed_in_fragment(bearer: &str) -> Response {
    let mut response = JsBuilder::new()
        .id("choose-username-form")
        .morph_inner(auth_signed_in_fragment())
        .redirect("/auth/complete")
        .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie_header_value(bearer));
    response
}

/// Resolve the signed-in username from `Authorization: Bearer` or `slug_session` cookie.
pub fn optional_principal(headers: &HeaderMap, jar: &CookieJar, reduced: &ReducerState) -> Option<String> {
    if let Ok(u) = verify_bearer_principal(headers, reduced) {
        return Some(u);
    }
    let c = jar.get(SLUG_SESSION_COOKIE)?;
    verify_token(reduced, c.value()).ok()
}

fn redirect_with_session_cookie(public_url: &str, path_and_query: &str, bearer: &str) -> Response {
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, format!("{public_url}{path_and_query}"))
        .header(header::SET_COOKIE, session_cookie_header_value(bearer))
        .body(Body::empty())
        .unwrap()
}

async fn apply_invite_redemption(state: &AppState, invite_token: &str, grantee_username: &str) -> Result<(), String> {
    let now = now_ms();
    let ga = {
        let mut invites = state.invites.write().await;
        let Some(inv) = invites.get_mut(invite_token) else {
            return Err("invite not found".into());
        };
        if now > inv.expires_at_ms {
            invites.remove(invite_token);
            return Err("invite expired".into());
        }
        if inv.current_uses >= inv.max_uses {
            return Err("invite exhausted".into());
        }
        inv.current_uses += 1;
        Event::GrantAdded(GrantAdded {
            ts: now,
            room_id: inv.room_id.clone(),
            username: grantee_username.to_string(),
            capabilities: inv.capabilities.clone(),
            granted_by: inv.inviter.clone(),
        })
    };

    match state.event_log.append(&ga).await {
        Ok(()) => {
            let mut reduced = state.reduced.write().await;
            reduced.apply_event(ga);
            let mut invites = state.invites.write().await;
            if let Some(inv) = invites.get(invite_token) {
                if inv.current_uses >= inv.max_uses {
                    invites.remove(invite_token);
                }
            }
            Ok(())
        }
        Err(e) => {
            let mut invites = state.invites.write().await;
            if let Some(inv) = invites.get_mut(invite_token) {
                inv.current_uses = inv.current_uses.saturating_sub(1);
            }
            Err(format!("{e}"))
        }
    }
}

fn pending_sessions(state: &AppState) -> Arc<RwLock<HashMap<String, PendingSession>>> {
    state.pending_sessions.clone()
}

/// Decode the `sub` claim from a JWT payload without verifying the signature.
/// Safe here because the token was received directly from Google's token endpoint over TLS.
fn extract_jwt_sub(jwt: &str) -> Option<String> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("sub")?.as_str().map(|s| s.to_string())
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

/// `stored_username` must already be in persisted shape (lowercase slug, no `@`).
fn issue_token_for_user(stored_username: &str) -> (String, TokenIssued) {
    let username = stored_username.to_string();
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
        username: username.clone(),
        token_id,
        token_hash,
        salt,
        issued_via: "oauth".to_string(),
    };
    (bearer, event)
}

#[derive(Debug, Deserialize)]
pub struct AuthLoginQuery {
    pub session: String,
}

pub async fn get_join_invite(Path(token): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let token = token.trim().to_string();
    if token.is_empty() {
        return api_error(StatusCode::NOT_FOUND, "invite invalid or expired", None).into_response();
    }
    let now = now_ms();
    let valid = {
        let invites = state.invites.read().await;
        match invites.get(&token) {
            None => false,
            Some(inv) => now <= inv.expires_at_ms && inv.current_uses < inv.max_uses,
        }
    };
    if !valid {
        return api_error(StatusCode::NOT_FOUND, "invite invalid or expired", None).into_response();
    }

    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let s = PendingSession {
        agent: INVITE_BROWSER_AGENT.to_string(),
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: Some(token),
        complete: None,
    };
    state.pending_sessions.write().await.insert(session.clone(), s);

    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    Redirect::temporary(&format!(
        "{public_url}/auth/login?session={}",
        urlencoding::encode(&session)
    ))
    .into_response()
}

pub async fn get_auth_login(Query(q): Query<AuthLoginQuery>, State(state): State<AppState>) -> impl IntoResponse {
    // Redirect to Google auth endpoint.
    let sessions = pending_sessions(&state);
    let sessions_read = sessions.read().await;
    let Some(_s) = sessions_read.get(&q.session) else {
        return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
    };
    drop(sessions_read);

    let auth_url_base = std::env::var("SLUG_GOOGLE_AUTH_URL")
        .unwrap_or_else(|_| "https://accounts.google.com/o/oauth2/v2/auth".to_string());
    let client_id = std::env::var("SLUG_GOOGLE_CLIENT_ID").unwrap_or_else(|_| "dev".to_string());
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let redirect_uri = format!("{public_url}/auth/callback");
    let auth_url = format!(
        "{auth_url_base}?client_id={}&redirect_uri={}&response_type=code&scope=openid%20email&state={}",
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

    let token_url = std::env::var("SLUG_GOOGLE_TOKEN_URL")
        .unwrap_or_else(|_| "https://oauth2.googleapis.com/token".to_string());
    let client_id = std::env::var("SLUG_GOOGLE_CLIENT_ID").unwrap_or_else(|_| "dev".to_string());
    let client_secret = std::env::var("SLUG_GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| "dev".to_string());
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let redirect_uri = format!("{public_url}/auth/callback");

    // Exchange code for id_token + access_token.
    #[derive(Deserialize)]
    struct TokenResp {
        id_token: String,
    }
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

    // Extract sub from the id_token JWT payload (base64-decode middle segment).
    // The token arrived directly from Google over TLS — no need for an extra userinfo roundtrip.
    let sub = match extract_jwt_sub(&tr.id_token) {
        Some(s) => s,
        None => return api_error(StatusCode::BAD_GATEWAY, "oauth: could not extract sub from id_token", None).into_response(),
    };

    // If user exists, issue token and complete session. Otherwise redirect to choose-username.
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let provider_key = ("google".to_string(), sub.clone());
    let existing = reduced.users_by_provider.get(&provider_key).cloned();
    drop(reduced);

    {
        let mut sessions_write = sessions.write().await;
        let s = sessions_write.get_mut(&q.state).expect("session checked above");
        s.provider = Some("google".to_string());
        s.provider_id = Some(sub.clone());
        if let Some(username) = existing {
            let invite_tok = s.redeem_invite.clone();
            let (bearer, token_event) = issue_token_for_user(&username);
            // append token event
            let ev = Event::TokenIssued(token_event);
            if let Err(err) = state.event_log.append(&ev).await {
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to persist token", Some(format!("{err}"))).into_response();
            }
            {
                let mut reduced = reduced_arc.write().await;
                reduced.apply_event(ev);
            }
            if let Some(tok) = invite_tok {
                if let Err(e) = apply_invite_redemption(&state, &tok, &username).await {
                    tracing::warn!(error = %e, "invite redemption skipped after oauth");
                }
            }
            let cookie_bearer = bearer.clone();
            s.complete = Some((username, bearer));
            return redirect_with_session_cookie(&public_url, "/", &cookie_bearer).into_response();
        }
    }

    Redirect::temporary(&format!("{public_url}/auth/choose-username?session={}", q.state)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ChooseUsernameQuery {
    pub session: String,
    pub error: Option<String>,
}

pub async fn get_choose_username(Query(q): Query<ChooseUsernameQuery>, State(state): State<AppState>) -> impl IntoResponse {
    let sessions = pending_sessions(&state);
    let sessions_read = sessions.read().await;
    if !sessions_read.contains_key(&q.session) {
        return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
    }
    drop(sessions_read);
    choose_username_page(&q.session, q.error.as_deref()).into_response()
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
    let canon_user = match parse_username(&form.username) {
        Ok(u) => u,
        Err(msg) => return js_form_error_fragment(&form.session, &format!("invalid username — {msg}")).into_response(),
    };

    let sessions = pending_sessions(&state);
    let (provider, provider_id, agent) = {
        let sessions_read = sessions.read().await;
        let Some(s) = sessions_read.get(&form.session) else {
            return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
        };
        let Some(provider) = s.provider.clone() else {
            return js_form_error_fragment(&form.session, "oauth not completed").into_response();
        };
        let Some(provider_id) = s.provider_id.clone() else {
            return js_form_error_fragment(&form.session, "oauth not completed").into_response();
        };
        (provider, provider_id, s.agent.clone())
    };

    if let Err(msg) = parse_agent(&agent) {
        return js_form_error_fragment(&form.session, &format!("invalid agent format — {msg}")).into_response();
    }

    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let provider_key = (provider.to_lowercase(), provider_id.clone());
    if reduced.users_by_provider.contains_key(&provider_key) {
        return js_form_error_fragment(&form.session, "provider already registered").into_response();
    }
    if reduced.users_by_provider.values().any(|u| u == &canon_user) {
        drop(reduced);
        return js_form_error_fragment(&form.session, "that username is taken — try another")
            .into_response();
    }
    drop(reduced);

    let ur = Event::UserRegistered(UserRegistered {
        ts: now_ms(),
        username: canon_user.clone(),
        provider: provider.to_lowercase(),
        provider_id: provider_id.clone(),
    });

    let (bearer, ti) = issue_token_for_user(&canon_user);
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

    // Redeem invite (if any) before marking the session complete.
    if let Some(tok) = {
        let sessions_read = sessions.read().await;
        sessions_read.get(&form.session).and_then(|s| s.redeem_invite.clone())
    } {
        if let Err(e) = apply_invite_redemption(&state, &tok, &canon_user).await {
            tracing::warn!(error = %e, "invite redemption skipped after registration");
        }
    }

    // Mark complete for polling.
    {
        let mut sessions_write = sessions.write().await;
        let s = sessions_write.get_mut(&form.session).expect("session checked above");
        s.complete = Some((canon_user.clone(), bearer.clone()));
    }

    js_signed_in_fragment(&bearer).into_response()
}

/// Start a browser-only OAuth flow (no CLI polling). Sets session cookie on success.
pub async fn get_web_login(State(state): State<AppState>) -> impl IntoResponse {
    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let s = PendingSession {
        agent: WEB_BROWSER_AGENT.to_string(),
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: None,
        complete: None,
    };
    state.pending_sessions.write().await.insert(session.clone(), s);
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    Redirect::temporary(&format!(
        "{public_url}/auth/login?session={}",
        urlencoding::encode(&session)
    ))
    .into_response()
}

pub async fn get_logout() -> impl IntoResponse {
    let clear = format!("{SLUG_SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, "/")
        .header(
            header::SET_COOKIE,
            HeaderValue::from_str(&clear).expect("static cookie clears"),
        )
        .body(Body::empty())
        .unwrap()
        .into_response()
}

pub async fn post_pending_session(
    State(state): State<AppState>,
    Json(req): Json<PendingSessionStartRequest>,
) -> impl IntoResponse {
    let agent_naked = match parse_agent(&req.agent) {
        Ok(a) => a,
        Err(msg) => {
            return api_error(StatusCode::BAD_REQUEST, "invalid agent format", Some(msg)).into_response();
        }
    };
    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let public_url = std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let login_url = format!("{public_url}/auth/login?session={}", urlencoding::encode(&session));
    let poll_url = format!("/api/v0/pending-session/{}", session);
    let s = PendingSession {
        agent: agent_naked,
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: None,
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
        Some((u, t)) => (true, Some(u.clone()), Some(t.clone())),
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
    auth_complete_page()
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
        user: username,
        agents_bound,
    })
    .into_response()
}

