//! Thin OAuth 2.1 authorization server for ChatGPT / Codex / Claude MCP clients.
//!
//! Those hosts are the OAuth clients. Humans still sign in with the existing
//! Google login. On success we mint a one-time authorization code and the token
//! endpoint returns the same `slug_…` bearer the rest of the app already
//! verifies.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    api::{now_ms, public_url},
    state::{AppState, McpOauthCode, McpOauthRequest, PendingSession},
};

const CODE_TTL_MS: i64 = 10 * 60 * 1000;

pub fn mcp_resource_url() -> String {
    format!("{}/mcp", public_url())
}

pub fn issuer_url() -> String {
    public_url()
}

fn authorization_endpoint() -> String {
    format!("{}/oauth/authorize", public_url())
}

fn token_endpoint() -> String {
    format!("{}/oauth/token", public_url())
}

fn protected_resource_metadata_url() -> String {
    format!("{}/.well-known/oauth-protected-resource", public_url())
}

pub fn www_authenticate_challenge(error: &str, description: &str) -> String {
    format!(
        "Bearer resource_metadata=\"{}\", error=\"{}\", error_description=\"{}\"",
        protected_resource_metadata_url(),
        error,
        description.replace('"', "'")
    )
}

pub async fn oauth_protected_resource() -> impl IntoResponse {
    let resource = mcp_resource_url();
    Json(serde_json::json!({
        "resource": resource,
        "authorization_servers": [issuer_url()],
        "scopes_supported": ["slug.read", "slug.write"],
        "resource_documentation": format!("{}/", public_url()),
    }))
}

pub async fn oauth_authorization_server() -> impl IntoResponse {
    Json(serde_json::json!({
        "issuer": issuer_url(),
        "authorization_endpoint": authorization_endpoint(),
        "token_endpoint": token_endpoint(),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
        "scopes_supported": ["slug.read", "slug.write"],
    }))
}

pub async fn openai_apps_challenge() -> impl IntoResponse {
    match std::env::var("SLUG_OPENAI_APPS_CHALLENGE") {
        Ok(token) if !token.trim().is_empty() => {
            let mut res = token.trim().to_string().into_response();
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            res
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    pub response_type: Option<String>,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub state: Option<String>,
    pub resource: Option<String>,
    pub scope: Option<String>,
}

fn https_redirect_host_allowed(host: &str) -> bool {
    matches!(host, "chatgpt.com" | "chat.openai.com" | "claude.ai")
        || host.ends_with(".chatgpt.com")
        || host.ends_with(".chat.openai.com")
        || host.ends_with(".claude.ai")
}

pub fn redirect_uri_allowed(redirect_uri: &str) -> bool {
    let Ok(url) = url::Url::parse(redirect_uri) else {
        return false;
    };
    match url.scheme() {
        "https" => https_redirect_host_allowed(url.host_str().unwrap_or_default()),
        "http" => {
            let host = url.host_str().unwrap_or_default();
            host == "127.0.0.1" || host == "localhost" || host == "[::1]"
        }
        _ => false,
    }
}

fn authorize_error_redirect(
    redirect_uri: &str,
    state: Option<&str>,
    error: &str,
    desc: &str,
) -> Response {
    let mut url = match url::Url::parse(redirect_uri) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid redirect_uri and {error}: {desc}"),
            )
                .into_response();
        }
    };
    url.query_pairs_mut()
        .append_pair("error", error)
        .append_pair("error_description", desc)
        .append_pair("iss", &issuer_url());
    if let Some(state) = state {
        url.query_pairs_mut().append_pair("state", state);
    }
    Redirect::temporary(url.as_str()).into_response()
}

pub async fn oauth_authorize(
    Query(q): Query<AuthorizeQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let redirect_uri = q.redirect_uri.clone().unwrap_or_default();
    let state_q = q.state.clone();
    if !redirect_uri_allowed(&redirect_uri) {
        return (
            StatusCode::BAD_REQUEST,
            "redirect_uri is not an allowed ChatGPT, Claude, or localhost callback",
        )
            .into_response();
    }
    if q.response_type.as_deref() != Some("code") {
        return authorize_error_redirect(
            &redirect_uri,
            state_q.as_deref(),
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    if q.code_challenge_method.as_deref() != Some("S256") {
        return authorize_error_redirect(
            &redirect_uri,
            state_q.as_deref(),
            "invalid_request",
            "code_challenge_method must be S256",
        );
    }
    let Some(code_challenge) = q
        .code_challenge
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return authorize_error_redirect(
            &redirect_uri,
            state_q.as_deref(),
            "invalid_request",
            "code_challenge is required",
        );
    };
    let Some(client_id) = q
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return authorize_error_redirect(
            &redirect_uri,
            state_q.as_deref(),
            "invalid_request",
            "client_id is required",
        );
    };

    let expected_resource = mcp_resource_url();
    let resource = q
        .resource
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&expected_resource)
        .to_string();
    if resource != expected_resource {
        return authorize_error_redirect(
            &redirect_uri,
            state_q.as_deref(),
            "invalid_target",
            "resource must be this server's /mcp URL",
        );
    }

    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let pending = PendingSession {
        agent: None,
        created_ts: now_ms(),
        provider: None,
        provider_id: None,
        redeem_invite: None,
        redirect_next: None,
        mcp_oauth: Some(McpOauthRequest {
            client_id,
            redirect_uri,
            state: state_q,
            code_challenge,
            resource,
            scope: q.scope,
        }),
        complete: None,
    };
    state
        .pending_sessions
        .write()
        .await
        .insert(session.clone(), pending);

    Redirect::temporary(&format!(
        "{}/auth/login?session={}",
        public_url(),
        urlencoding::encode(&session)
    ))
    .into_response()
}

/// After Google login / username choice, mint a code and send the user back to the MCP client.
pub async fn finish_mcp_oauth_if_pending(
    state: &AppState,
    session_id: &str,
    username: &str,
    bearer: &str,
) -> Option<String> {
    let req = {
        let sessions = state.pending_sessions.read().await;
        sessions.get(session_id)?.mcp_oauth.clone()
    }?;
    let code = format!("ac_{}", uuid::Uuid::new_v4().simple());
    state.mcp_oauth_codes.write().await.insert(
        code.clone(),
        McpOauthCode {
            username: username.to_string(),
            bearer: bearer.to_string(),
            client_id: req.client_id,
            redirect_uri: req.redirect_uri.clone(),
            code_challenge: req.code_challenge,
            resource: req.resource,
            created_ts: now_ms(),
        },
    );
    let mut url = url::Url::parse(&req.redirect_uri).ok()?;
    url.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("iss", &issuer_url());
    if let Some(st) = req.state.as_deref() {
        url.query_pairs_mut().append_pair("state", st);
    }
    Some(url.to_string())
}

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    pub grant_type: Option<String>,
    pub code: Option<String>,
    pub code_verifier: Option<String>,
    pub redirect_uri: Option<String>,
    pub client_id: Option<String>,
    pub resource: Option<String>,
}

#[derive(Debug, Serialize)]
struct TokenError {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_description: Option<String>,
}

fn token_err(status: StatusCode, error: &str, desc: &str) -> Response {
    (
        status,
        Json(TokenError {
            error: error.to_string(),
            error_description: Some(desc.to_string()),
        }),
    )
        .into_response()
}

pub fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub async fn oauth_token(
    State(state): State<AppState>,
    Form(form): Form<TokenForm>,
) -> impl IntoResponse {
    if form.grant_type.as_deref() != Some("authorization_code") {
        return token_err(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code is supported",
        );
    }
    let Some(code) = form
        .code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return token_err(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "code is required",
        );
    };
    let Some(verifier) = form
        .code_verifier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return token_err(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "code_verifier is required",
        );
    };

    let grant = {
        let mut codes = state.mcp_oauth_codes.write().await;
        codes.remove(code)
    };
    let Some(grant) = grant else {
        return token_err(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "unknown or reused code",
        );
    };
    if now_ms().saturating_sub(grant.created_ts) > CODE_TTL_MS {
        return token_err(StatusCode::BAD_REQUEST, "invalid_grant", "code expired");
    }
    if let Some(client_id) = form.client_id.as_deref() {
        if client_id != grant.client_id {
            return token_err(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "client_id mismatch",
            );
        }
    }
    if let Some(redirect_uri) = form.redirect_uri.as_deref() {
        if redirect_uri != grant.redirect_uri {
            return token_err(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "redirect_uri mismatch",
            );
        }
    }
    if let Some(resource) = form.resource.as_deref().filter(|s| !s.is_empty()) {
        if resource != grant.resource {
            return token_err(
                StatusCode::BAD_REQUEST,
                "invalid_target",
                "resource mismatch",
            );
        }
    }
    if pkce_s256_challenge(verifier) != grant.code_challenge {
        return token_err(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "PKCE verification failed",
        );
    }

    Json(serde_json::json!({
        "access_token": grant.bearer,
        "token_type": "Bearer",
        "expires_in": 31536000,
        "scope": "slug.read slug.write",
    }))
    .into_response()
}

pub fn cors_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(
            "authorization, content-type, mcp-session-id, mcp-protocol-version, accept",
        ),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("www-authenticate, mcp-session-id"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_rfc7636_example() {
        // RFC 7636 appendix B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn chatgpt_redirect_allowed() {
        assert!(redirect_uri_allowed(
            "https://chatgpt.com/connector_platform_oauth_redirect"
        ));
        assert!(redirect_uri_allowed(
            "https://chatgpt.com/connector/oauth/abc"
        ));
        assert!(redirect_uri_allowed(
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(redirect_uri_allowed(
            "https://www.claude.ai/api/mcp/auth_callback"
        ));
        assert!(redirect_uri_allowed("http://127.0.0.1:9/cb"));
        assert!(redirect_uri_allowed("http://localhost:3118/callback"));
        assert!(!redirect_uri_allowed("https://evil.example/cb"));
        assert!(!redirect_uri_allowed(
            "https://notclaude.ai/api/mcp/auth_callback"
        ));
        assert!(!redirect_uri_allowed("/local"));
    }
}
