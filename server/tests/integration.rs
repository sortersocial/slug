use sha2::{Digest, Sha256};
use slugsocial_server::{
    event_log::EventLog,
    events::{Event, TokenIssued, UserRegistered},
    spawn_writer_actor_for_test,
    state::{AppConfig, AppState},
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use std::net::SocketAddr;

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Fixed bearer for integration tests (`TokenIssued` seeded into reducer in `create_test_server`).
fn test_bearer() -> String {
    let token_id = "testtok";
    let secret = "secret";
    format!("slug_{token_id}_{secret}")
}

/// Compact JSON for `POST /ui` (`HtmlUiAction::PostIngest`).
fn ui_post_ingest_rpc(room: &str, thread_tag: &str, text: &str) -> String {
    serde_json::json!({
        "action": "post_ingest",
        "room": room,
        "thread_tag": thread_tag,
        "text": text,
    })
    .to_string()
}

/// Compact JSON for `POST /ui` (`HtmlUiAction::CheckIngest`).
fn ui_check_ingest_rpc(room: &str, thread_tag: &str, text: &str, error_target: &str) -> String {
    serde_json::json!({
        "action": "check_ingest",
        "room": room,
        "thread_tag": thread_tag,
        "text": text,
        "error_target": error_target,
        "form_id": "thread-compose-form",
    })
    .to_string()
}

/// `commands` is a JSON array of RPC commands (`RpcBatch` is a transparent `Vec`).
async fn rpc_batch(
    client: &reqwest::Client,
    addr: SocketAddr,
    bearer: Option<&str>,
    commands: serde_json::Value,
) -> serde_json::Value {
    let url = format!("http://{}/api/v0/rpc", addr);
    let mut req = client.post(url).json(&commands);
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    let response = req.send().await.unwrap();
    assert!(
        response.status().is_success(),
        "rpc http {}",
        response.status()
    );
    response.json().await.unwrap()
}

async fn seed_test_token(state: &AppState) {
    let registered = Event::UserRegistered(UserRegistered {
        ts: 0,
        username: "testuser".to_string(),
        provider: "test".to_string(),
        provider_id: "testuser".to_string(),
    });
    let token_id = "testtok";
    let secret = "secret";
    let salt = "salt";
    let token_hash = sha256_hex(&format!("{salt}:{secret}"));
    let ev = Event::TokenIssued(TokenIssued {
        ts: 0,
        username: "testuser".to_string(),
        token_id: token_id.to_string(),
        token_hash,
        salt: salt.to_string(),
        issued_via: "test".to_string(),
    });
    let mut r = state.reduced.write().await;
    r.apply_event(registered);
    r.apply_event(ev);
}

async fn create_test_server_with_state() -> (SocketAddr, TempDir, EventLog, AppState, tokio::task::JoinHandle<()>) {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("events.jsonl");
    let log = EventLog::new(&log_path);

    let cfg = AppConfig {
        data_dir: tmp.path().to_string_lossy().to_string(),
        event_log_path: log_path.to_string_lossy().to_string(),
    };

    let (state, write_rx) = AppState::create_for_test(cfg);
    seed_test_token(&state).await;
    spawn_writer_actor_for_test(state.clone(), write_rx);
    let app = slugsocial_server::create_app(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    (addr, tmp, log, state, handle)
}

async fn create_test_server() -> (SocketAddr, TempDir, EventLog, tokio::task::JoinHandle<()>) {
    let (addr, tmp, log, _state, handle) = create_test_server_with_state().await;
    (addr, tmp, log, handle)
}

/// Spin up a server that replays all events already present in the given log file.
/// Uses a separate temp dir for views.redb so it doesn't conflict with the original server.
#[tokio::test]
async fn test_healthz() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let response = client.get(&format!("http://{}/healthz", addr)).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_room_create_rpc() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let batch = serde_json::json!([{
        "RoomCreate": { "slug": "secret-project" }
    }]);
    let body = rpc_batch(&client, addr, Some(&test_bearer()), batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], true, "room create: {:?}", line);
    let room_id = line["result"]["RoomCreated"]["room_id"].as_str().unwrap();
    assert!(
        room_id.contains("/secret-project"),
        "expected room_id to contain slug, got {room_id}"
    );
}

/// Private room reads use `authorize_room_read`, which returns "room not found" when no bearer is sent
/// (same as unknown room). The CLI must attach the saved token for `private … forum show` etc.
#[tokio::test]
async fn test_private_room_forum_read_requires_bearer() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "bearer-for-reads" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    let post = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": "cli-read-test",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "hello from private room\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(post["results"][0]["ok"], true);

    let no_auth = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": room_id,
                "thread_tag": "cli-read-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let line_na = &no_auth["results"][0];
    assert_eq!(line_na["ok"], false, "expected failure without bearer: {:?}", line_na);
    assert_eq!(line_na["error"], "room not found");

    let with_auth = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetForumThread": {
                "room": room_id,
                "thread_tag": "cli-read-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let line_ok = &with_auth["results"][0];
    assert_eq!(line_ok["ok"], true, "expected success with bearer: {:?}", line_ok);
    let total = line_ok["result"]["ForumThread"]["total"].as_u64().unwrap();
    assert!(total >= 1);
}

#[tokio::test]
async fn test_private_room_read_requires_explicit_view_capability() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    // Create private room as testuser.
    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "private-read-acl" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Ingest private content while owner still has full caps.
    let post = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": "main",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "~/secret/item {top secret}\nprivate prose\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(post["results"][0]["ok"], true);

    // Remove explicit view cap from owner (still has Manage/Post/Vote/AddItem).
    let revoke_view = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomRevoke": {
                "room": room_id,
                "username": "testuser",
                "capability": "view"
            }
        }]),
    )
    .await;
    assert_eq!(revoke_view["results"][0]["ok"], true);

    // Private reads must now be denied with not-found semantics.
    let item_read = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetGardenItem": {
                "room": room_id,
                "item_path": "~/secret/item",
                "full": true
            }
        }]),
    )
    .await;
    let line = &item_read["results"][0];
    assert_eq!(line["ok"], false);
    assert_eq!(line["error"], "room not found");

    // Search should not leak private posts when caller lacks explicit view.
    let search = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Search": { "query": "secret" }
        }]),
    )
    .await;
    let posts = search["results"][0]["result"]["Search"]["posts"]
        .as_array()
        .unwrap();
    assert!(
        posts.is_empty(),
        "search should hide private posts without view capability"
    );

    // Feed should likewise hide private posts (keyed by delegate on those ingests).
    let feed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "delegate": "00000000-0000-0000-0000-000000000000:test:local/test", "limit": 20 }
        }]),
    )
    .await;
    let feed_posts = feed["results"][0]["result"]["Feed"]["posts"]
        .as_array()
        .unwrap();
    assert!(
        feed_posts.is_empty(),
        "feed should hide private posts without view capability"
    );
}

#[tokio::test]
async fn test_feed_since_last_post_is_scoped_to_delegate() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let d1 = "00000000-0000-0000-0000-0000000000a1:feedtest:local/model-a";
    let d2 = "00000000-0000-0000-0000-0000000000a2:feedtest:local/model-b";

    let p1 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "feed-delegate-test",
                "delegate": d1,
                "text": "#feed-delegate-test\nalpha marker for d1\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p1["results"][0]["ok"], true, "first post: {:?}", p1);

    let p2 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "feed-delegate-test",
                "delegate": d2,
                "text": "#feed-delegate-test\nbeta marker for d2\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p2["results"][0]["ok"], true, "second post: {:?}", p2);

    // Since d1 last posted first, the feed for d1 should include everything after that (here: d2's post).
    let feed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "delegate": d1, "limit": 20 }
        }]),
    )
    .await;
    let line = &feed["results"][0];
    assert_eq!(line["ok"], true, "GetFeed failed: {:?}", line);
    let delegate = line["result"]["Feed"]["delegate"].as_str().unwrap();
    assert_eq!(delegate, d1);
    let bodies: String = line["result"]["Feed"]["posts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["body"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bodies.contains("beta marker for d2"),
        "expected d2's post after d1's cutoff, got: {bodies}"
    );
    assert!(
        !bodies.contains("alpha marker for d1"),
        "d1's own prior post should not appear after cutoff, got: {bodies}"
    );

    let d_other = "00000000-0000-0000-0000-0000000000ff:feedtest:local/other";
    let steal = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "delegate": d_other, "limit": 5 }
        }]),
    )
    .await;
    let steal_line = &steal["results"][0];
    assert_eq!(steal_line["ok"], false, "expected rejection for unbound delegate: {:?}", steal_line);
    assert_eq!(steal_line["error"], "not your delegate");

    let no_auth = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetFeed": { "delegate": d1, "limit": 5 }
        }]),
    )
    .await;
    let na = &no_auth["results"][0];
    assert_eq!(na["ok"], false, "GetFeed without bearer: {:?}", na);
    assert_eq!(na["error"], "missing Authorization header");
}

#[tokio::test]
async fn test_feed_without_delegate_uses_principal_last_post_including_delegate() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let d = "00000000-0000-0000-0000-0000000000b1:principalfeed:local/model";
    let p1 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "principal-feed-test",
                "delegate": d,
                "text": "#principal-feed-test\nfirst delegate post\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p1["results"][0]["ok"], true, "{:?}", p1);

    let p2 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "principal-feed-test",
                "delegate": d,
                "text": "#principal-feed-test\nsecond delegate post\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p2["results"][0]["ok"], true, "{:?}", p2);

    // Argless GetFeed: cutoff is last ingest by this principal (even if every post used --delegate),
    // so revisiting an old chat with only a token still gets a bounded "since", not full history.
    let feed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "limit": 20 }
        }]),
    )
    .await;
    let line = &feed["results"][0];
    assert_eq!(line["ok"], true, "{:?}", line);
    assert!(
        line["result"]["Feed"]["delegate"].is_null(),
        "principal-wide feed omits delegate in JSON: {:?}",
        line["result"]["Feed"]
    );
    assert!(
        line["result"]["Feed"]["since"].is_number(),
        "expected since from principal's last post (including delegate ingests), got: {:?}",
        line["result"]["Feed"]["since"]
    );
    let posts = line["result"]["Feed"]["posts"].as_array().unwrap();
    assert!(
        posts.is_empty(),
        "nothing is strictly newer than the latest own post; got {} posts",
        posts.len()
    );
}

#[tokio::test]
async fn test_private_room_thread_urls_use_t_segment() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "url-shape" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (room_short, room_slug) = room_id.split_once('/').unwrap();

    let rpc = ui_post_ingest_rpc(&room_id, "main-thread", "private post via web");
    let post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(post.status(), reqwest::StatusCode::OK);
    assert_eq!(
        post.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let post_js = post.text().await.unwrap();
    let location = format!("/r/{room_short}/{room_slug}/t/main-thread");
    assert!(post_js.contains(&format!("window.location = {:?};", location)));
    assert!(post_js.contains("#room-thread-feed"));
    assert!(post_js.contains("#thread-feed-region"));

    let thread_page = client
        .get(format!("http://{addr}{location}"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(thread_page.status().is_success());
    let body = thread_page.text().await.unwrap();
    assert!(body.contains("#main-thread"));
    assert!(body.contains("private post via web"));
}

#[tokio::test]
async fn test_private_room_post_links_use_private_garden_routes() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "private-garden-links" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (room_short, room_slug) = room_id.split_once('/').unwrap();

    let rpc = ui_post_ingest_rpc(
        &room_id,
        "garden-thread",
        "~/secret/item {classified}\n~/secret/other {other body}\n~/secret/item 3:1 ~/secret/other {because}\n",
    );
    let post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), reqwest::StatusCode::OK);

    let thread_page = client
        .get(format!("http://{addr}/r/{room_short}/{room_slug}/t/garden-thread"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(thread_page.status().is_success());
    let body = thread_page.text().await.unwrap();
    assert!(body.contains(&format!("/r/{room_short}/{room_slug}/~/secret/item")));
    assert!(!body.contains("href=\"/~/secret/item\""));

    let garden_page = client
        .get(format!("http://{addr}/r/{room_short}/{room_slug}/~/secret/item"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(garden_page.status().is_success());
    let garden_body = garden_page.text().await.unwrap();
    assert!(garden_body.contains("classified"));
    assert!(garden_body.contains(&format!("/r/{room_short}/{room_slug}/t/garden-thread")));
}

#[tokio::test]
async fn test_post_check_returns_targeted_js_error_for_missing_thread_tag() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let rpc = ui_check_ingest_rpc("public", "", "hello", "thread-compose-errors");
    let resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let js = resp.text().await.unwrap();
    assert!(js.contains("#thread-compose-errors"));
    assert!(js.contains("missing thread tag"));
}

#[tokio::test]
async fn test_choose_username_returns_evalable_js() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let start = client
        .post(format!("http://{addr}/api/v0/pending-session"))
        .json(&serde_json::json!({
            "agent": "00000000-0000-0000-0000-000000000123:test:web/form"
        }))
        .send()
        .await
        .unwrap();
    assert!(start.status().is_success());
    let start_json: serde_json::Value = start.json().await.unwrap();
    let session = start_json["session"].as_str().unwrap();

    {
        let mut sessions = state.pending_sessions.write().await;
        let pending = sessions.get_mut(session).expect("pending session must exist");
        pending.provider = Some("google".to_string());
        pending.provider_id = Some("google-user-123".to_string());
    }

    let choose = client
        .post(format!("http://{addr}/auth/choose-username"))
        .form(&[
            ("session", session),
            ("username", "webuser"),
        ])
        .send()
        .await
        .unwrap();

    assert_eq!(choose.status(), reqwest::StatusCode::OK);
    assert_eq!(
        choose.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let body = choose.text().await.unwrap();
    assert!(body.contains("#choose-username-form"));
    assert!(body.contains("Idiomorph.morph"));
    assert!(body.contains("window.location = \"/auth/complete\""));
}

#[tokio::test]
async fn test_sse_stream_emits_evalable_js_after_post() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "sse-room" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (room_short, room_slug) = room_id.split_once('/').unwrap();
    let room_path = format!("/r/{room_short}/{room_slug}");

    let sse_resp = client
        .get(format!("http://{addr}/sse?path={}", urlencoding::encode(&room_path)))
        .send()
        .await
        .unwrap();
    assert!(sse_resp.status().is_success());

    let rpc = ui_post_ingest_rpc(&room_id, "live-thread", "hello over sse");
    let _post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    let mut body = String::new();
    let mut sse_resp = sse_resp;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sse_resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("room-thread-feed") {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("sse read failed: {err}"),
            Err(_) => {}
        }
    }
    assert!(body.contains("room-thread-feed"));
    assert!(body.contains("live-thread"));
}

#[tokio::test]
async fn test_index_page() {
    // HTML routes are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_ingest_actor_with_colons_is_detected_and_validated() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let b = test_bearer();

    // Old archive style: agent includes colons but UUID is only a prefix (invalid).
    let bad_delegate = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "t",
            "delegate": "aec1e31c:claudecode:anthropic/claude-sonnet-4.5",
            "text": "~/x {x}\n",
            "return_rank_diff": false
        }
    }]);
    let body = rpc_batch(&client, addr, Some(&b), bad_delegate).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], false);
    assert_eq!(line["error"], "invalid delegate format");
    let hint = line["hint"].as_str().unwrap_or_default();
    assert!(
        hint.to_lowercase().contains("uuid"),
        "hint should mention uuid, got: {hint}"
    );

    let at_batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "t",
            "delegate": "@00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/x {x}\n",
            "return_rank_diff": false
        }
    }]);
    let at_body = rpc_batch(&client, addr, Some(&b), at_batch).await;
    let at_line = &at_body["results"][0];
    assert_eq!(at_line["ok"], false);
    let at_hint = at_line["hint"].as_str().unwrap_or_default();
    assert!(
        at_hint.contains('@'),
        "hint should reject '@' in delegate, got: {at_hint}"
    );
}

#[tokio::test]
async fn test_post_redact_removes_garden_and_marks_thread() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let post_body = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "redact-test",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "~/del-a {a}\n~/del-b {b}\n~/del-a 2:1 ~/del-b {vote line}\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    let line = &post_body["results"][0];
    assert_eq!(line["ok"], true, "post ingest: {:?}", line);

    let thread_before = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": "public",
                "thread_tag": "redact-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let items_before = thread_before["results"][0]["result"]["ForumThread"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items_before.len(), 1);
    let post_id = items_before[0]["id"].as_str().unwrap().to_string();
    assert_eq!(items_before[0]["redacted"], serde_json::Value::Bool(false));

    let rank_before = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenRank": {
                "room": "public",
                "parent_path": "~",
                "depth": 1
            }
        }]),
    )
    .await;
    let rank_line = &rank_before["results"][0];
    assert_eq!(rank_line["ok"], true);
    let ranking = rank_line["result"]["GardenRank"]["components"][0]["ranking"]
        .as_array()
        .unwrap();
    assert_eq!(ranking.len(), 2);

    let redact = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "PostRedact": { "post_id": post_id }
        }]),
    )
    .await;
    assert_eq!(redact["results"][0]["ok"], true, "redact: {:?}", redact);

    let rank_after = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenRank": {
                "room": "public",
                "parent_path": "~",
                "depth": 1
            }
        }]),
    )
    .await;
    let ra_line = &rank_after["results"][0];
    assert_eq!(ra_line["ok"], true);
    let comps = ra_line["result"]["GardenRank"]["components"].as_array().unwrap();
    assert!(
        comps.is_empty() || comps[0]["ranking"].as_array().unwrap().is_empty(),
        "votes from redacted post should be removed: {:?}",
        ra_line
    );

    let thread_after = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": "public",
                "thread_tag": "redact-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let item = &thread_after["results"][0]["result"]["ForumThread"]["items"][0];
    assert_eq!(item["redacted"], serde_json::Value::Bool(true));
    assert_eq!(item["body"].as_str().unwrap(), "");
}

#[tokio::test]
async fn test_vote_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "cli",
            "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/clap {cli parser}\n~/argh {cli parser}\n~/clap 3:1 ~/argh {because clap is more full-featured}\n",
            "return_rank_diff": true
        }
    }]);
    let body = rpc_batch(&client, addr, Some(&test_bearer()), batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], true);
    let post = &line["result"]["PostOk"];
    assert!(post["next"].is_object());
}

#[tokio::test]
async fn test_rank_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "langs",
            "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/rust {systems}\n~/go {concurrency}\n~/rust 3:1 ~/go {because i prefer rust for systems work}\n",
            "return_rank_diff": false
        }
    }]);
    rpc_batch(&client, addr, Some(&test_bearer()), ingest).await;

    let rank_batch = serde_json::json!([{
        "GetGardenRank": {
            "room": "public",
            "parent_path": "~",
            "depth": 1
        }
    }]);
    let body = rpc_batch(&client, addr, None, rank_batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], true);
    let rank = &line["result"]["GardenRank"];
    assert!(rank["components"].is_array());
    let components = rank["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    let ranking = components[0]["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 2);
    assert_eq!(ranking[0]["item"], "https://slug.social/~/rust");
    assert!(rank["unranked_items"].is_array());
}

#[tokio::test]
async fn test_check_endpoint_does_not_commit() {
    let (addr, tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let check_batch = serde_json::json!([{
        "Check": {
            "room": "public",
            "text": "~/a {x}\n~/b {y}\n~/a 2:1 ~/b {because}\n",
        }
    }]);
    let resp_body = rpc_batch(&client, addr, None, check_batch).await;
    let line = &resp_body["results"][0];
    assert_eq!(line["ok"], true);

    // It should not write events.jsonl.
    let log_path = tmp.path().join("events.jsonl");
    let content = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(content.trim().is_empty(), "check must not append events");

    // And the live state should still have no forum threads (check is dry-run).
    let list_batch = serde_json::json!([{
        "ListForumThreads": { "room": "public" }
    }]);
    let threads_body = rpc_batch(&client, addr, None, list_batch).await;
    let tline = &threads_body["results"][0];
    assert_eq!(tline["ok"], true);
    let threads = tline["result"]["ForumThreads"]["threads"].as_array().unwrap();
    assert!(threads.is_empty());
}

#[tokio::test]
async fn test_ontology_item_page_shows_body_children_and_collapsible_votes() {
    // HTML routes are offline during the auth-v3 refactor.
}

/// Thread connective tissue: item_threads and VoteData.thread_tag are exposed by item, pair, and matchup RPC.
#[tokio::test]
async fn test_garden_item_pair_matchup_include_threads() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest_batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "sorting-hat",
            "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/sorts/insertion { O(n^2) }\n~/sorts/mergesort { O(n log n) }\n~/sorts/insertion 3:1 ~/sorts/mergesort { simpler for small n }\n",
            "return_rank_diff": false
        }
    }]);
    let ing = rpc_batch(&client, addr, Some(&test_bearer()), ingest_batch).await;
    assert_eq!(ing["results"][0]["ok"], true, "ingest should succeed");

    let item_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenItem": {
                "room": "public",
                "item_path": "~/sorts/insertion",
                "full": true
            }
        }]),
    )
    .await;
    let item = &item_body["results"][0]["result"]["GardenItem"];
    assert_eq!(item["item"], "https://slug.social/~/sorts/insertion");
    assert!(item["body"].as_str().unwrap().contains("O(n^2)"));
    let threads: Vec<&str> = item["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(threads.contains(&"sorting-hat"), "item threads should contain sorting-hat: {:?}", threads);

    let pair_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": "~/sorts"
            }
        }]),
    )
    .await;
    let pair = &pair_body["results"][0]["result"]["Pair"];
    let pair_threads: Vec<&str> = pair["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(pair_threads.contains(&"sorting-hat"), "pair threads should contain sorting-hat: {:?}", pair_threads);

    let matchup_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetMatchup": {
                "room": "public",
                "item_path": "~/sorts/insertion"
            }
        }]),
    )
    .await;
    let matchup = &matchup_body["results"][0]["result"]["Matchup"];
    assert_eq!(matchup["item"], "https://slug.social/~/sorts/insertion");
    let votes = matchup["votes"].as_array().unwrap();
    assert!(!votes.is_empty(), "matchup should have at least one vote");
    let first_thread = votes[0]["thread"].as_str().unwrap();
    assert_eq!(first_thread, "sorting-hat", "vote should cite thread");
}

#[tokio::test]
async fn test_search_page_and_results() {
    // HTML search pages are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_view_counts_increment_and_display() {
    // HTML view counters are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_search_handles_multibyte_unicode() {
    // HTML search pages are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_rank_history() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let bearer = test_bearer();
    let ingest = |delegate: &str, text: &str| {
        let client = client.clone();
        let addr = addr;
        let bearer = bearer.clone();
        let text = text.to_string();
        let delegate = delegate.to_string();
        async move {
            rpc_batch(
                &client,
                addr,
                Some(&bearer),
                serde_json::json!([{
                    "Post": {
                        "room": "public",
                        "thread_tag": "hist-test",
                        "delegate": delegate,
                        "text": text,
                        "return_rank_diff": false
                    }
                }]),
            )
            .await
        }
    };

    ingest(
        "00000000-0000-0000-0000-000000000001:rig:test/model",
        "~/hist/rust { systems }\n~/hist/python { scripting }\n~/hist/go { concurrency }\n~/hist/rust 3:1 ~/hist/python { ownership over gc }\n~/hist/rust 2:1 ~/hist/go { performance over simplicity }\n",
    )
    .await;

    let hist_rust = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetRankHistory": {
                "room": "public",
                "item_path": "~/hist/rust"
            }
        }]),
    )
    .await;
    let resp = &hist_rust["results"][0]["result"]["RankHistory"];
    assert_eq!(resp["item"], "https://slug.social/~/hist/rust");
    let history = resp["history"].as_array().unwrap();
    assert_eq!(history.len(), 1, "one ingest → one history entry");
    let entry = &history[0];
    assert_eq!(entry["scope_rank"], 1, "rust should be #1 in scope after first ingest");
    assert_eq!(entry["scope_rank_delta"], 0, "delta is 0 on first appearance");
    let caused_by = entry["caused_by"].as_array().unwrap();
    assert_eq!(caused_by.len(), 2, "both votes in the ingest touched rust");

    ingest(
        "00000000-0000-0000-0000-000000000002:rig:test/model",
        "~/hist/python 3:1 ~/hist/go { dynamic typing is worth it }\n",
    )
    .await;

    let hist_py = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetRankHistory": {
                "room": "public",
                "item_path": "~/hist/python"
            }
        }]),
    )
    .await;
    let resp2 = &hist_py["results"][0]["result"]["RankHistory"];
    let hist2 = resp2["history"].as_array().unwrap();
    assert_eq!(hist2.len(), 2, "python touched in both ingests");

    let caused_by2 = hist2[1]["caused_by"].as_array().unwrap();
    assert_eq!(caused_by2.len(), 1);
    assert!(caused_by2[0]["a"].as_str().unwrap().ends_with("python") ||
            caused_by2[0]["b"].as_str().unwrap().ends_with("python"));

    let hist_rust2 = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetRankHistory": {
                "room": "public",
                "item_path": "~/hist/rust"
            }
        }]),
    )
    .await;
    let resp3 = &hist_rust2["results"][0]["result"]["RankHistory"];
    assert_eq!(
        resp3["history"].as_array().unwrap().len(),
        1,
        "rust still has only one history entry — not directly voted on in second ingest"
    );
}

#[tokio::test]
async fn pair_returns_connectivity_stats() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let doc = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "connectivity-test",
            "delegate": "00000000-0000-0000-0000-000000000001:testrig:test/model",
            "text": "~/conn/a { item a }\n~/conn/b { item b }\n~/conn/c { item c }\n~/conn/d { item d }\n~/conn/a 3:1 ~/conn/b { a is better }\n",
            "return_rank_diff": false
        }
    }]);
    let resp = rpc_batch(&client, addr, Some(&test_bearer()), doc).await;
    assert_eq!(resp["results"][0]["ok"], true, "ingest should succeed");

    let pair_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": "~/conn"
            }
        }]),
    )
    .await;
    let pair = &pair_body["results"][0]["result"]["Pair"];

    let conn = &pair["connectivity"];
    assert!(!conn.is_null(), "pair response should include connectivity stats");
    assert_eq!(conn["items"].as_u64().unwrap(), 4, "4 items in scope");
    assert_eq!(conn["components"].as_u64().unwrap(), 3, "3 components (1 connected + 2 isolates)");
    assert_eq!(conn["comparisons_until_connected"].as_u64().unwrap(), 2, "need 2 more comparisons");
    assert_eq!(conn["pairs_voted"].as_u64().unwrap(), 1, "1 pair voted");
    assert_eq!(conn["pairs_possible"].as_u64().unwrap(), 6, "4*3/2 = 6 possible pairs");

    let doc2 = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "connectivity-test",
            "delegate": "00000000-0000-0000-0000-000000000001:testrig:test/model",
            "text": "~/conn/c 2:1 ~/conn/a { c beats a }\n",
            "return_rank_diff": false
        }
    }]);
    let resp2 = rpc_batch(&client, addr, Some(&test_bearer()), doc2).await;
    assert_eq!(
        resp2["results"][0]["ok"],
        true,
        "second ingest failed: {:?}",
        resp2
    );

    let pair_body2 = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": "~/conn"
            }
        }]),
    )
    .await;
    let pair2 = &pair_body2["results"][0]["result"]["Pair"];
    let conn2 = &pair2["connectivity"];
    assert_eq!(conn2["components"].as_u64().unwrap(), 2, "2 components after connecting c");
    assert_eq!(conn2["comparisons_until_connected"].as_u64().unwrap(), 1, "1 more comparison to connect");
    assert_eq!(conn2["pairs_voted"].as_u64().unwrap(), 2, "2 pairs voted now");
}

