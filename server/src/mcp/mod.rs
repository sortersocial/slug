//! ChatGPT / Codex MCP app surface (`POST /mcp`).
//!
//! Streamable HTTP with JSON request/response. Tools call [`crate::api::dispatch_rpc`]
//! so garden/forum writes still go through the same event-log path as `POST /api/v0/rpc`.

pub mod oauth;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use slug_types::*;

use crate::{api::dispatch_rpc, state::AppState};

use self::oauth::{cors_headers, www_authenticate_challenge};

const SERVER_NAME: &str = "slug-social";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const INSTRUCTIONS: &str = "\
Slug is a garden (path-addressed ontology + pairwise rank centrality) and a forum \
(bump-ordered threads). Read tools work anonymously on room public. Before posting \
a comparison, call get_pair or get_item, ask the human for their view, draft a \
.sorter document, call check_sorter, then post_sorter. Do not invent delegate \
UUIDs. Cite the url fields returned by tools. Writes require the user to link \
their slug.social account.";

pub async fn mcp_options() -> impl IntoResponse {
    let mut res = StatusCode::NO_CONTENT.into_response();
    cors_headers(res.headers_mut());
    res
}

pub async fn mcp_get() -> impl IntoResponse {
    let mut res = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"error": "use POST /mcp for JSON-RPC"})),
    )
        .into_response();
    cors_headers(res.headers_mut());
    res
}

pub async fn mcp_delete() -> impl IntoResponse {
    let mut res = StatusCode::NO_CONTENT.into_response();
    cors_headers(res.headers_mut());
    res
}

pub async fn mcp_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> impl IntoResponse {
    let response = handle_mcp_body(&state, &headers, body.0).await;
    let mut res = Json(response).into_response();
    cors_headers(res.headers_mut());
    res
}

async fn handle_mcp_body(state: &AppState, headers: &HeaderMap, body: Value) -> Value {
    if let Some(arr) = body.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            if let Some(resp) = handle_rpc(state, headers, item).await {
                out.push(resp);
            }
        }
        return Value::Array(out);
    }
    handle_rpc(state, headers, &body).await.unwrap_or(
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"invalid request"}}),
    )
}

async fn handle_rpc(state: &AppState, headers: &HeaderMap, body: &Value) -> Option<Value> {
    if body.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        return Some(json!({
            "jsonrpc": "2.0",
            "id": body.get("id").cloned().unwrap_or(Value::Null),
            "error": {"code": -32600, "message": "jsonrpc must be 2.0"}
        }));
    }
    let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = body.get("id").cloned();
    let params = body.get("params").cloned().unwrap_or(json!({}));
    if id.is_none() {
        return None;
    }
    let id = id.unwrap();
    let result = match method {
        "initialize" => initialize(&params),
        "ping" => json!({}),
        "tools/list" => tools_list(),
        "tools/call" => tools_call(state, headers, &params).await,
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method not found: {other}")}
            }));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn initialize(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-03-26");
    let protocol_version = match requested {
        "2024-11-05" | "2025-03-26" | "2025-11-25" | "2026-07-28" => requested,
        _ => "2025-03-26",
    };
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {"listChanged": false}
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
            "title": "slug.social"
        },
        "instructions": INSTRUCTIONS
    })
}

fn annotations(read_only: bool, open_world: bool, destructive: bool) -> Value {
    json!({
        "readOnlyHint": read_only,
        "openWorldHint": open_world,
        "destructiveHint": destructive,
    })
}

fn noauth() -> Value {
    json!([{"type": "noauth"}])
}

fn oauth_write() -> Value {
    json!([{"type": "oauth2", "scopes": ["slug.write"]}])
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    input: Value,
    output: Value,
    ann: Value,
    schemes: Value,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input,
        "outputSchema": output,
        "annotations": ann,
        "securitySchemes": schemes,
        "_meta": { "securitySchemes": schemes }
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            tool(
                "search",
                "Search slug",
                "Search public garden items, forum threads, and posts. Use this first when the user asks about something on slug.social.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"}
                    },
                    "required": ["query"]
                }),
                json!({
                    "type": "object",
                    "properties": {
                        "results": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {"type": "string"},
                                    "title": {"type": "string"},
                                    "url": {"type": "string"}
                                },
                                "required": ["id", "title", "url"]
                            }
                        }
                    },
                    "required": ["results"]
                }),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "fetch",
                "Fetch slug document",
                "Open one search hit by id (item:, thread:, or post:). Returns full text and a citation URL.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Id from search, e.g. item:~/languages/rust"}
                    },
                    "required": ["id"]
                }),
                json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "title": {"type": "string"},
                        "text": {"type": "string"},
                        "url": {"type": "string"},
                        "metadata": {"type": "object"}
                    },
                    "required": ["id", "title", "text", "url"]
                }),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "list_threads",
                "List forum threads",
                "List recently active public forum threads (bump-ordered).",
                json!({"type": "object", "properties": {}}),
                json!({"type": "object"}),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "get_thread",
                "Read a forum thread",
                "Read a public forum thread page. Use the tag without #.",
                json!({
                    "type": "object",
                    "properties": {
                        "thread_tag": {"type": "string"},
                        "offset": {"type": "integer"},
                        "limit": {"type": "integer"},
                        "post_id": {"type": "string"}
                    },
                    "required": ["thread_tag"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "get_rank",
                "Garden ranking",
                "Ranked children under a garden parent path (e.g. ~ or ~/languages).",
                json!({
                    "type": "object",
                    "properties": {
                        "parent_path": {"type": "string", "description": "Garden parent, default ~"},
                        "depth": {"type": "integer"},
                        "offset": {"type": "integer"},
                        "limit": {"type": "integer"},
                        "percent": {"type": "boolean"}
                    }
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "get_item",
                "Garden item",
                "Item body plus related thread tags for a garden path.",
                json!({
                    "type": "object",
                    "properties": {
                        "item_path": {"type": "string"},
                        "full": {"type": "boolean"}
                    },
                    "required": ["item_path"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "get_pair",
                "Next comparison pair",
                "Suggest the next pairwise comparison under a garden parent path.",
                json!({
                    "type": "object",
                    "properties": {
                        "parent_path": {"type": "string", "description": "Garden parent, default ~"}
                    }
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "check_sorter",
                "Dry-run a .sorter document",
                "Parse and preview ranking effects of a .sorter document without writing.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "Full .sorter document"}
                    },
                    "required": ["text"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                noauth(),
            ),
            tool(
                "post_sorter",
                "Publish a .sorter document",
                "Publish a comparison or item definition to a public forum thread. Requires the user to link slug.social. Ask the human before posting. Do not invent delegate UUIDs.",
                json!({
                    "type": "object",
                    "properties": {
                        "thread_tag": {"type": "string", "description": "Forum tag without #"},
                        "text": {"type": "string", "description": "Full .sorter document"}
                    },
                    "required": ["thread_tag", "text"]
                }),
                json!({"type": "object"}),
                annotations(false, true, false),
                oauth_write(),
            ),
            tool(
                "redact_post",
                "Redact a post",
                "Tombstone the signed-in user's post and remove its garden contributions. Irreversible.",
                json!({
                    "type": "object",
                    "properties": {
                        "post_id": {"type": "string"}
                    },
                    "required": ["post_id"]
                }),
                json!({"type": "object"}),
                annotations(false, true, true),
                oauth_write(),
            ),
        ]
    })
}

fn arg_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .map(|n| n as usize)
            .or_else(|| v.as_str()?.parse().ok())
    })
}

fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| {
        v.as_bool().or_else(|| match v.as_str()? {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        })
    })
}

fn tool_ok(structured: Value, summary: impl Into<String>) -> Value {
    let text = if structured.is_null() {
        summary.into()
    } else {
        structured.to_string()
    };
    json!({
        "structuredContent": structured,
        "content": [{"type": "text", "text": text}],
        "isError": false
    })
}

fn tool_err(message: impl Into<String>, hint: Option<String>) -> Value {
    let mut text = message.into();
    if let Some(h) = hint {
        text.push_str(" — ");
        text.push_str(&h);
    }
    json!({
        "structuredContent": {"error": text},
        "content": [{"type": "text", "text": text}],
        "isError": true
    })
}

fn auth_required() -> Value {
    let desc = "Link your slug.social account to post or redact.";
    json!({
        "structuredContent": {"error": desc},
        "content": [{"type": "text", "text": desc}],
        "isError": true,
        "_meta": {
            "mcp/www_authenticate": [
                www_authenticate_challenge("insufficient_scope", desc)
            ]
        }
    })
}

fn bearer_from_headers(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = v.strip_prefix("Bearer ").unwrap_or(v).trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

async fn rpc(
    state: &AppState,
    headers: &HeaderMap,
    cmd: RpcCommand,
) -> Result<RpcResult, (String, Option<String>)> {
    let line = dispatch_rpc(state, headers, cmd).await;
    if line.ok {
        line.result.ok_or_else(|| ("empty result".into(), None))
    } else {
        Err((line.error.unwrap_or_else(|| "rpc failed".into()), line.hint))
    }
}

fn thread_url(tag: &str) -> String {
    ForumThreadUrl::from_room_tag("public", tag.trim_start_matches('#')).into_inner()
}

fn search_results(resp: SearchResponse) -> Value {
    let mut results = Vec::new();
    for item in resp.items {
        let url = item.path.as_str().to_string();
        results.push(json!({
            "id": format!("item:{url}"),
            "title": url,
            "url": url
        }));
    }
    for th in resp.threads {
        let tag = th.tag.trim_start_matches('#');
        let url = thread_url(tag);
        results.push(json!({
            "id": format!("thread:{tag}"),
            "title": format!("#{}", tag),
            "url": url
        }));
    }
    for post in resp.posts {
        let tag = post.thread.trim_start_matches('#');
        let url = thread_url(tag);
        let id = if post.id.is_empty() {
            format!("thread:{tag}")
        } else {
            format!("post:{}", post.id)
        };
        let title = post.snippet.lines().next().unwrap_or("post").trim();
        results.push(json!({
            "id": id,
            "title": title,
            "url": url
        }));
    }
    json!({"results": results})
}

async fn tools_call(state: &AppState, headers: &HeaderMap, params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "search" => {
            let Some(query) = arg_string(&args, "query") else {
                return tool_err("query is required", None);
            };
            match rpc(state, headers, RpcCommand::Search { query }).await {
                Ok(RpcResult::Search(resp)) => {
                    let structured = search_results(resp);
                    let n = structured["results"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0);
                    tool_ok(structured, format!("Found {n} results."))
                }
                Ok(_) => tool_err("unexpected search result", None),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "fetch" => fetch_doc(state, headers, &args).await,
        "list_threads" => {
            match rpc(
                state,
                headers,
                RpcCommand::ListForumThreads {
                    room: "public".into(),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "threads"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "get_thread" => {
            let Some(thread_tag) = arg_string(&args, "thread_tag") else {
                return tool_err("thread_tag is required", None);
            };
            match rpc(
                state,
                headers,
                RpcCommand::GetForumThread {
                    room: "public".into(),
                    thread_tag,
                    offset: arg_usize(&args, "offset"),
                    limit: arg_usize(&args, "limit"),
                    since: None,
                    before: None,
                    actor: None,
                    post_id: arg_string(&args, "post_id"),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "thread"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "get_rank" => {
            match rpc(
                state,
                headers,
                RpcCommand::GetGardenRank {
                    room: "public".into(),
                    parent_path: arg_string(&args, "parent_path").unwrap_or_else(|| "~".into()),
                    depth: arg_usize(&args, "depth"),
                    offset: arg_usize(&args, "offset"),
                    limit: arg_usize(&args, "limit"),
                    percent: arg_bool(&args, "percent"),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "rank"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "get_item" => {
            let Some(item_path) = arg_string(&args, "item_path") else {
                return tool_err("item_path is required", None);
            };
            match rpc(
                state,
                headers,
                RpcCommand::GetGardenItem {
                    room: "public".into(),
                    item_path,
                    full: arg_bool(&args, "full"),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "item"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "get_pair" => {
            match rpc(
                state,
                headers,
                RpcCommand::GetPair {
                    room: "public".into(),
                    parent_path: arg_string(&args, "parent_path").unwrap_or_else(|| "~".into()),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "pair"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "check_sorter" => {
            let Some(text) = arg_string(&args, "text") else {
                return tool_err("text is required", None);
            };
            match rpc(
                state,
                headers,
                RpcCommand::Check {
                    room: "public".into(),
                    text,
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "check"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "post_sorter" => {
            if bearer_from_headers(headers).is_none() {
                return auth_required();
            }
            let Some(thread_tag) = arg_string(&args, "thread_tag") else {
                return tool_err("thread_tag is required", None);
            };
            let Some(text) = arg_string(&args, "text") else {
                return tool_err("text is required", None);
            };
            match rpc(
                state,
                headers,
                RpcCommand::Post {
                    room: "public".into(),
                    thread_tag,
                    delegate: None,
                    text,
                    return_rank_diff: true,
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "posted"),
                Err((e, h)) => {
                    if e.contains("Authorization") || e.contains("token") || e.contains("Bearer") {
                        auth_required()
                    } else {
                        tool_err(e, h)
                    }
                }
            }
        }
        "redact_post" => {
            if bearer_from_headers(headers).is_none() {
                return auth_required();
            }
            let Some(post_id) = arg_string(&args, "post_id") else {
                return tool_err("post_id is required", None);
            };
            match rpc(state, headers, RpcCommand::PostRedact { post_id }).await {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "redacted"),
                Err((e, h)) => {
                    if e.contains("Authorization") || e.contains("token") || e.contains("Bearer") {
                        auth_required()
                    } else {
                        tool_err(e, h)
                    }
                }
            }
        }
        "" => tool_err("tool name is required", None),
        other => tool_err(format!("unknown tool: {other}"), None),
    }
}

async fn fetch_doc(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    let Some(raw_id) = arg_string(args, "id") else {
        return tool_err("id is required", None);
    };
    let id = if raw_id.starts_with("item:")
        || raw_id.starts_with("thread:")
        || raw_id.starts_with("post:")
    {
        raw_id
    } else if raw_id.contains('/') || raw_id.starts_with('~') || raw_id.starts_with("http") {
        format!("item:{raw_id}")
    } else {
        format!("thread:{raw_id}")
    };
    if let Some(path) = id.strip_prefix("item:") {
        match rpc(
            state,
            headers,
            RpcCommand::GetGardenItem {
                room: "public".into(),
                item_path: path.to_string(),
                full: Some(true),
            },
        )
        .await
        {
            Ok(RpcResult::GardenItem(item)) => {
                let url = item.item.as_str().to_string();
                let text = item.body.clone().unwrap_or_default();
                let structured = json!({
                    "id": id,
                    "title": url,
                    "text": text,
                    "url": url,
                    "metadata": {"threads": item.threads, "truncated": item.truncated}
                });
                tool_ok(structured, "item")
            }
            Ok(_) => tool_err("unexpected item result", None),
            Err((e, h)) => tool_err(e, h),
        }
    } else if let Some(tag) = id.strip_prefix("thread:") {
        match rpc(
            state,
            headers,
            RpcCommand::GetForumThread {
                room: "public".into(),
                thread_tag: tag.to_string(),
                offset: Some(0),
                limit: Some(50),
                since: None,
                before: None,
                actor: None,
                post_id: None,
            },
        )
        .await
        {
            Ok(RpcResult::ForumThread(th)) => {
                let url = thread_url(tag);
                let text = serde_json::to_string_pretty(&th).unwrap_or_default();
                let structured = json!({
                    "id": id,
                    "title": format!("#{}", tag.trim_start_matches('#')),
                    "text": text,
                    "url": url,
                    "metadata": {"total": th.total}
                });
                tool_ok(structured, "thread")
            }
            Ok(_) => tool_err("unexpected thread result", None),
            Err((e, h)) => tool_err(e, h),
        }
    } else if let Some(post_id) = id.strip_prefix("post:") {
        let thread_tag = {
            let reduced = state.reduced.read().await;
            reduced
                .ingests_by_id
                .get(post_id)
                .map(|ing| ing.thread_tag.clone())
        };
        let Some(thread_tag) = thread_tag else {
            return tool_err("post not found", None);
        };
        match rpc(
            state,
            headers,
            RpcCommand::GetForumThread {
                room: "public".into(),
                thread_tag,
                offset: None,
                limit: None,
                since: None,
                before: None,
                actor: None,
                post_id: Some(post_id.to_string()),
            },
        )
        .await
        {
            Ok(RpcResult::ForumThread(th)) => {
                let url = thread_url(th.thread.trim_start_matches('#'));
                let text = match th.items.first() {
                    Some(ThreadItem::Post { body, .. }) => body.clone(),
                    Some(ThreadItem::System { text, .. }) => text.clone(),
                    None => String::new(),
                };
                let structured = json!({
                    "id": id,
                    "title": th.thread,
                    "text": text,
                    "url": url,
                    "metadata": {"thread": th.thread}
                });
                tool_ok(structured, "post")
            }
            Ok(_) => tool_err("unexpected post result", None),
            Err((e, h)) => tool_err(e, h),
        }
    } else {
        tool_err(format!("unknown fetch id: {id}"), None)
    }
}

pub fn mcp_routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/mcp",
            post(mcp_post)
                .get(mcp_get)
                .delete(mcp_delete)
                .options(mcp_options),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::oauth_protected_resource).options(mcp_options),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::oauth_authorization_server).options(mcp_options),
        )
        .route(
            "/.well-known/openid-configuration",
            get(oauth::oauth_authorization_server).options(mcp_options),
        )
        .route(
            "/.well-known/openai-apps-challenge",
            get(oauth::openai_apps_challenge),
        )
        .route("/oauth/authorize", get(oauth::oauth_authorize))
        .route("/oauth/token", post(oauth::oauth_token))
}
