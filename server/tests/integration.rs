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
async fn test_vote_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Define items first (required).
    let ingest_payload = serde_json::json!({
        "text": "#rust\n:default\n/clap {cli parser}\n/argh {cli parser}\n",
        "mode": "dsl"
    });
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();

    let vote_payload = serde_json::json!({
        "tag": "#rust",
        "aspect": ":speed",
        "a": "/clap",
        "b": "/argh",
        "ratio": "3:1",
        "body": "because clap is more full-featured"
    });

    let response = client
        .post(&format!("http://{}/api/v0/vote", addr))
        .json(&vote_payload)
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

    // Define items then vote.
    let ingest_payload = serde_json::json!({
        "text": "#langs\n:default\n/rust {systems}\n/go {concurrency}\n",
        "mode": "dsl"
    });
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();

    let vote_payload = serde_json::json!({
        "tag": "#langs",
        "aspect": ":speed",
        "a": "/rust",
        "b": "/go",
        "ratio": "3:1",
        "body": "because i prefer rust for systems work"
    });

    client
        .post(&format!("http://{}/api/v0/vote", addr))
        .json(&vote_payload)
        .send()
        .await
        .unwrap();

    // Then query ranking
    let response = client
        .get(&format!("http://{}/api/v0/rank?tag=%23langs&aspect=%3Aspeed", addr))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["ranking"].is_array());
    let ranking = body["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 2);
    assert_eq!(ranking[0]["item"], "/rust");
}

