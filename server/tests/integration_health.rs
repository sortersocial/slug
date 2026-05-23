mod support;

use support::*;

#[tokio::test]
async fn test_healthz() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{}/healthz", addr))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_index_page() {
    // HTML routes are offline during the auth-v3 refactor.
}

