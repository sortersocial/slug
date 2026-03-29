use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tower::ServiceExt as _;

use slugsocial_server::state::{AppConfig, AppState};

#[derive(Debug, Clone, Serialize, Deserialize)]
enum SelectedRefV2 {
    I(u32),
    S(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeStateV2 {
    v: u8,
    #[serde(default)]
    base: String,
    #[serde(default)]
    open_suffixes: Vec<String>,
    #[serde(default)]
    selected: Option<SelectedRefV2>,
}

#[tokio::test]
async fn tree_accepts_state_blob_s_param() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().to_string();
    let event_log_path = format!("{}/events.jsonl", data_dir);

    let state = AppState::new(AppConfig {
        data_dir,
        event_log_path,
        views_path: Some(format!("{}/views.json", tmp.path().to_string_lossy())),
    });
    let app = slugsocial_server::create_app(state);

    // postcard(TreeStateV2) base64url(no pad)
    // Root is `/tree` => `~/` so these relative suffixes resolve to `~/a/b` and `~/c/d`.
    let st = TreeStateV2 {
        v: 2,
        base: "".to_string(),
        open_suffixes: vec!["a/b".to_string(), "c/d".to_string()],
        selected: Some(SelectedRefV2::I(0)),
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
        views_path: Some(format!("{}/views.json", tmp.path().to_string_lossy())),
    });
    let app = slugsocial_server::create_app(state);

    let resp = app.clone()
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

    assert!(loc.contains("s="), "location should include s=... (got {loc})");

    // Follow redirect target and ensure it's a valid baseline state.
    let path = loc.strip_prefix("http://localhost").unwrap_or(loc);
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

