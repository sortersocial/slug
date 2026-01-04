use axum_test::TestServer;
use slugsocial_server::{
    api,
    event_log::EventLog,
    html,
    state::{AppConfig, AppState, KeyRecord},
};
use tempfile::TempDir;
use tower::ServiceBuilder;

async fn create_test_server() -> (TestServer, TempDir, EventLog) {
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
    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/", axum::routing::get(html::index))
        .route("/t/:tag", axum::routing::get(html::tag_page))
        .route("/t/:tag/a/:aspect", axum::routing::get(html::tag_aspect_page))
        .route("/api/v0/vote", axum::routing::post(api::post_vote))
        .route("/api/v0/rank", axum::routing::get(api::get_rank))
        .with_state(state)
        .layer(ServiceBuilder::new().into_inner());

    let server = TestServer::new(app).unwrap();
    (server, tmp, log)
}

#[tokio::test]
async fn test_healthz() {
    let (server, _tmp, _log) = create_test_server().await;
    let response = server.get("/healthz").await;
    response.assert_status_ok();
    response.assert_text("ok");
}

#[tokio::test]
async fn test_index_page() {
    let (server, _tmp, _log) = create_test_server().await;
    let response = server.get("/").await;
    response.assert_status_ok();
    let body = response.text();
    assert!(body.contains("<html"), "should contain HTML");
    assert!(body.contains("Slug"), "should contain Slug");
}

#[tokio::test]
async fn test_vote_endpoint() {
    let (server, _tmp, _log) = create_test_server().await;

    let vote_payload = serde_json::json!({
        "tag": "#rust",
        "aspect": ":speed",
        "a": "/clap",
        "b": "/argh",
        "score": 30
    });

    let response = server
        .post("/api/v0/vote")
        .add_header("Authorization", "Bearer test-secret")
        .json(&vote_payload)
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["status"], "ok");
    assert!(body["next_moves"].is_object());
}

#[tokio::test]
async fn test_vote_requires_auth() {
    let (server, _tmp, _log) = create_test_server().await;

    let vote_payload = serde_json::json!({
        "tag": "#rust",
        "aspect": ":speed",
        "a": "/clap",
        "b": "/argh",
        "score": 30
    });

    let response = server.post("/api/v0/vote").json(&vote_payload).await;
    response.assert_status(401);
}

#[tokio::test]
async fn test_rank_endpoint() {
    let (server, _tmp, _log) = create_test_server().await;

    // First add a vote
    let vote_payload = serde_json::json!({
        "tag": "#langs",
        "aspect": ":speed",
        "a": "/rust",
        "b": "/go",
        "score": 40
    });

    server
        .post("/api/v0/vote")
        .add_header("Authorization", "Bearer test-secret")
        .json(&vote_payload)
        .await;

    // Then query ranking
    let response = server
        .get("/api/v0/rank?tag=%23langs&aspect=%3Aspeed")
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["ranking"].is_array());
    let ranking = body["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 2);
    assert_eq!(ranking[0]["item"], "rust");
}

#[tokio::test]
async fn test_tag_page() {
    let (server, _tmp, _log) = create_test_server().await;

    // Add a vote to create a tag
    let vote_payload = serde_json::json!({
        "tag": "#test-tag",
        "aspect": ":quality",
        "a": "/item-a",
        "b": "/item-b",
        "score": 20
    });

    server
        .post("/api/v0/vote")
        .add_header("Authorization", "Bearer test-secret")
        .json(&vote_payload)
        .await;

    let response = server.get("/t/test-tag").await;
    response.assert_status_ok();
    let body = response.text();
    assert!(body.contains("test-tag"), "should contain tag name");
}

#[tokio::test]
async fn test_tag_aspect_page() {
    let (server, _tmp, _log) = create_test_server().await;

    // Add votes
    let vote_payload = serde_json::json!({
        "tag": "#tools",
        "aspect": ":usefulness",
        "a": "/tool-a",
        "b": "/tool-b",
        "score": 35
    });

    server
        .post("/api/v0/vote")
        .add_header("Authorization", "Bearer test-secret")
        .json(&vote_payload)
        .await;

    let response = server.get("/t/tools/a/usefulness").await;
    response.assert_status_ok();
    let body = response.text();
    assert!(body.contains("tools"), "should contain tag");
    assert!(body.contains("usefulness"), "should contain aspect");
    assert!(body.contains("tool-a") || body.contains("tool-b"), "should contain items");
}

