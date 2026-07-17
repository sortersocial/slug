use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use axum_extra::extract::cookie::CookieJar;
use base64::Engine;
use serde::Deserialize;
use slug_types::{
    PendingSessionPollResponse, PendingSessionStartRequest, PendingSessionStartResponse,
    WhoamiResponse,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{oneshot, RwLock};

use crate::{
    api::helpers::{api_error, now_ms, sha256_hex},
    events::{Event, TokenIssued},
    html::{
        auth_complete_page, auth_signed_in_fragment, choose_username_error_fragment,
        choose_username_page, theme_cookie_header_from_jar, theme_from_jar, theme_next_from_uri,
        JsBuilder,
    },
    identity::{parse_agent, parse_username},
    reducer::ReducerState,
    state::{AppState, PendingSession},
    write_cmd::WriteCmd,
};

/// Delegate id for browser users who land via `/join/inv_…` (no CLI agent).
const INVITE_BROWSER_AGENT: &str = "00000000-0000-0000-0000-000000000000:invite:web/join";

/// Agent id for `/login` browser OAuth (no CLI); must pass [`parse_agent`].
pub const WEB_BROWSER_AGENT: &str = "00000000-0000-0000-0000-000000000001:social:web/browser";

/// True for well-known browser / human-form sentinel delegates (not real AI agents).
/// HTML attribution should show the human username for these, not `@@uuid:rig:…`.
pub fn is_browser_sentinel_delegate(agent: &str) -> bool {
    agent == WEB_BROWSER_AGENT || agent == INVITE_BROWSER_AGENT
}

/// HttpOnly cookie storing the same `slug_*` bearer string the CLI uses.
pub const SLUG_SESSION_COOKIE: &str = "slug_session";

/// `Set-Cookie` header value (full attribute string).
pub fn session_cookie_header_value(bearer: &str) -> HeaderValue {
    let s =
        format!("{SLUG_SESSION_COOKIE}={bearer}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000");
    HeaderValue::from_str(&s).expect("session cookie value must be ASCII")
}

fn safe_local_redirect(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.starts_with('/') && !s.starts_with("//") && s.len() < 8192 {
        Some(s.to_string())
    } else {
        None
    }
}

fn redirect_query(next: Option<&str>) -> String {
    safe_local_redirect(next)
        .map(|n| format!("&next={}", urlencoding::encode(&n)))
        .unwrap_or_default()
}

fn js_form_error_fragment(session: &str, error: &str) -> Response {
    JsBuilder::new()
        .id("choose-username-form")
        .morph_inner(choose_username_error_fragment(session, error))
        .into_response()
}

fn js_signed_in_fragment(bearer: &str, jar: &CookieJar, redirect_to: &str) -> Response {
    let mut response = JsBuilder::new()
        .id("choose-username-form")
        .morph_inner(auth_signed_in_fragment())
        .redirect(redirect_to)
        .into_response();
    let headers = response.headers_mut();
    headers.append(header::SET_COOKIE, session_cookie_header_value(bearer));
    if let Some(theme) = theme_cookie_header_from_jar(jar) {
        headers.append(header::SET_COOKIE, theme);
    }
    response
}

/// Resolve the signed-in username from `Authorization: Bearer` or `slug_session` cookie.
pub fn optional_principal(
    headers: &HeaderMap,
    jar: &CookieJar,
    reduced: &ReducerState,
) -> Option<String> {
    if let Ok(u) = verify_bearer_principal(headers, reduced) {
        return Some(u);
    }
    let c = jar.get(SLUG_SESSION_COOKIE)?;
    verify_token(reduced, c.value()).ok()
}

/// Browser session: principal + bearer token string (same shape as CLI session cookie).
#[derive(Debug, Clone)]
pub struct WebSession {
    pub username: String,
    pub bearer: String,
}

/// Resolve username and bearer together for `POST /ui` dispatch (one read of headers + jar).
pub fn resolve_web_session(
    headers: &HeaderMap,
    jar: &CookieJar,
    reduced: &ReducerState,
) -> Option<WebSession> {
    let username = optional_principal(headers, jar, reduced)?;
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
        .or_else(|| jar.get(SLUG_SESSION_COOKIE).map(|c| c.value().to_string()))?;
    Some(WebSession { username, bearer })
}

fn redirect_with_session_cookie(
    public_url: &str,
    path_and_query: &str,
    bearer: &str,
    jar: &CookieJar,
) -> Response {
    let mut res = Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, format!("{public_url}{path_and_query}"))
        .body(Body::empty())
        .unwrap();
    let headers = res.headers_mut();
    headers.append(header::SET_COOKIE, session_cookie_header_value(bearer));
    if let Some(theme) = theme_cookie_header_from_jar(jar) {
        headers.append(header::SET_COOKIE, theme);
    }
    res
}

fn pending_sessions(state: &AppState) -> Arc<RwLock<HashMap<String, PendingSession>>> {
    state.pending_sessions.clone()
}

/// Decode the `sub` claim from a JWT payload without verifying the signature.
/// Safe here because the token was received directly from Google's token endpoint over TLS.
fn extract_jwt_sub(jwt: &str) -> Option<String> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("sub")?.as_str().map(|s| s.to_string())
}

pub(crate) fn parse_bearer(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing Authorization header".to_string(),
        ));
    };
    let Ok(s) = value.to_str() else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid Authorization header".to_string(),
        ));
    };
    let s = s.trim();
    let Some(rest) = s.strip_prefix("Bearer ") else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authorization must be Bearer".to_string(),
        ));
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

pub(crate) fn verify_token(
    reduced: &crate::reducer::ReducerState,
    bearer: &str,
) -> Result<String, (StatusCode, String)> {
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
pub(crate) fn issue_token_for_user(stored_username: &str) -> (String, TokenIssued) {
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
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub redirect: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JoinInviteQuery {
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub redirect: Option<String>,
}

pub async fn get_join_invite(
    Path(token): Path<String>,
    Query(q): Query<JoinInviteQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
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
    let redirect_next = safe_local_redirect(q.next.as_deref().or(q.redirect.as_deref()));
    let s = PendingSession {
        agent: INVITE_BROWSER_AGENT.to_string(),
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: Some(token),
        redirect_next: redirect_next.clone(),
        complete: None,
    };
    state
        .pending_sessions
        .write()
        .await
        .insert(session.clone(), s);

    let public_url =
        std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let next_q = redirect_next
        .as_deref()
        .map(|n| redirect_query(Some(n)))
        .unwrap_or_default();
    Redirect::temporary(&format!(
        "{public_url}/auth/login?session={}{}",
        urlencoding::encode(&session),
        next_q
    ))
    .into_response()
}

pub async fn get_auth_login(
    Query(q): Query<AuthLoginQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Redirect to Google auth endpoint.
    let sessions = pending_sessions(&state);
    {
        let mut sessions_write = sessions.write().await;
        let Some(s) = sessions_write.get_mut(&q.session) else {
            return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
        };
        if let Some(next) = safe_local_redirect(q.next.as_deref().or(q.redirect.as_deref())) {
            s.redirect_next = Some(next);
        }
    }

    let auth_url_base = std::env::var("SLUG_GOOGLE_AUTH_URL")
        .unwrap_or_else(|_| "https://accounts.google.com/o/oauth2/v2/auth".to_string());
    let client_id = std::env::var("SLUG_GOOGLE_CLIENT_ID").unwrap_or_else(|_| "dev".to_string());
    let public_url =
        std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
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

pub async fn get_auth_callback(
    Query(q): Query<AuthCallbackQuery>,
    State(state): State<AppState>,
    jar: CookieJar,
) -> impl IntoResponse {
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
    let client_secret =
        std::env::var("SLUG_GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| "dev".to_string());
    let public_url =
        std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
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
            Err(err) => {
                return api_error(
                    StatusCode::BAD_GATEWAY,
                    "oauth token exchange failed",
                    Some(format!("{err}")),
                )
                .into_response()
            }
        },
        Err(err) => {
            return api_error(
                StatusCode::BAD_GATEWAY,
                "oauth token exchange failed",
                Some(format!("{err}")),
            )
            .into_response()
        }
    };

    // Extract sub from the id_token JWT payload (base64-decode middle segment).
    // The token arrived directly from Google over TLS — no need for an extra userinfo roundtrip.
    let sub = match extract_jwt_sub(&tr.id_token) {
        Some(s) => s,
        None => {
            return api_error(
                StatusCode::BAD_GATEWAY,
                "oauth: could not extract sub from id_token",
                None,
            )
            .into_response()
        }
    };

    // If user exists, issue token and complete session. Otherwise redirect to choose-username.
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let provider_key = ("google".to_string(), sub.clone());
    let existing = reduced.users_by_provider.get(&provider_key).cloned();
    drop(reduced);

    {
        let mut sessions_write = sessions.write().await;
        let s = sessions_write
            .get_mut(&q.state)
            .expect("session checked above");
        s.provider = Some("google".to_string());
        s.provider_id = Some(sub.clone());
        if let Some(username) = existing {
            let invite_tok = s.redeem_invite.clone();
            let (bearer, token_event) = issue_token_for_user(&username);
            let ev = Event::TokenIssued(token_event);
            let (tx, rx) = oneshot::channel();
            if state
                .write_tx
                .send(WriteCmd::OAuthTokenIssue {
                    token_event: ev,
                    redeem_invite: invite_tok,
                    reply: tx,
                })
                .await
                .is_err()
            {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "writer unavailable",
                    None,
                )
                .into_response();
            }
            match rx.await {
                Err(_) => {
                    return api_error(StatusCode::INTERNAL_SERVER_ERROR, "writer dropped", None)
                        .into_response();
                }
                Ok(Err(err)) => {
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to persist token",
                        Some(err),
                    )
                    .into_response();
                }
                Ok(Ok(())) => {}
            }
            let redirect_to =
                safe_local_redirect(s.redirect_next.as_deref()).unwrap_or_else(|| "/".to_string());
            let cookie_bearer = bearer.clone();
            s.complete = Some((username, bearer));
            return redirect_with_session_cookie(&public_url, &redirect_to, &cookie_bearer, &jar)
                .into_response();
        }
    }

    let next_q = {
        let sessions_read = sessions.read().await;
        sessions_read
            .get(&q.state)
            .and_then(|s| s.redirect_next.as_deref())
            .map(|n| redirect_query(Some(n)))
            .unwrap_or_default()
    };
    Redirect::temporary(&format!(
        "{public_url}/auth/choose-username?session={}{}",
        urlencoding::encode(&q.state),
        next_q
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ChooseUsernameQuery {
    pub session: String,
    pub error: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub redirect: Option<String>,
}

pub async fn get_choose_username(
    Query(q): Query<ChooseUsernameQuery>,
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let sessions = pending_sessions(&state);
    {
        let mut sessions_write = sessions.write().await;
        let Some(s) = sessions_write.get_mut(&q.session) else {
            return api_error(StatusCode::NOT_FOUND, "unknown session", None).into_response();
        };
        if let Some(next) = safe_local_redirect(q.next.as_deref().or(q.redirect.as_deref())) {
            s.redirect_next = Some(next);
        }
    }
    let next = theme_next_from_uri(&uri);
    choose_username_page(&q.session, q.error.as_deref(), theme_from_jar(&jar), &next)
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ChooseUsernameForm {
    pub session: String,
    pub username: String,
}

pub async fn post_choose_username(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ChooseUsernameForm>,
) -> impl IntoResponse {
    let canon_user = match parse_username(&form.username) {
        Ok(u) => u,
        Err(msg) => {
            return js_form_error_fragment(&form.session, &format!("invalid username — {msg}"))
                .into_response()
        }
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
        return js_form_error_fragment(&form.session, &format!("invalid agent format — {msg}"))
            .into_response();
    }

    let redeem_invite = {
        let sessions_read = sessions.read().await;
        sessions_read
            .get(&form.session)
            .and_then(|s| s.redeem_invite.clone())
    };

    let (tx, rx) = oneshot::channel();
    if state
        .write_tx
        .send(WriteCmd::Register {
            username: canon_user.clone(),
            provider,
            provider_id,
            redeem_invite,
            reply: tx,
        })
        .await
        .is_err()
    {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "writer unavailable",
            None,
        )
        .into_response();
    }

    let bearer = match rx.await {
        Err(_) => {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "writer dropped", None)
                .into_response();
        }
        Ok(Err(msg)) => {
            return js_form_error_fragment(&form.session, &msg).into_response();
        }
        Ok(Ok(b)) => b,
    };

    let redirect_to = {
        let mut sessions_write = sessions.write().await;
        let s = sessions_write
            .get_mut(&form.session)
            .expect("session checked above");
        s.complete = Some((canon_user.clone(), bearer.clone()));
        safe_local_redirect(s.redirect_next.as_deref())
            .unwrap_or_else(|| "/auth/complete".to_string())
    };

    js_signed_in_fragment(&bearer, &jar, &redirect_to).into_response()
}

/// Start a browser-only OAuth flow (no CLI polling). Sets session cookie on success.
#[derive(Debug, Deserialize)]
pub struct WebLoginQuery {
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub redirect: Option<String>,
}

pub async fn get_web_login(
    Query(q): Query<WebLoginQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let redirect_next = safe_local_redirect(q.next.as_deref().or(q.redirect.as_deref()))
        .or_else(|| Some("/".to_string()));
    let s = PendingSession {
        agent: WEB_BROWSER_AGENT.to_string(),
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: None,
        redirect_next: redirect_next.clone(),
        complete: None,
    };
    state
        .pending_sessions
        .write()
        .await
        .insert(session.clone(), s);
    let public_url =
        std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let next_q = redirect_next
        .as_deref()
        .map(|n| redirect_query(Some(n)))
        .unwrap_or_default();
    Redirect::temporary(&format!(
        "{public_url}/auth/login?session={}{}",
        urlencoding::encode(&session),
        next_q
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
            return api_error(StatusCode::BAD_REQUEST, "invalid agent format", Some(msg))
                .into_response();
        }
    };
    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let public_url =
        std::env::var("SLUG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let login_url = format!(
        "{public_url}/auth/login?session={}",
        urlencoding::encode(&session)
    );
    let poll_url = format!("/api/v0/pending-session/{session}");
    let s = PendingSession {
        agent: agent_naked,
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: None,
        redirect_next: None,
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

pub async fn get_auth_complete(jar: CookieJar, uri: Uri) -> impl IntoResponse {
    let next = theme_next_from_uri(&uri);
    auth_complete_page(theme_from_jar(&jar), &next).into_response()
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
    Json(WhoamiResponse { user: username }).into_response()
}
