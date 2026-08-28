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
use slug_types::paths::{room_id_from_route_segment, ForumThreadUrl};
use slug_types::*;

use crate::{
    api::{dispatch_rpc, now_ms, public_url, verify_bearer_principal},
    identity::parse_agent,
    state::{AppState, PendingSession},
};

use self::oauth::{cors_headers, www_authenticate_challenge};

const SERVER_NAME: &str = "slug-social";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const INSTRUCTIONS: &str = "\
Slug is a garden (leaf-identity ontology + weighted containment + pairwise rank \
centrality) and a forum (bump-ordered threads), including private rooms. Garden \
items are flat tilde tokens (`~luke`); `~/x/luke` is the same item as `~luke`. \
A scope is any item with active members — its body is the prompt for that role. \
`{ explanation }` then `~a <: ~b` claims membership; `~a !<: ~b` raises a border \
(suspended when containment <= border). `==` does not exist. Nested paths in \
.sorter files are sugar for those claims. After the human links their \
account: call whoami, then list_rooms, then read_room(room_id) to catch up on \
private-room threads and recent posts (each post has actor and delegate). \
get_thread(room_id, thread_tag) reads one private thread. get_feed is the \
continuity primitive: activity since this conversation's delegate (or the \
linked human) last posted. get_matchup is the evidence trail for one garden \
item (wins/losses and the thread behind each vote). Public garden/forum also \
work anonymously via search/fetch when room_id is omitted or public. Before \
posting: call identity_start to mint a fresh uuid:rig:provider/model for this \
chat (do not invent a UUID), ask the human for their view, draft a .sorter \
document, call check_sorter, then post_sorter with that delegate. The server \
binds a delegate to the first linked human who uses it. create_room only \
creates private rooms. Cite url fields.";

const MEMBER_CAPS: &[&str] = &["view", "post", "vote", "add_item"];

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

fn oauth_or_anon() -> Value {
    json!([
        {"type": "oauth2", "scopes": ["slug.read"]},
        {"type": "noauth"}
    ])
}

fn oauth_read() -> Value {
    json!([{"type": "oauth2", "scopes": ["slug.read"]}])
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

fn room_id_schema() -> Value {
    json!({
        "type": "string",
        "description": "Room id: \"public\" (default) or a private room id from create_room / list_rooms (shortid/slug)."
    })
}

fn search_result_schema() -> Value {
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
                        "url": {"type": "string"},
                        "actor": {"type": ["string", "null"]},
                        "delegate": {"type": ["string", "null"]}
                    },
                    "required": ["id", "title", "url"]
                }
            }
        },
        "required": ["results"]
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            tool(
                "whoami",
                "Linked identity",
                "Return the linked human username and every delegate already bound to that human. To mint a fresh conversation-bound delegate, call identity_start.",
                json!({"type": "object", "properties": {}}),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_read(),
            ),
            tool(
                "identity_start",
                "Mint a conversation delegate",
                "Mint a fresh delegate (uuid:rig:provider/model) for this chat. If the human is already linked, returns the delegate immediately — pass it on every post_sorter and get_feed. If not linked, returns a Google login_url and session; show the URL, then call identity_poll. Do not invent a UUID.",
                json!({
                    "type": "object",
                    "properties": {
                        "rig": {"type": "string", "description": "Rig name, e.g. cursor or claude"},
                        "model": {"type": "string", "description": "provider/model, e.g. anthropic/claude-sonnet-4.5"}
                    },
                    "required": ["rig", "model"]
                }),
                json!({"type": "object"}),
                annotations(false, false, false),
                oauth_or_anon(),
            ),
            tool(
                "identity_poll",
                "Poll identity login",
                "After identity_start without a linked account, poll the session until the human finishes Google login. Returns the minted delegate when complete.",
                json!({
                    "type": "object",
                    "properties": {
                        "session": {"type": "string", "description": "Session id from identity_start"}
                    },
                    "required": ["session"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "list_rooms",
                "List private rooms",
                "Authenticated. List every private room the linked human can access. Then call read_room with a room_id to open it.",
                json!({"type": "object", "properties": {}}),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_read(),
            ),
            tool(
                "read_room",
                "Read a private room",
                "Authenticated. Open one private room: members, bump-ordered threads, and recent posts with actor/delegate provenance. Use room_id from list_rooms.",
                json!({
                    "type": "object",
                    "properties": {
                        "room_id": {"type": "string", "description": "Private room id (shortid/slug) from list_rooms"}
                    },
                    "required": ["room_id"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_read(),
            ),
            tool(
                "get_feed",
                "Catch-up feed",
                "Authenticated. Posts since this conversation's delegate last posted (same uuid:rig:provider/model as post_sorter). Omit delegate for principal-wide catch-up from the linked human's last ingest. Optional since (unix ms) overrides the server cutoff. Optional room_id limits to one room. Each post includes actor and delegate.",
                json!({
                    "type": "object",
                    "properties": {
                        "delegate": {
                            "type": "string",
                            "description": "Conversation delegate (uuid:rig:provider/model). Same string as post_sorter."
                        },
                        "room_id": room_id_schema(),
                        "since": {"type": "integer", "description": "Override cutoff (unix ms). Default: last ingest for the delegate or principal."},
                        "limit": {"type": "integer", "description": "Max posts (default 10)"}
                    }
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_read(),
            ),
            tool(
                "search",
                "Search slug",
                "Search garden items, forum threads, and posts. With a linked account, omit room_id to include every post the human can view (including private rooms). Set room_id to search one private room. Anonymous calls only see public.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"},
                        "room_id": room_id_schema()
                    },
                    "required": ["query"]
                }),
                search_result_schema(),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "fetch",
                "Fetch slug document",
                "Open one search hit by id (item:, thread:, or post:). For private-room hits pass room_id and a linked account. Returns full text, a citation URL, and actor/delegate provenance when the document is a post.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Id from search, e.g. item:~/languages/rust or post:<uuid>"},
                        "room_id": room_id_schema()
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
                        "metadata": {"type": "object"},
                        "actor": {"type": ["string", "null"]},
                        "delegate": {"type": ["string", "null"]}
                    },
                    "required": ["id", "title", "text", "url"]
                }),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "list_threads",
                "List forum threads",
                "List recently active forum threads (bump-ordered). Pass room_id from list_rooms to list a private room (linked account required). Omit room_id or use public for the public forum.",
                json!({
                    "type": "object",
                    "properties": { "room_id": room_id_schema() }
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "get_thread",
                "Read a forum thread",
                "Read a forum thread page. For a private room pass room_id from list_rooms (linked account required). Use the tag without #. Each post includes actor and delegate.",
                json!({
                    "type": "object",
                    "properties": {
                        "thread_tag": {"type": "string"},
                        "room_id": room_id_schema(),
                        "offset": {"type": "integer"},
                        "limit": {"type": "integer"},
                        "post_id": {"type": "string"}
                    },
                    "required": ["thread_tag"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "get_rank",
                "Garden ranking",
                "Ranked active members of a garden item used as a scope (leaf identity: ~/x/luke and ~luke are the same; default parent is ~, the root electorate).",
                json!({
                    "type": "object",
                    "properties": {
                        "parent_path": {"type": "string", "description": "Garden scope (leaf or legacy nested path; resolved to leaf). Default ~"},
                        "room_id": room_id_schema(),
                        "depth": {"type": "integer"},
                        "offset": {"type": "integer"},
                        "limit": {"type": "integer"},
                        "percent": {"type": "boolean"},
                        "aspect": {"type": "string", "description": "Optional aspect slug; omit for the canonical ranking"}
                    }
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "get_item",
                "Garden item",
                "Item body plus related thread tags. Identity is the leaf (`~luke` / `~/luke`); nested `~/x/luke` resolves to the same item. When the item has active members, the body is that scope's prompt.",
                json!({
                    "type": "object",
                    "properties": {
                        "item_path": {"type": "string"},
                        "room_id": room_id_schema(),
                        "full": {"type": "boolean"}
                    },
                    "required": ["item_path"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "get_pair",
                "Next comparison pair",
                "Suggest the next pairwise comparison under a garden parent path.",
                json!({
                    "type": "object",
                    "properties": {
                        "parent_path": {"type": "string", "description": "Garden parent, default ~"},
                        "room_id": room_id_schema()
                    }
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "get_matchup",
                "Item vote history",
                "Evidence trail for one garden item (leaf identity): each vote (win/loss ratio, opponent, actor) and the forum thread that recorded it. Same as CLI `garden matchup`. `item_path` accepts `luke`, `~luke`, or legacy `~/x/luke`.",
                json!({
                    "type": "object",
                    "properties": {
                        "item_path": {"type": "string", "description": "Garden item leaf, e.g. insertion or ~/insertion"},
                        "room_id": room_id_schema(),
                        "limit": {"type": "integer"}
                    },
                    "required": ["item_path"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "check_sorter",
                "Dry-run a .sorter document",
                "Parse and preview ranking and containment effects of a .sorter document without writing. Items are `~name` (or `~/path` sugar). Claims require a leading { explanation } then `~a <: ~b` / `~a !<: ~b`. `==` is not a statement.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "Full .sorter document"},
                        "room_id": room_id_schema()
                    },
                    "required": ["text"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_or_anon(),
            ),
            tool(
                "create_room",
                "Create a private room",
                "Create a private room owned by the linked human. visibility must be \"private\". Optional members are extra usernames granted view/post/vote/add_item (not manage).",
                json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Room name / slug (lowercase alphanumeric and hyphens)"},
                        "visibility": {"type": "string", "enum": ["private"], "description": "Only private rooms can be created"},
                        "members": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Usernames to grant view/post/vote/add_item"
                        }
                    },
                    "required": ["name", "visibility"]
                }),
                json!({"type": "object"}),
                annotations(false, false, false),
                oauth_write(),
            ),
            tool(
                "grant_room",
                "Grant room access",
                "Grant capabilities in a private room you manage. Default capabilities are view, post, vote, add_item.",
                json!({
                    "type": "object",
                    "properties": {
                        "room_id": {"type": "string"},
                        "username": {"type": "string"},
                        "capabilities": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "view, post, vote, add_item, manage"
                        }
                    },
                    "required": ["room_id", "username"]
                }),
                json!({"type": "object"}),
                annotations(false, false, false),
                oauth_write(),
            ),
            tool(
                "audit_room",
                "Audit room members",
                "List principals and capabilities in a private room.",
                json!({
                    "type": "object",
                    "properties": {
                        "room_id": {"type": "string"}
                    },
                    "required": ["room_id"]
                }),
                json!({"type": "object"}),
                annotations(true, false, false),
                oauth_read(),
            ),
            tool(
                "post_sorter",
                "Publish a .sorter document",
                "Publish a comparison, item, or containment claim to a forum thread. Items are leaf tokens (`~luke`); nested `~/x/luke` is sugar for the same leaf plus membership edges. `{ explanation }` then `~a <: ~b` / `~a !<: ~b` (no `==`). A scope is any item with active members — its body is the prompt. delegate is required (uuid:rig:provider/model) and is bound to the linked human. Ask the human for the exact delegate; do not invent one. room_id defaults to public. thread_tag is the forum channel inside that room.",
                json!({
                    "type": "object",
                    "properties": {
                        "thread_tag": {"type": "string", "description": "Forum tag without #"},
                        "text": {"type": "string", "description": "Full .sorter document"},
                        "delegate": {
                            "type": "string",
                            "description": "Required agent identity: uuid:rig:provider/model"
                        },
                        "room_id": room_id_schema()
                    },
                    "required": ["thread_tag", "text", "delegate"]
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

fn arg_room(args: &Value) -> String {
    arg_string(args, "room_id")
        .or_else(|| arg_string(args, "room"))
        .unwrap_or_else(|| "public".into())
}

fn arg_string_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn slug_from_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
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
    let desc = "Link your slug.social account to continue.";
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

fn require_bearer(headers: &HeaderMap) -> Option<Value> {
    if bearer_from_headers(headers).is_none() {
        Some(auth_required())
    } else {
        None
    }
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

fn write_rpc_err(e: String, h: Option<String>) -> Value {
    if e.contains("Authorization") || e.contains("token") || e.contains("Bearer") {
        auth_required()
    } else {
        tool_err(e, h)
    }
}

fn thread_url(room: &str, tag: &str) -> String {
    ForumThreadUrl::from_room_tag(room, tag.trim_start_matches('#')).into_inner()
}

fn room_or_public(room: &str) -> &str {
    if room.trim().is_empty() {
        "public"
    } else {
        room
    }
}

fn merge_provenance(mut obj: Value, actor: Option<&str>, delegate: Option<&str>) -> Value {
    if let Some(map) = obj.as_object_mut() {
        map.insert(
            "actor".into(),
            actor.map(Value::from).unwrap_or(Value::Null),
        );
        map.insert(
            "delegate".into(),
            delegate.map(Value::from).unwrap_or(Value::Null),
        );
    }
    obj
}

fn post_item_provenance(item: &ThreadItem) -> (Option<&str>, Option<&str>) {
    match item {
        ThreadItem::Post {
            actor, delegate, ..
        } => (Some(actor.as_str()), delegate.as_deref()),
        ThreadItem::System { .. } => (None, None),
    }
}

fn search_results(resp: SearchResponse) -> Value {
    let mut results = Vec::new();
    for item in resp.items {
        let url = item.path.as_str().to_string();
        results.push(json!({
            "id": format!("item:{url}"),
            "title": url,
            "url": url,
            "actor": Value::Null,
            "delegate": Value::Null
        }));
    }
    for th in resp.threads {
        let tag = th.tag.trim_start_matches('#');
        let room = room_or_public(&th.room);
        let url = thread_url(room, tag);
        results.push(json!({
            "id": format!("thread:{tag}"),
            "title": format!("#{}", tag),
            "url": url,
            "room": room,
            "actor": Value::Null,
            "delegate": Value::Null
        }));
    }
    for post in resp.posts {
        let tag = post
            .thread
            .rsplit('#')
            .next()
            .unwrap_or(post.thread.as_str())
            .trim_start_matches('#');
        let room = room_or_public(&post.room);
        let url = thread_url(room, tag);
        let id = if post.id.is_empty() {
            format!("thread:{tag}")
        } else {
            format!("post:{}", post.id)
        };
        let title = post.snippet.lines().next().unwrap_or("post").trim();
        results.push(json!({
            "id": id,
            "title": title,
            "url": url,
            "room": room,
            "actor": post.actor,
            "delegate": post.delegate
        }));
    }
    json!({"results": results})
}

fn room_from_garden_url(raw: &str) -> Option<String> {
    let path = if let Some(idx) = raw.find("/r/") {
        &raw[idx + 3..]
    } else {
        return None;
    };
    let seg = path.split('/').next().filter(|s| !s.is_empty())?;
    room_id_from_route_segment(seg)
}

async fn linked_principal(state: &AppState, headers: &HeaderMap) -> Result<String, Value> {
    if bearer_from_headers(headers).is_none() {
        return Err(auth_required());
    }
    let reduced = state.reduced.read().await;
    verify_bearer_principal(headers, &reduced).map_err(|_| auth_required())
}

async fn tools_call(state: &AppState, headers: &HeaderMap, params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "whoami" => whoami(state, headers).await,
        "identity_start" => identity_start(state, headers, &args).await,
        "identity_poll" => identity_poll(state, &args).await,
        "search" => {
            let Some(query) = arg_string(&args, "query") else {
                return tool_err("query is required", None);
            };
            let room = arg_string(&args, "room_id").or_else(|| arg_string(&args, "room"));
            match rpc(state, headers, RpcCommand::Search { query, room }).await {
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
                    room: arg_room(&args),
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
                    room: arg_room(&args),
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
                Ok(r) => tool_ok(with_thread_provenance(r), "thread"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "get_rank" => {
            match rpc(
                state,
                headers,
                RpcCommand::GetGardenRank {
                    room: arg_room(&args),
                    parent_path: arg_string(&args, "parent_path").unwrap_or_else(|| "~".into()),
                    depth: arg_usize(&args, "depth"),
                    offset: arg_usize(&args, "offset"),
                    limit: arg_usize(&args, "limit"),
                    percent: arg_bool(&args, "percent"),
                    aspect: arg_string(&args, "aspect"),
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
                    room: arg_room(&args),
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
                    room: arg_room(&args),
                    parent_path: arg_string(&args, "parent_path").unwrap_or_else(|| "~".into()),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "pair"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "get_matchup" => {
            let Some(item_path) = arg_string(&args, "item_path") else {
                return tool_err("item_path is required", None);
            };
            match rpc(
                state,
                headers,
                RpcCommand::GetMatchup {
                    room: arg_room(&args),
                    item_path,
                    limit: arg_usize(&args, "limit"),
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "matchup"),
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
                    room: arg_room(&args),
                    text,
                },
            )
            .await
            {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "check"),
                Err((e, h)) => tool_err(e, h),
            }
        }
        "list_rooms" => {
            if let Some(err) = require_bearer(headers) {
                return err;
            }
            match rpc(state, headers, RpcCommand::RoomList).await {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "rooms"),
                Err((e, h)) => write_rpc_err(e, h),
            }
        }
        "read_room" => read_room(state, headers, &args).await,
        "get_feed" => get_feed(state, headers, &args).await,
        "create_room" => create_room(state, headers, &args).await,
        "grant_room" => grant_room(state, headers, &args).await,
        "audit_room" => {
            if let Some(err) = require_bearer(headers) {
                return err;
            }
            let Some(room_id) = arg_string(&args, "room_id").or_else(|| arg_string(&args, "room"))
            else {
                return tool_err("room_id is required", None);
            };
            match rpc(state, headers, RpcCommand::RoomAudit { room: room_id }).await {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "audit"),
                Err((e, h)) => write_rpc_err(e, h),
            }
        }
        "post_sorter" => post_sorter(state, headers, &args).await,
        "redact_post" => {
            if let Some(err) = require_bearer(headers) {
                return err;
            }
            let Some(post_id) = arg_string(&args, "post_id") else {
                return tool_err("post_id is required", None);
            };
            match rpc(state, headers, RpcCommand::PostRedact { post_id }).await {
                Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "redacted"),
                Err((e, h)) => write_rpc_err(e, h),
            }
        }
        "" => tool_err("tool name is required", None),
        other => tool_err(format!("unknown tool: {other}"), None),
    }
}

async fn whoami(state: &AppState, headers: &HeaderMap) -> Value {
    let user = match linked_principal(state, headers).await {
        Ok(u) => u,
        Err(err) => return err,
    };
    let delegates = {
        let reduced = state.reduced.read().await;
        let mut ds: Vec<String> = reduced
            .agent_bindings
            .iter()
            .filter(|(_, owner)| *owner == &user)
            .map(|(agent, _)| agent.clone())
            .collect();
        ds.sort();
        ds
    };
    tool_ok(
        json!({"user": user, "delegates": delegates}),
        format!("linked as {user}"),
    )
}

fn mint_delegate(rig: &str, model: &str) -> Result<String, Value> {
    let uuid = uuid::Uuid::new_v4();
    let raw = format!("{uuid}:{rig}:{model}");
    parse_agent(&raw).map_err(|msg| tool_err("invalid rig or model", Some(msg)))
}

async fn identity_start(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    let Some(rig) = arg_string(args, "rig") else {
        return tool_err("rig is required", None);
    };
    let Some(model) = arg_string(args, "model") else {
        return tool_err("model is required", None);
    };
    let delegate = match mint_delegate(&rig, &model) {
        Ok(d) => d,
        Err(err) => return err,
    };
    if let Ok(user) = linked_principal(state, headers).await {
        return tool_ok(
            json!({
                "phase": "ready",
                "user": user,
                "delegate": delegate,
                "bound": false,
                "instruction": "Pass this exact delegate on every post_sorter and get_feed in this conversation. The server binds it to the linked human on first post."
            }),
            format!("minted {delegate}"),
        );
    }
    let session = format!("p_{}", uuid::Uuid::new_v4().simple());
    let login_url = format!(
        "{}/auth/login?session={}",
        public_url(),
        urlencoding::encode(&session)
    );
    let poll_url = format!("/api/v0/pending-session/{session}");
    state.pending_sessions.write().await.insert(
        session.clone(),
        PendingSession {
            agent: Some(delegate.clone()),
            created_ts: now_ms(),
            provider: None,
            provider_id: None,
            redeem_invite: None,
            redirect_next: None,
            mcp_oauth: None,
            complete: None,
        },
    );
    tool_ok(
        json!({
            "phase": "present_oauth_url_to_user",
            "delegate": delegate,
            "session": session,
            "login_url": login_url,
            "poll_url": poll_url,
            "instruction": "Show login_url to the human as a clickable link, then immediately call identity_poll with this session."
        }),
        format!("sign in at {login_url}"),
    )
}

async fn identity_poll(state: &AppState, args: &Value) -> Value {
    let Some(session) = arg_string(args, "session") else {
        return tool_err("session is required", None);
    };
    let sessions = state.pending_sessions.read().await;
    let Some(pending) = sessions.get(&session) else {
        return tool_err("unknown session", None);
    };
    match &pending.complete {
        Some((user, _token)) => tool_ok(
            json!({
                "phase": "complete",
                "complete": true,
                "user": user,
                "delegate": pending.agent,
            }),
            format!("linked as {user}"),
        ),
        None => tool_ok(
            json!({
                "phase": "pending",
                "complete": false,
                "delegate": pending.agent,
            }),
            "waiting for the human to finish Google login",
        ),
    }
}

async fn create_room(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    if let Some(err) = require_bearer(headers) {
        return err;
    }
    let visibility = arg_string(args, "visibility").unwrap_or_else(|| "private".into());
    if visibility != "private" {
        return tool_err(
            "only private rooms can be created",
            Some("visibility must be \"private\"; public is the shared site, not a room".into()),
        );
    }
    let Some(name) = arg_string(args, "name").or_else(|| arg_string(args, "slug")) else {
        return tool_err("name is required", None);
    };
    let slug = slug_from_name(&name);
    let members = arg_string_list(args, "members");
    let created = match rpc(state, headers, RpcCommand::RoomCreate { slug }).await {
        Ok(RpcResult::RoomCreated { room_id }) => room_id,
        Ok(_) => return tool_err("unexpected create_room result", None),
        Err((e, h)) => return write_rpc_err(e, h),
    };
    let mut granted = Vec::new();
    let mut grant_errors = Vec::new();
    for username in members {
        match rpc(
            state,
            headers,
            RpcCommand::RoomGrant {
                room: created.clone(),
                username: username.clone(),
                capabilities: MEMBER_CAPS.iter().map(|s| (*s).to_string()).collect(),
            },
        )
        .await
        {
            Ok(_) => granted.push(username),
            Err((e, h)) => {
                let mut msg = format!("{username}: {e}");
                if let Some(hint) = h {
                    msg.push_str(" (");
                    msg.push_str(&hint);
                    msg.push(')');
                }
                grant_errors.push(msg);
            }
        }
    }
    let structured = json!({
        "room_id": created,
        "visibility": "private",
        "members_granted": granted,
        "member_errors": grant_errors
    });
    tool_ok(structured, format!("created private room {created}"))
}

async fn grant_room(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    if let Some(err) = require_bearer(headers) {
        return err;
    }
    let Some(room_id) = arg_string(args, "room_id").or_else(|| arg_string(args, "room")) else {
        return tool_err("room_id is required", None);
    };
    let Some(username) = arg_string(args, "username") else {
        return tool_err("username is required", None);
    };
    let capabilities = {
        let listed = arg_string_list(args, "capabilities");
        if listed.is_empty() {
            MEMBER_CAPS.iter().map(|s| (*s).to_string()).collect()
        } else {
            listed
        }
    };
    match rpc(
        state,
        headers,
        RpcCommand::RoomGrant {
            room: room_id,
            username,
            capabilities,
        },
    )
    .await
    {
        Ok(r) => tool_ok(serde_json::to_value(r).unwrap_or(Value::Null), "granted"),
        Err((e, h)) => write_rpc_err(e, h),
    }
}

async fn read_room(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    if let Some(err) = require_bearer(headers) {
        return err;
    }
    let Some(room_id) = arg_string(args, "room_id").or_else(|| arg_string(args, "room")) else {
        return tool_err("room_id is required", None);
    };
    if room_id == "public" {
        return tool_err(
            "read_room is for private rooms",
            Some("use list_threads / get_thread without room_id for the public forum".into()),
        );
    }
    let members = match rpc(
        state,
        headers,
        RpcCommand::RoomAudit {
            room: room_id.clone(),
        },
    )
    .await
    {
        Ok(RpcResult::RoomAudit(audit)) => serde_json::to_value(audit.grants).unwrap_or(json!([])),
        Ok(_) => json!([]),
        Err((e, h)) => return write_rpc_err(e, h),
    };
    let threads = match rpc(
        state,
        headers,
        RpcCommand::ListForumThreads {
            room: room_id.clone(),
        },
    )
    .await
    {
        Ok(RpcResult::ForumThreads(resp)) => {
            serde_json::to_value(resp.threads).unwrap_or(json!([]))
        }
        Ok(_) => json!([]),
        Err((e, h)) => return write_rpc_err(e, h),
    };
    let recent_posts = match rpc(
        state,
        headers,
        RpcCommand::GetFeed {
            delegate: None,
            since: Some(0),
            limit: Some(40),
        },
    )
    .await
    {
        Ok(RpcResult::Feed(feed)) => {
            feed_posts_json(state, feed.posts.into_iter().filter(|p| p.room == room_id)).await
        }
        Ok(_) => json!([]),
        Err((e, h)) => return write_rpc_err(e, h),
    };
    tool_ok(
        json!({
            "room_id": room_id,
            "visibility": "private",
            "members": members,
            "threads": threads,
            "recent_posts": recent_posts
        }),
        format!("private room {room_id}"),
    )
}

async fn get_feed(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    if let Some(err) = require_bearer(headers) {
        return err;
    }
    let room_filter = arg_string(args, "room_id").or_else(|| arg_string(args, "room"));
    let since = args.get("since").and_then(|v| v.as_i64());
    let limit = Some(arg_usize(args, "limit").unwrap_or(10));
    let delegate = match arg_string(args, "delegate") {
        None => None,
        Some(raw) => match parse_agent(&raw) {
            Ok(d) => Some(d),
            Err(msg) => return tool_err("invalid delegate format", Some(msg)),
        },
    };
    match rpc(
        state,
        headers,
        RpcCommand::GetFeed {
            delegate: delegate.clone(),
            since,
            limit,
        },
    )
    .await
    {
        Ok(RpcResult::Feed(feed)) => {
            let posts = match room_filter.as_deref() {
                Some(room) => {
                    feed_posts_json(state, feed.posts.into_iter().filter(|p| p.room == room)).await
                }
                None => feed_posts_json(state, feed.posts).await,
            };
            tool_ok(
                json!({
                    "posts": posts,
                    "total": feed.total,
                    "since": feed.since,
                    "delegate": feed.delegate,
                    "room": room_filter,
                }),
                "feed",
            )
        }
        Ok(_) => tool_err("unexpected feed result", None),
        Err((e, h)) => write_rpc_err(e, h),
    }
}

async fn feed_posts_json(
    state: &AppState,
    posts: impl IntoIterator<Item = FeedPost>,
) -> Value {
    let reduced = state.reduced.read().await;
    let items: Vec<Value> = posts
        .into_iter()
        .map(|post| {
            let (actor, delegate) = reduced
                .ingests_by_id
                .get(&post.id)
                .map(|ing| (Some(ing.principal.as_str()), ing.delegate.as_deref()))
                .unwrap_or((None, None));
            let tag = post.thread.as_deref().unwrap_or("");
            let url = thread_url(&post.room, tag);
            json!({
                "id": format!("post:{}", post.id),
                "post_id": post.id,
                "room": post.room,
                "thread": tag,
                "ts": post.ts,
                "url": url,
                "text": post.body,
                "actor": actor,
                "delegate": delegate
            })
        })
        .collect();
    Value::Array(items)
}

async fn post_sorter(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    if let Some(err) = require_bearer(headers) {
        return err;
    }
    let Some(thread_tag) = arg_string(args, "thread_tag") else {
        return tool_err("thread_tag is required", None);
    };
    let Some(text) = arg_string(args, "text") else {
        return tool_err("text is required", None);
    };
    let Some(raw_delegate) = arg_string(args, "delegate") else {
        return tool_err(
            "delegate is required",
            Some("pass uuid:rig:provider/model from the human; do not invent a UUID".into()),
        );
    };
    let delegate = match parse_agent(&raw_delegate) {
        Ok(d) => d,
        Err(msg) => return tool_err("invalid delegate format", Some(msg)),
    };
    let room = arg_room(args);
    let actor = match linked_principal(state, headers).await {
        Ok(u) => u,
        Err(err) => return err,
    };
    match rpc(
        state,
        headers,
        RpcCommand::Post {
            room,
            thread_tag,
            delegate: Some(delegate.clone()),
            text,
            return_rank_diff: true,
        },
    )
    .await
    {
        Ok(r) => {
            let mut structured = serde_json::to_value(r).unwrap_or(Value::Null);
            structured = merge_provenance(structured, Some(&actor), Some(&delegate));
            tool_ok(structured, "posted")
        }
        Err((e, h)) => write_rpc_err(e, h),
    }
}

fn with_thread_provenance(result: RpcResult) -> Value {
    let Ok(value) = serde_json::to_value(&result) else {
        return Value::Null;
    };
    let (actor, delegate) = result_first_post(&result)
        .map(|(a, d)| (Some(a), d))
        .unwrap_or((None, None));
    merge_provenance(value, actor, delegate)
}

fn result_first_post(result: &RpcResult) -> Option<(&str, Option<&str>)> {
    match result {
        RpcResult::ForumThread(th) => th.items.iter().find_map(|item| match item {
            ThreadItem::Post {
                actor, delegate, ..
            } => Some((actor.as_str(), delegate.as_deref())),
            _ => None,
        }),
        _ => None,
    }
}

async fn fetch_doc(state: &AppState, headers: &HeaderMap, args: &Value) -> Value {
    let Some(raw_id) = arg_string(args, "id") else {
        return tool_err("id is required", None);
    };
    let explicit_room = arg_string(args, "room_id").or_else(|| arg_string(args, "room"));
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
        let room = explicit_room
            .or_else(|| room_from_garden_url(path))
            .unwrap_or_else(|| "public".into());
        match rpc(
            state,
            headers,
            RpcCommand::GetGardenItem {
                room,
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
                    "actor": Value::Null,
                    "delegate": Value::Null,
                    "metadata": {
                        "threads": item.threads,
                        "truncated": item.truncated,
                        "actor": Value::Null,
                        "delegate": Value::Null
                    }
                });
                tool_ok(structured, "item")
            }
            Ok(_) => tool_err("unexpected item result", None),
            Err((e, h)) => tool_err(e, h),
        }
    } else if let Some(tag) = id.strip_prefix("thread:") {
        let room = explicit_room.unwrap_or_else(|| "public".into());
        match rpc(
            state,
            headers,
            RpcCommand::GetForumThread {
                room: room.clone(),
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
                let url = thread_url(&room, tag);
                let text = serde_json::to_string_pretty(&th).unwrap_or_default();
                let (actor, delegate) = th
                    .items
                    .iter()
                    .find_map(|item| match post_item_provenance(item) {
                        (Some(a), d) => Some((a.to_string(), d.map(str::to_string))),
                        _ => None,
                    })
                    .map(|(a, d)| (Some(a), d))
                    .unwrap_or((None, None));
                let structured = json!({
                    "id": id,
                    "title": format!("#{}", tag.trim_start_matches('#')),
                    "text": text,
                    "url": url,
                    "actor": actor,
                    "delegate": delegate,
                    "metadata": {
                        "total": th.total,
                        "room": room,
                        "actor": actor,
                        "delegate": delegate
                    }
                });
                tool_ok(structured, "thread")
            }
            Ok(_) => tool_err("unexpected thread result", None),
            Err((e, h)) => tool_err(e, h),
        }
    } else if let Some(post_id) = id.strip_prefix("post:") {
        let (thread_tag, room) = {
            let reduced = state.reduced.read().await;
            match reduced.ingests_by_id.get(post_id) {
                Some(ing) => (ing.thread_tag.clone(), ing.room_id.clone()),
                None => return tool_err("post not found", None),
            }
        };
        let room = explicit_room.unwrap_or(room);
        match rpc(
            state,
            headers,
            RpcCommand::GetForumThread {
                room: room.clone(),
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
                let url = thread_url(&room, th.thread.trim_start_matches('#'));
                let (text, actor, delegate) = match th.items.first() {
                    Some(ThreadItem::Post {
                        body,
                        actor,
                        delegate,
                        ..
                    }) => (body.clone(), Some(actor.clone()), delegate.clone()),
                    Some(ThreadItem::System { text, .. }) => (text.clone(), None, None),
                    None => (String::new(), None, None),
                };
                let structured = json!({
                    "id": id,
                    "title": th.thread,
                    "text": text,
                    "url": url,
                    "actor": actor,
                    "delegate": delegate,
                    "metadata": {
                        "thread": th.thread,
                        "room": room,
                        "actor": actor,
                        "delegate": delegate
                    }
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
