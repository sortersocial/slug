use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tower::ServiceExt as _;

use slugsocial_server::state::{AppConfig, AppState, KeyRecord};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeStateV1 {
    v: u8,
    #[serde(default)]
    open: Vec<String>,
    #[serde(default)]
    selected: Option<String>,
}

#[tokio::test]
async fn tree_accepts_state_blob_s_param() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().to_string();
    let event_log_path = format!("{}/events.jsonl", data_dir);

    let state = AppState::new(AppConfig {
        data_dir,
        event_log_path,
        keys: vec![KeyRecord {
            id: "dev".to_string(),
            secret: "dev".to_string(),
        }],
        rate_limit_per_minute: 9999,
        views_path: Some(format!("{}/views.json", tmp.path().to_string_lossy())),
        x_bot: None,
    });
    let app = slugsocial_server::create_app(state);

    // postcard(TreeStateV1) base64url(no pad)
    let st = TreeStateV1 {
        v: 1,
        open: vec!["~/a/b".to_string(), "~/c/d".to_string()],
        selected: Some("~/a/b".to_string()),
    };
    let bytes = postcard::to_allocvec(&st).unwrap();
    let s = URL_SAFE_NO_PAD.encode(bytes);
    let uri = format!("/tree?s={s}");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn tree_missing_s_redirects_to_baseline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().to_string();
    let event_log_path = format!("{}/events.jsonl", data_dir);

    let state = AppState::new(AppConfig {
        data_dir,
        event_log_path,
        keys: vec![KeyRecord {
            id: "dev".to_string(),
            secret: "dev".to_string(),
        }],
        rate_limit_per_minute: 9999,
        views_path: Some(format!("{}/views.json", tmp.path().to_string_lossy())),
        x_bot: None,
    });
    let app = slugsocial_server::create_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tree")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    let loc = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Baseline postcard blob for v=1, open=[], selected=None is 3 bytes -> 4 chars base64url: "AQAA".
    assert!(loc.contains("s=AQAA"), "location should include baseline s=AQAA (got {loc})");
}

