use slugsocial_server::{
    event_log::EventLog,
    state::{AppConfig, AppState, KeyRecord},
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use std::net::SocketAddr;

async fn create_test_server() -> (SocketAddr, TempDir, EventLog, tokio::task::JoinHandle<()>) {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("events.jsonl");
    let log = EventLog::new(&log_path);

    let cfg = AppConfig {
        data_dir: tmp.path().to_string_lossy().to_string(),
        event_log_path: log_path.to_string_lossy().to_string(),
        keys: vec![KeyRecord {
            id: "test-key".to_string(),
            secret: "test-secret".to_string(),
        }],
        rate_limit_per_minute: 1000,
    };

    let state = AppState::new(cfg);
    let app = slugsocial_server::create_app(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    (addr, tmp, log, handle)
}

#[tokio::test]
async fn test_healthz() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let response = client.get(&format!("http://{}/healthz", addr)).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_index_page() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let response = client.get(&format!("http://{}/", addr)).send().await.unwrap();
    assert!(response.status().is_success());
    let body = response.text().await.unwrap();
    assert!(body.contains("<html"), "should contain HTML");
    assert!(body.contains("slug.social"), "should contain slug.social");
}

#[tokio::test]
async fn test_ingest_actor_with_colons_is_detected_and_validated() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Old archive style: actor includes colons but UUID is only a prefix (invalid).
    // We should detect the actor line, then fail with "invalid actor format" (not "missing actor").
    let ingest_payload = serde_json::json!({
        "text": "@aec1e31c:claudecode:anthropic/claude-sonnet-4.5\n/a {x}\n",
    });

    let response = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "invalid actor format");
    let hint = body["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("Invalid UUID"),
        "hint should mention invalid UUID, got: {hint}"
    );
}

#[tokio::test]
async fn test_vote_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // /api/v0/vote was removed; all votes are submitted via ingest.
    let ingest_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n/clap {cli parser}\n/argh {cli parser}\n/clap 3:1 /argh {because clap is more full-featured}\n",
    });

    let response = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert!(body["next"].is_object());
}

#[tokio::test]
async fn test_rank_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Ingest items + vote (vote endpoint removed).
    let ingest_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n/rust {systems}\n/go {concurrency}\n/rust 3:1 /go {because i prefer rust for systems work}\n",
    });
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();

    // Then query ranking
    let response = client
        .get(&format!("http://{}/api/v0/rank", addr))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["components"].is_array());
    let components = body["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    let ranking = components[0]["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 2);
    assert_eq!(ranking[0]["item"], "/rust");
    assert!(body["unranked_items"].is_array());
}

#[tokio::test]
async fn test_check_endpoint_does_not_commit() {
    let (addr, tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let check_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n",
    });
    let resp = client
        .post(&format!("http://{}/api/v0/check", addr))
        .json(&check_payload)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // It should not write events.jsonl.
    let log_path = tmp.path().join("events.jsonl");
    let content = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(content.trim().is_empty(), "check must not append events");

    // And the live state should still have no threads (check is dry-run).
    let threads_resp = client
        .get(&format!("http://{}/api/v0/threads", addr))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = threads_resp.json().await.unwrap();
    assert!(body["threads"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_ontology_item_page_shows_body_children_and_collapsible_votes() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n\
/topic {topic body}\n\
/topic/a {alpha body}\n\
/topic/b {beta body}\n\
/topic/c {gamma body}\n\
/other/x {x body}\n\
/other/y {y body}\n\
/topic/a 4:1 /topic/b {a over b}\n\
/other/x 2:1 /other/y {other pair}\n",
    });

    let ingest_response = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();
    assert!(ingest_response.status().is_success());

    let item_html = client
        .get(&format!("http://{}/~/topic/a", addr))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(item_html.contains("ont-item-content"));
    assert!(item_html.contains("#1 of 2"));
    assert!(item_html.contains("alpha body"));
    assert!(item_html.contains("ranked child groups"));
    assert!(item_html.contains("ont-related-votes"));
    assert!(item_html.contains("a over b"));
    assert!(!item_html.contains("other pair"));
}

