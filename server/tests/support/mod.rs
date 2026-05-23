use sha2::{Digest, Sha256};
use slugsocial_server::{
    event_log::EventLog,
    events::{Event, TokenIssued, UserRegistered},
    spawn_writer_actor_for_test,
    state::{AppConfig, AppState},
};
use std::net::SocketAddr;
use tempfile::TempDir;
use tokio::net::TcpListener;

pub fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Fixed bearer for integration tests (`TokenIssued` seeded into reducer in `create_test_server`).
pub fn test_bearer() -> String {
    let token_id = "testtok";
    let secret = "secret";
    format!("slug_{token_id}_{secret}")
}

/// Compact JSON for `POST /ui` (`HtmlUiAction::PostIngest`).
pub fn ui_post_ingest_rpc(room: &str, thread_tag: &str, text: &str) -> String {
    serde_json::json!({
        "action": "post_ingest",
        "room": room,
        "thread_tag": thread_tag,
        "text": text,
    })
    .to_string()
}

/// Compact JSON for `POST /ui` (`HtmlUiAction::CheckIngest`).
pub fn ui_check_ingest_rpc(room: &str, thread_tag: &str, text: &str, error_target: &str) -> String {
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
pub async fn rpc_batch(
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

pub async fn seed_test_token(state: &AppState) {
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

pub async fn create_test_server_with_state() -> (
    SocketAddr,
    TempDir,
    EventLog,
    AppState,
    tokio::task::JoinHandle<()>,
) {
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

pub async fn create_test_server() -> (SocketAddr, TempDir, EventLog, tokio::task::JoinHandle<()>) {
    let (addr, tmp, log, _state, handle) = create_test_server_with_state().await;
    (addr, tmp, log, handle)
}
