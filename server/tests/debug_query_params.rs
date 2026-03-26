use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt as _;

use slugsocial_server::state::{AppConfig, AppState, KeyRecord};

#[tokio::test]
async fn debug_query_parses_repeated_open_array_and_decodes() {
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

    // ~%2Fmodels%2Fllms == "~/models/llms"
    let uri = "/debug/query?open=~%2Fmodels%2Fllms&open=~%2Fmusic%2Fjazz&selected=~%2Fmodels%2Fllms&q=hello+world";

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
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        v["open"],
        serde_json::json!(["~/models/llms", "~/music/jazz"])
    );
    assert_eq!(v["selected"], serde_json::json!("~/models/llms"));
    assert_eq!(v["q"], serde_json::json!("hello world"));
}

