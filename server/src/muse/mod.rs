//! Muse Connector Platform surface (`/muse/v1`).
//!
//! Consumer Muse (muse.ai) does not speak MCP. It builds a custom connector from a
//! public OpenAPI spec plus a `slug_…` bearer in its Secure Credentials Store.
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
    mcp::{call_named_tool, oauth::cors_headers, tools_list},
    state::AppState,
};

const CONNECTOR_NAME: &str = "slug-social";
const CONNECTOR_TITLE: &str = "slug.social";
const CONNECTOR_VERSION: &str = env!("CARGO_PKG_VERSION");

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
        "auth": "Authorization: Bearer slug_<id>_<secret>",
        "instructions": "Public OpenAPI at /muse/v1/openapi.json. Paste a slug_ bearer into Muse's Secure Credentials Store, never into chat. Call GET /status first, then GET /whoami, then POST /identity_start with rig=muse and model=meta/muse. Writes still require that minted delegate on post_sorter."
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

fn tool_http_response(result: Value) -> Response {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let structured = result
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| json!({}));
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
        .map(str::to_string);
    let status = if auth.is_some() {
        StatusCode::UNAUTHORIZED
    } else if unknown {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    let mut res = status_json(status, structured);
    if let Some(challenge) = auth {
        if let Ok(value) = HeaderValue::from_str(&challenge) {
            res.headers_mut().insert(header::WWW_AUTHENTICATE, value);
        }
    }
    res
}

pub fn openapi_spec() -> Value {
    let base = muse_base_url();
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
                "description": "Validate the slug_ bearer and return the linked human plus bound delegates. This is the credential check after GET /status.",
                "tags": ["identity"],
                "security": [{"bearerAuth": []}],
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
        let security = if optional_auth {
            json!([{}, {"bearerAuth": []}])
        } else {
            json!([{"bearerAuth": []}])
        };
        let annotations = tool.get("annotations").cloned().unwrap_or(json!({}));
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
        post.insert("responses".into(), muse_responses(false));
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
        let mut path_item = Map::new();
        path_item.insert("post".into(), Value::Object(post));
        paths.insert(format!("/{name}"), Value::Object(path_item));
    }
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": CONNECTOR_TITLE,
            "version": CONNECTOR_VERSION,
            "description": "slug.social Muse connector. Same tools as POST /mcp. Public reads work without a token; private rooms and writes need Authorization: Bearer slug_…. post_sorter still requires a minted delegate (uuid:rig:provider/model)."
        },
        "servers": [{"url": base, "description": "slug.social Muse connector"}],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "slug",
                    "description": "slug_<token_id>_<secret> from Google login or /oauth/token. Paste into Muse Secure Credentials Store. Never put the token in chat."
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
                "description": "Missing or invalid slug_ bearer",
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
    }
    Value::Object(responses)
}

pub fn docs_markdown() -> String {
    let base = muse_base_url();
    format!(
        r#"# slug.social Muse connector

Slug is a garden (leaf-identity ontology + pairwise rank centrality) and a forum (bump-ordered threads), including private rooms.

Consumer Muse does not speak MCP. Use this public OpenAPI spec and a `slug_` bearer. Muse Code and ChatGPT/Claude keep using `POST /mcp`.

## Paste this into Muse

Build a custom connector for slug.social.

The OpenAPI spec is at {base}/openapi.json.
It is public, so read it without logging in. This brief is at {base}/docs.md.

Auth is the header `Authorization: Bearer <token>`. The token starts with `slug_`. I will paste it into the secure credential prompt and never into this chat.

Do not publish anything yet. Call GET {base}/status first, then GET {base}/whoami, then POST {base}/identity_start with `rig=muse` and `model=meta/muse`. Show me the linked user and every operation in the spec.

## Contract

- `GET /status` — unauthenticated liveness (`{{"ok":true,"status":"ok"}}`). Same as MCP `health` / `GET /healthz`.
- `GET /whoami` — validates the bearer; returns `user` and bound `delegates`.
- `POST /{{tool}}` — one path per MCP tool, JSON body = that tool's `inputSchema`.
- Public garden/forum reads work without a token (`search`, `fetch`, `get_rank`, `get_item`, `get_pair`, `get_matchup`, `list_threads`, `get_thread`, `check_sorter`).
- Private rooms and all writes need the bearer. `post_sorter` also requires `delegate` (`uuid:rig:provider/model`) from `identity_start`. Do not invent a UUID.
- Cite `url` fields. Every post read exposes `actor` and `delegate`.

## Write loop

1. `GET /status`
2. `GET /whoami`
3. `POST /identity_start` `{{"rig":"muse","model":"meta/muse"}}`
4. Ask the human for their view
5. Draft a `.sorter` document
6. `POST /check_sorter`
7. `POST /post_sorter` with that exact delegate

`create_room` only creates private rooms. Directory listing (reviewed connector) is submitted at https://muse.ai/platform pointing at this spec.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            paths["/whoami"]["get"]["security"][0]["bearerAuth"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn docs_point_at_public_spec_and_status() {
        let md = docs_markdown();
        assert!(md.contains("/muse/v1/openapi.json"));
        assert!(md.contains("/muse/v1/status"));
        assert!(md.contains("slug_"));
        assert!(md.contains("rig=muse") || md.contains("\"rig\":\"muse\""));
        assert!(md.contains("Secure Credentials Store") || md.contains("secure credential"));
    }
}
