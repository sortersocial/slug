use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use tower::ServiceExt as _;

use slugsocial_server::state::{AppConfig, AppState, KeyRecord};

#[tokio::test]
async fn tree_toggle_returns_js_and_updates_url() {
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

    // Minimal baseline state blob is produced by hitting /tree and reading Location,
    // but here we can just use the empty v1 blob which is still accepted.
    let body = "root=https%3A%2F%2Fslug.social%2F~%2F&target=https%3A%2F%2Fslug.social%2F~%2F&s=AQAA";

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tree/toggle")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/javascript"), "expected js content-type, got {ct}");

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let js = String::from_utf8_lossy(&bytes);
    assert!(js.contains("Idiomorph.morph"), "js should morph DOM");
    assert!(js.contains("history.replaceState"), "js should update URL");
    assert!(js.contains("?s="), "js should set blob URL");
}

