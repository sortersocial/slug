//! Muse Connector Platform surface (`/muse/v1`).
//!
//! Consumer Muse (muse.ai) does not speak MCP. It builds a custom connector from a
//! public OpenAPI spec. Humans never see a `slug_` token: the agent starts
//! `identity_start` and shows `login_url` (Google register / OAuth). After
//! `identity_poll` completes, Muse sends `X-Slug-Session`. Host OAuth 2.1 + PKCE
//! is the directory/Connect path; the access token stays in the host vault.
//! Tools call [`crate::mcp::call_named_tool`] so garden/forum writes still go through
//! the same event-log path as `POST /mcp` and `POST /api/v0/rpc`.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};

use crate::{
    api::public_url,
    mcp::{
        call_named_tool,
        oauth::{cors_headers, www_authenticate_challenge_muse},
        tools_list,
    },
    state::AppState,
};

const CONNECTOR_NAME: &str = "slug-social";
const CONNECTOR_TITLE: &str = "slug.social";
const CONNECTOR_VERSION: &str = env!("CARGO_PKG_VERSION");
const SESSION_HEADER: &str = "x-slug-session";
const AUTH_NEXT: &str = "POST /identity_start with rig=muse and model=meta/muse. Show login_url as a clickable OAuth/register link. Never ask the human for a token.";

pub fn muse_base_url() -> String {
    format!("{}/muse/v1", public_url())
}

pub fn muse_routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/muse/v1", get(muse_index).options(muse_options))
        .route("/muse/v1/", get(muse_index).options(muse_options))
        .route(
            "/muse/v1/openapi.json",
            get(muse_openapi).options(muse_options),
        )
        .route("/muse/v1/docs.md", get(muse_docs).options(muse_options))
        .route("/muse.md", get(muse_docs).options(muse_options))
        .route("/muse/v1/status", get(muse_status).options(muse_options))
        .route("/muse/v1/whoami", get(muse_whoami).options(muse_options))
        .route(
            "/muse/v1/:tool",
            post(muse_tool_post)
                .get(muse_tool_get)
                .options(muse_options),
        )
}

pub async fn muse_options() -> impl IntoResponse {
    let mut res = StatusCode::NO_CONTENT.into_response();
    cors_headers(res.headers_mut());
    res
}

fn json_cors(body: Value) -> Response {
    let mut res = Json(body).into_response();
    cors_headers(res.headers_mut());
    res
}

fn text_cors(body: String, content_type: &'static str) -> Response {
    let mut res = body.into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    cors_headers(res.headers_mut());
    res
}

async fn muse_index() -> impl IntoResponse {
    let base = muse_base_url();
    json_cors(json!({
        "name": CONNECTOR_NAME,
        "title": CONNECTOR_TITLE,
        "version": CONNECTOR_VERSION,
        "kind": "muse-connector",
        "status": format!("{base}/status"),
        "openapi": format!("{base}/openapi.json"),
        "docs": format!("{base}/docs.md"),
        "mcp": format!("{}/mcp", public_url()),
        "oauth_authorize": format!("{}/oauth/authorize", public_url()),
        "auth": "identity_start login_url (Google register). The human never sees a token.",
        "instructions": "Public OpenAPI at /muse/v1/openapi.json. Do not ask the human for a token. Call GET /status, then POST /identity_start with rig=muse and model=meta/muse, show login_url as a clickable OAuth link, then POST /identity_poll until complete. Afterwards send X-Slug-Session. Writes still require that minted delegate on post_sorter."
    }))
}

async fn muse_openapi() -> impl IntoResponse {
    json_cors(openapi_spec())
}

async fn muse_docs() -> impl IntoResponse {
    text_cors(docs_markdown(), "text/markdown; charset=utf-8")
}

async fn muse_status() -> impl IntoResponse {
    json_cors(json!({"ok": true, "status": "ok"}))
}

async fn muse_whoami(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let headers = resolve_muse_session_auth(&state, headers).await;
    let result = call_named_tool(&state, &headers, "whoami", json!({})).await;
    tool_http_response(result)
}

async fn muse_tool_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tool): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> impl IntoResponse {
    if is_doc_path(&tool) {
        return reserved_tool_response(&tool);
    }
    let name = if tool == "status" {
        "health"
    } else {
        tool.as_str()
    };
    let args = match body {
        Ok(Json(Value::Null)) => json!({}),
        Ok(Json(Value::Object(map))) => Value::Object(map),
        Ok(Json(_)) => {
            return status_json(
                StatusCode::BAD_REQUEST,
                json!({"error": "request body must be a JSON object"}),
            );
        }
        Err(JsonRejection::MissingJsonContentType(_)) => json!({}),
        Err(err) => {
            return status_json(StatusCode::BAD_REQUEST, json!({"error": err.body_text()}));
        }
    };
    let headers = resolve_muse_session_auth(&state, headers).await;
    let result = call_named_tool(&state, &headers, name, args).await;
    tool_http_response(result)
}

async fn muse_tool_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tool): Path<String>,
) -> impl IntoResponse {
    match tool.as_str() {
        "status" | "health" => return json_cors(json!({"ok": true, "status": "ok"})),
        "whoami" => {
            let headers = resolve_muse_session_auth(&state, headers).await;
            let result = call_named_tool(&state, &headers, "whoami", json!({})).await;
            return tool_http_response(result);
        }
        "openapi.json" | "docs.md" => return reserved_tool_response(&tool),
        _ => {}
    }
    if !tool_exists(&tool) {
        return status_json(
            StatusCode::NOT_FOUND,
            json!({"error": format!("unknown tool: {tool}")}),
        );
    }
    status_json(
        StatusCode::METHOD_NOT_ALLOWED,
        json!({"error": format!("use POST /muse/v1/{tool} with a JSON body")}),
    )
}

fn is_doc_path(tool: &str) -> bool {
    matches!(tool, "openapi.json" | "docs.md")
}

fn reserved_tool_response(tool: &str) -> Response {
    status_json(
        StatusCode::METHOD_NOT_ALLOWED,
        json!({"error": format!("GET /muse/v1/{tool} is the document; do not POST it")}),
    )
}

fn tool_exists(name: &str) -> bool {
    tools_list()
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|tools| {
            tools
                .iter()
                .any(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
        })
        .unwrap_or(false)
}

fn status_json(status: StatusCode, body: Value) -> Response {
    let mut res = (status, Json(body)).into_response();
    cors_headers(res.headers_mut());
    res
}

fn session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(SESSION_HEADER)?.to_str().ok()?.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// After `identity_poll` completes, Muse authenticates with `X-Slug-Session`
/// instead of a user-pasted bearer. Host OAuth still sends `Authorization`.
async fn resolve_muse_session_auth(state: &AppState, mut headers: HeaderMap) -> HeaderMap {
    if headers.get(header::AUTHORIZATION).is_some() {
        return headers;
    }
    let Some(session) = session_id_from_headers(&headers) else {
        return headers;
    };
    let sessions = state.pending_sessions.read().await;
    let Some((_, bearer)) = sessions.get(&session).and_then(|p| p.complete.clone()) else {
        return headers;
    };
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {bearer}")) {
        headers.insert(header::AUTHORIZATION, value);
    }
    headers
}

fn sanitize_muse_structured(mut structured: Value) -> Value {
    let Some(obj) = structured.as_object_mut() else {
        return structured;
    };
    obj.remove("poll_url");
    obj.remove("token");
    match obj.get("phase").and_then(|v| v.as_str()) {
        Some("present_oauth_url_to_user") => {
            obj.insert(
                "instruction".into(),
                json!("Show login_url as a clickable OAuth/register link. The human signs in with Google and picks a username. Never ask for, paste, or mention a token. Then call identity_poll with this session until complete, and send X-Slug-Session on later private/write calls."),
            );
        }
        Some("pending") => {
            obj.insert(
                "instruction".into(),
                json!("Keep showing login_url as a clickable link. Do not ask for a token. Poll identity_poll again."),
            );
        }
        Some("complete") => {
            obj.insert(
                "instruction".into(),
                json!("Linked. Send header X-Slug-Session with this session on later private-room and write calls. Never show or ask for a token."),
            );
        }
        _ => {}
    }
    structured
}

fn tool_http_response(result: Value) -> Response {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut structured = result
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| json!({}));
    structured = sanitize_muse_structured(structured);
    if !is_error {
        return json_cors(structured);
    }
    let message = structured
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("tool error")
        .to_string();
    let unknown = message.starts_with("unknown tool:");
    let auth = result
        .get("_meta")
        .and_then(|m| m.get("mcp/www_authenticate"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .is_some();
    let status = if auth {
        StatusCode::UNAUTHORIZED
    } else if unknown {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    if status == StatusCode::UNAUTHORIZED {
        if let Some(obj) = structured.as_object_mut() {
            obj.insert("next".into(), json!(AUTH_NEXT));
        }
    }
    let mut res = status_json(status, structured);
    if auth {
        let challenge = www_authenticate_challenge_muse(
            "insufficient_scope",
            "Link your slug.social account to continue.",
        );
        if let Ok(value) = HeaderValue::from_str(&challenge) {
            res.headers_mut().insert(header::WWW_AUTHENTICATE, value);
        }
    }
    res
}

pub fn openapi_spec() -> Value {
    let base = muse_base_url();
    let origin = public_url();
    let tools = tools_list()
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut paths = Map::new();
    paths.insert(
        "/status".into(),
        json!({
            "get": {
                "operationId": "getStatus",
                "summary": "Server liveness",
                "description": "Unauthenticated liveness check. Same payload as MCP health / GET /healthz. Call this first when building a custom connector.",
                "tags": ["status"],
                "security": [{}],
                "responses": {
                    "200": {
                        "description": "Process is serving requests",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["ok", "status"],
                                    "properties": {
                                        "ok": {"type": "boolean"},
                                        "status": {"type": "string"}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }),
    );
    paths.insert(
        "/whoami".into(),
        json!({
            "get": {
                "operationId": "getWhoami",
                "summary": "Linked identity",
                "description": "Return the linked human plus bound delegates after the human finishes the identity_start login_url (or host OAuth). Unlinked callers get 401 with next = show login_url. Never ask the human for a token.",
                "tags": ["identity"],
                "security": [{"oauth2": ["slug.read"]}],
                "responses": muse_responses(true)
            }
        }),
    );
    for tool in &tools {
        let Some(name) = tool.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let title = tool.get("title").and_then(|v| v.as_str()).unwrap_or(name);
        let description = tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let input = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or(json!({"type": "object"}));
        let optional_auth = tool_allows_noauth(tool);
        let annotations = tool.get("annotations").cloned().unwrap_or(json!({}));
        let read_only = annotations
            .get("readOnlyHint")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let oauth_scopes = if read_only {
            json!(["slug.read"])
        } else {
            json!(["slug.read", "slug.write"])
        };
        let security = if optional_auth {
            json!([{}, {"oauth2": oauth_scopes}])
        } else {
            json!([{"oauth2": oauth_scopes}])
        };
        let mut post = Map::new();
        post.insert("operationId".into(), json!(format!("call_{name}")));
        post.insert("summary".into(), json!(title));
        post.insert("description".into(), json!(description));
        post.insert("tags".into(), json!(["tools"]));
        post.insert("security".into(), security);
        post.insert(
            "requestBody".into(),
            json!({
                "required": input_has_required(&input),
                "content": {
                    "application/json": {
                        "schema": input
                    }
                }
            }),
        );
        post.insert("responses".into(), muse_responses(!optional_auth));
        post.insert("x-mcp-name".into(), json!(name));
        post.insert(
            "x-readOnlyHint".into(),
            annotations
                .get("readOnlyHint")
                .cloned()
                .unwrap_or(json!(true)),
        );
        post.insert(
            "x-openWorldHint".into(),
            annotations
                .get("openWorldHint")
                .cloned()
                .unwrap_or(json!(false)),
        );
        post.insert(
            "x-destructiveHint".into(),
            annotations
                .get("destructiveHint")
                .cloned()
                .unwrap_or(json!(false)),
        );
        let key = format!("/{name}");
        let mut path_item = match paths.remove(&key) {
            Some(Value::Object(existing)) => existing,
            _ => Map::new(),
        };
        path_item.insert("post".into(), Value::Object(post));
        paths.insert(key, Value::Object(path_item));
    }
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": CONNECTOR_TITLE,
            "version": CONNECTOR_VERSION,
            "description": "slug.social Muse connector. Same tools as POST /mcp. Public reads work without login. Private rooms and writes need the human to click the identity_start login_url (Google register) or host OAuth 2.1 + PKCE — the human never sees a token. post_sorter still requires a minted delegate (uuid:rig:provider/model)."
        },
        "servers": [{"url": base, "description": "slug.social Muse connector"}],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "oauth2": {
                    "type": "oauth2",
                    "description": "OAuth 2.1 authorization code + PKCE S256. The human clicks the authorize URL, signs in with Google, and picks a username. The access token stays in the host. Never show or ask the human for a token. Agent-driven linking uses POST /identity_start (show login_url) then X-Slug-Session.",
                    "flows": {
                        "authorizationCode": {
                            "authorizationUrl": format!("{origin}/oauth/authorize"),
                            "tokenUrl": format!("{origin}/oauth/token"),
                            "scopes": {
                                "slug.read": "Private-room reads and identity",
                                "slug.write": "Writes (post_sorter, create_room, …)"
                            }
                        }
                    }
                }
            }
        },
        "tags": [
            {"name": "status", "description": "Liveness"},
            {"name": "identity", "description": "Linked human and delegates"},
            {"name": "tools", "description": "Same catalog as MCP tools/list"}
        ]
    })
}

fn tool_allows_noauth(tool: &Value) -> bool {
    tool.get("securitySchemes")
        .and_then(|v| v.as_array())
        .map(|schemes| {
            schemes
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("noauth"))
        })
        .unwrap_or(true)
}

fn input_has_required(input: &Value) -> bool {
    input
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

fn muse_responses(auth_required: bool) -> Value {
    let mut responses = Map::new();
    responses.insert(
        "200".into(),
        json!({
            "description": "structuredContent from the matching MCP tool",
            "content": {
                "application/json": {
                    "schema": {"type": "object"}
                }
            }
        }),
    );
    responses.insert(
        "400".into(),
        json!({
            "description": "Validation or tool error",
            "content": {
                "application/json": {
                    "schema": {
                        "type": "object",
                        "properties": {"error": {"type": "string"}}
                    }
                }
            }
        }),
    );
    if auth_required {
        responses.insert(
            "401".into(),
            json!({
                "description": "Not linked. Call POST /identity_start and show login_url. Never ask for a token.",
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "properties": {
                                "error": {"type": "string"},
                                "next": {"type": "string"}
                            }
                        }
                    }
                }
            }),
        );
    }
    Value::Object(responses)
}

pub fn docs_markdown() -> String {
    let base = muse_base_url();
    format!(
        r#"# slug.social Muse connector

Slug is a garden (leaf-identity ontology + pairwise rank centrality) and a forum (bump-ordered threads), including private rooms.

Consumer Muse does not speak MCP. Use this public OpenAPI spec. Muse Code and ChatGPT/Claude keep using `POST /mcp`.

## Give this to Muse

Build a custom connector for slug.social.

The OpenAPI spec is at {base}/openapi.json.
It is public, so read it without logging in. This brief is at {base}/docs.md.

The human never has a token and must never be asked for one. Do not open a credential prompt. Do not mention tokens.

Do not publish anything yet. Call GET {base}/status first, then POST {base}/identity_start with `rig=muse` and `model=meta/muse`. Show `login_url` as a clickable OAuth/register link. The human signs in with Google and picks a username. Poll POST {base}/identity_poll with that `session` until `complete`. Then send header `X-Slug-Session: <session>` on private-room and write calls. Show the linked user and every operation in the spec.

## Contract

- `GET /status` — unauthenticated liveness (`{{"ok":true,"status":"ok"}}`). Same as MCP `health` / `GET /healthz`.
- `POST /identity_start` — starts the register flow. Unlinked callers get `login_url`. Show that URL. Never invent a UUID. Never ask for a token.
- `POST /identity_poll` — wait until the human finishes Google login. Then send `X-Slug-Session`.
- `GET /whoami` — linked human and bound `delegates` after that register flow (or host OAuth). Unlinked is HTTP 401 with `next` pointing at `identity_start`.
- `POST /{{tool}}` — one path per MCP tool, JSON body = that tool's `inputSchema`.
- Public garden/forum reads work without login (`search`, `fetch`, `get_rank`, `get_item`, `get_pair`, `get_matchup`, `list_threads`, `get_thread`, `check_sorter`).
- Private rooms and all writes need the completed session header (or host OAuth). `post_sorter` also requires `delegate` (`uuid:rig:provider/model`) from `identity_start`. Do not invent a UUID.
- Cite `url` fields. Every post read exposes `actor` and `delegate`.

## Write loop

1. `GET /status`
2. `POST /identity_start` `{{"rig":"muse","model":"meta/muse"}}` — show `login_url`
3. `POST /identity_poll` until complete, then `X-Slug-Session`
4. `GET /whoami`
5. Ask the human for their view
6. Draft a `.sorter` document
7. `POST /check_sorter`
8. `POST /post_sorter` with that exact delegate

`create_room` only creates private rooms. Directory listing (reviewed connector) is submitted at https://muse.ai/platform pointing at this spec. Host OAuth (authorization code + PKCE) is `/oauth/authorize` if Muse Connect uses the OpenAPI oauth2 scheme — still a click, never a pasted secret.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_user_token_copy(text: &str) {
        let lower = text.to_lowercase();
        assert!(
            !text.contains("slug_"),
            "user-facing Muse copy must not mention slug_ tokens: {text}"
        );
        assert!(
            !lower.contains("secure credential"),
            "must not tell Muse to use a credential vault paste: {text}"
        );
        for phrase in [
            "paste a token",
            "paste the token",
            "paste it into",
            "bearer <token>",
            "bearer token",
        ] {
            assert!(
                !lower.contains(phrase),
                "must not tell anyone to paste a token ({phrase}): {text}"
            );
        }
    }

    #[test]
    fn openapi_lists_every_mcp_tool() {
        let spec = openapi_spec();
        assert_eq!(spec["openapi"], "3.0.3");
        assert_eq!(spec["info"]["title"], CONNECTOR_TITLE);
        let paths = spec["paths"].as_object().unwrap();
        assert!(paths.contains_key("/status"));
        assert!(paths.contains_key("/whoami"));
        let catalog = tools_list();
        let tools = catalog["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(
                paths.contains_key(&format!("/{name}")),
                "missing /{name} in openapi paths {:?}",
                paths.keys().collect::<Vec<_>>()
            );
            assert_eq!(paths[&format!("/{name}")]["post"]["x-mcp-name"], name);
        }
        assert_eq!(paths["/post_sorter"]["post"]["x-readOnlyHint"], false);
        assert_eq!(paths["/post_sorter"]["post"]["x-openWorldHint"], true);
        assert_eq!(
            paths["/search"]["post"]["security"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.as_object().map(|o| o.is_empty()).unwrap_or(false)),
            true
        );
        assert!(paths["/whoami"]["get"].is_object());
        assert!(paths["/whoami"]["post"].is_object());
        assert_eq!(
            paths["/whoami"]["get"]["security"][0]["oauth2"]
                .as_array()
                .unwrap()
                .as_slice(),
            ["slug.read"]
        );
        assert_eq!(
            spec["components"]["securitySchemes"]["oauth2"]["type"],
            "oauth2"
        );
        assert!(spec["components"]["securitySchemes"]["bearerAuth"].is_null());
        assert_no_user_token_copy(&spec["info"]["description"].as_str().unwrap());
        assert_no_user_token_copy(
            spec["components"]["securitySchemes"]["oauth2"]["description"]
                .as_str()
                .unwrap(),
        );
    }

    #[test]
    fn docs_point_at_public_spec_and_login_url() {
        let md = docs_markdown();
        assert!(md.contains("/muse/v1/openapi.json"));
        assert!(md.contains("/muse/v1/status"));
        assert!(md.contains("login_url"));
        assert!(md.contains("identity_start"));
        assert!(md.contains("rig=muse") || md.contains("\"rig\":\"muse\""));
        assert!(md.contains("Never ask") || md.contains("never be asked"));
        assert_no_user_token_copy(&md);
    }
}
