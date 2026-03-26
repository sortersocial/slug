use slugsocial_server::{
    event_log::EventLog,
    state::{AppConfig, AppState, KeyRecord},
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use std::net::SocketAddr;
use std::path::PathBuf;

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
        views_path: None,
        x_bot: None,
        telegram_bot: None,
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

/// Spin up a server that replays all events already present in the given log file.
/// Uses a separate temp dir for views.redb so it doesn't conflict with the original server.
async fn create_test_server_from_log(log_path: PathBuf) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let views_tmp = TempDir::new().unwrap();
    let views_path = views_tmp.path().join("views.json");
    let cfg = AppConfig {
        data_dir: log_path.parent().unwrap().to_string_lossy().to_string(),
        event_log_path: log_path.to_string_lossy().to_string(),
        keys: vec![KeyRecord {
            id: "test-key".to_string(),
            secret: "test-secret".to_string(),
        }],
        rate_limit_per_minute: 1000,
        views_path: Some(views_path.to_string_lossy().to_string()),
        x_bot: None,
        telegram_bot: None,
    };

    let state = AppState::new(cfg.clone());
    // Replay existing events (mirrors main.rs startup).
    {
        let (events, _) = EventLog::new(&cfg.event_log_path).load_all().await.unwrap();
        let mut reduced = state.reduced.write().await;
        for ev in events {
            reduced.apply_event(ev);
        }
    }
    let app = slugsocial_server::create_app(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    (addr, handle)
}

const TEST_DOC: &str = "@00000000-0000-0000-0000-000000000001:testrig:test/model\n#passkey-test\n~/pk/a { item a }\n";
const OTHER_ACTOR_DOC: &str = "@00000000-0000-0000-0000-000000000002:testrig:test/model\n#passkey-test\n~/pk/b { item b }\n";

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
        "text": "@aec1e31c:claudecode:anthropic/claude-sonnet-4.5\n~/x {x}\n",
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
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n#cli\n~/clap {cli parser}\n~/argh {cli parser}\n~/clap 3:1 ~/argh {because clap is more full-featured}\n",
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
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n#langs\n~/rust {systems}\n~/go {concurrency}\n~/rust 3:1 ~/go {because i prefer rust for systems work}\n",
    });
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();

    // Then query ranking (global: parent=~ — default empty parent only ranks direct children of "")
    let response = client
        .get(&format!("http://{}/api/v0/rank", addr))
        .query(&[("parent", "~")])
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
    assert_eq!(ranking[0]["item"], "https://slug.social/~/rust");
    assert!(body["unranked_items"].is_array());
}

#[tokio::test]
async fn test_check_endpoint_does_not_commit() {
    let (addr, tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let check_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n~/a {x}\n~/b {y}\n~/a 2:1 ~/b {because}\n",
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
#topic\n\
~/topic {topic body}\n\
~/topic/a {alpha body}\n\
~/topic/b {beta body}\n\
~/topic/c {gamma body}\n\
~/other/x {x body}\n\
~/other/y {y body}\n\
~/topic/a 4:1 ~/topic/b {a over b}\n\
~/other/x 2:1 ~/other/y {other pair}\n",
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

// ============================================================================
// Passkey Authentication Tests
// ============================================================================

#[tokio::test]
async fn test_passkey_unregistered_no_passkey_succeeds() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // First ingest without passkey — server generates and returns a passkey.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "first ingest should succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["registered"], true, "first ingest auto-registers the actor");
    let pk = body["passkey"].as_str().expect("passkey should be returned");
    assert!(pk.starts_with("slug_sk_"), "generated passkey should have slug_sk_ prefix");
}

#[tokio::test]
async fn test_passkey_first_ingest_registers() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // First ingest without passkey — server auto-registers, returns passkey, 2 events.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "first ingest should succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["registered"], true, "first ingest should set registered: true");
    assert_eq!(body["events_appended"], 2, "registration + ingest = 2 events");

    // Providing a passkey for an unregistered actor is rejected.
    let resp2 = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .header("x-slug-passkey", "slug_sk_ShouldBeRejected")
        .json(&serde_json::json!({ "text": OTHER_ACTOR_DOC }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), reqwest::StatusCode::UNAUTHORIZED, "passkey for unregistered actor should 401");
    let body2: serde_json::Value = resp2.json().await.unwrap();
    assert!(body2["error"].as_str().unwrap().contains("no passkey registered"));
}

#[tokio::test]
async fn test_passkey_second_ingest_correct_passkey_succeeds() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // First ingest — get the server-generated passkey.
    let resp1 = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    let body1: serde_json::Value = resp1.json().await.unwrap();
    let passkey = body1["passkey"].as_str().unwrap().to_string();

    // Second ingest with the correct passkey — should succeed.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .header("x-slug-passkey", &passkey)
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "second ingest with correct passkey should succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_ne!(body["registered"], true, "second ingest should not re-register");
}

#[tokio::test]
async fn test_passkey_second_ingest_wrong_passkey_401() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Register via first ingest (no passkey needed).
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();

    // Wrong passkey — should 401.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .header("x-slug-passkey", "slug_sk_WrongPasskey")
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED, "wrong passkey should return 401");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap().contains("invalid passkey"));
}

#[tokio::test]
async fn test_passkey_second_ingest_missing_passkey_401() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Register via first ingest (no passkey needed).
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();

    // No passkey on second ingest — should 401.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED, "missing passkey for registered actor should return 401");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap().contains("passkey"));
}

#[tokio::test]
async fn test_passkey_different_actor_unaffected() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Register TEST_ACTOR via first ingest.
    client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();

    // A completely different actor without a passkey should also succeed (auto-registers separately).
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": OTHER_ACTOR_DOC }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "different unregistered actor should succeed without passkey");
}

#[tokio::test]
async fn test_passkey_passkey_in_json_body_works() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Register via first ingest — get the server-generated passkey.
    let resp1 = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    let body1: serde_json::Value = resp1.json().await.unwrap();
    let passkey = body1["passkey"].as_str().unwrap().to_string();

    // Second ingest: pass the correct passkey in the JSON body instead of header.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC, "passkey": passkey }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "passkey in JSON body should work");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_ne!(body["registered"], true, "second ingest should not re-register");
}

#[tokio::test]
async fn test_passkey_event_replay_restores_actor_keys() {
    let (addr, tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let log_path = tmp.path().join("events.jsonl");

    // Register actor via first ingest — capture the server-generated passkey.
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let passkey = body["passkey"].as_str().unwrap().to_string();

    // Boot a second server from the same log file (simulates restart).
    let (addr2, _handle2) = create_test_server_from_log(log_path).await;

    // Correct passkey should succeed on the new server (actor_keys restored from log).
    let resp2 = client
        .post(&format!("http://{}/api/v0/ingest", addr2))
        .header("x-slug-passkey", &passkey)
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert!(resp2.status().is_success(), "correct passkey should succeed after replay");

    // Wrong passkey should fail on the new server too.
    let resp3 = client
        .post(&format!("http://{}/api/v0/ingest", addr2))
        .header("x-slug-passkey", "slug_sk_WrongAfterReplay")
        .json(&serde_json::json!({ "text": TEST_DOC }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp3.status(), reqwest::StatusCode::UNAUTHORIZED, "wrong passkey should still fail after replay");
}

/// Thread connective tissue: item_threads and VoteData.thread are exposed by item, pair, and matchup APIs.
#[tokio::test]
async fn test_garden_item_pair_matchup_include_threads() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Ingest with #tag and items/vote so item_threads and vote.thread are populated.
    let ingest_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n\
#sorting-hat\n\
~/sorts/insertion { O(n^2) }\n\
~/sorts/mergesort { O(n log n) }\n\
~/sorts/insertion 3:1 ~/sorts/mergesort { simpler for small n }\n",
    });
    let ingest_resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();
    assert!(ingest_resp.status().is_success(), "ingest should succeed");

    // GET item: body + threads
    let item_resp = client
        .get(&format!("http://{}/api/v0/item", addr))
        .query(&[("item", "~/sorts/insertion")])
        .send()
        .await
        .unwrap();
    assert!(item_resp.status().is_success());
    let item: serde_json::Value = item_resp.json().await.unwrap();
    assert_eq!(item["item"], "https://slug.social/~/sorts/insertion");
    assert!(item["body"].as_str().unwrap().contains("O(n^2)"));
    let threads: Vec<&str> = item["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(threads.contains(&"sorting-hat"), "item threads should contain sorting-hat: {:?}", threads);

    // GET pair (under sorts): left, right, threads
    let pair_resp = client
        .get(&format!("http://{}/api/v0/pair", addr))
        .query(&[("parent", "~/sorts")])
        .send()
        .await
        .unwrap();
    assert!(pair_resp.status().is_success());
    let pair: serde_json::Value = pair_resp.json().await.unwrap();
    let pair_threads: Vec<&str> = pair["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(pair_threads.contains(&"sorting-hat"), "pair threads should contain sorting-hat: {:?}", pair_threads);

    // GET matchup: vote history with thread per vote
    let matchup_resp = client
        .get(&format!("http://{}/api/v0/matchup", addr))
        .query(&[("item", "~/sorts/insertion")])
        .send()
        .await
        .unwrap();
    assert!(matchup_resp.status().is_success());
    let matchup: serde_json::Value = matchup_resp.json().await.unwrap();
    assert_eq!(matchup["item"], "https://slug.social/~/sorts/insertion");
    let votes = matchup["votes"].as_array().unwrap();
    assert!(!votes.is_empty(), "matchup should have at least one vote");
    let first_thread = votes[0]["thread"].as_str().unwrap();
    assert_eq!(first_thread, "#sorting-hat", "vote should cite thread");
}

#[tokio::test]
async fn test_search_page_and_results() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Ingest some data to search through.
    let ingest_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n\
#philosophy\n\
~/parables/counting-the-cost { A parable about weighing what you give up }\n\
~/parables/prodigal-son { The famous story of return and forgiveness }\n\
~/parables/counting-the-cost 3:1 ~/parables/prodigal-son { counting resonates more deeply }\n\
Some free prose about counting sheep at night.\n",
    });
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Full search page loads.
    let page = client
        .get(&format!("http://{}/search", addr))
        .send()
        .await
        .unwrap();
    assert!(page.status().is_success());
    let html = page.text().await.unwrap();
    assert!(html.contains("search-input"), "should have search input");

    // Full page with query param renders results server-side.
    let page_q = client
        .get(&format!("http://{}/search?q=counting", addr))
        .send()
        .await
        .unwrap();
    assert!(page_q.status().is_success());
    let html_q = page_q.text().await.unwrap();
    assert!(html_q.contains("counting"), "should contain search term in results");
    assert!(html_q.contains("search-results"), "should have results container");

    // Fragment endpoint returns just the results div.
    let frag = client
        .get(&format!("http://{}/search/results?q=counting", addr))
        .send()
        .await
        .unwrap();
    assert!(frag.status().is_success());
    let frag_html = frag.text().await.unwrap();
    assert!(frag_html.contains("search-results"), "fragment should have results div");
    assert!(frag_html.contains("<mark>"), "should highlight matching terms");
    assert!(frag_html.contains("counting"), "should find item with 'counting' in path");

    // Search for thread name.
    let frag_thread = client
        .get(&format!("http://{}/search/results?q=philosophy", addr))
        .send()
        .await
        .unwrap();
    let frag_thread_html = frag_thread.text().await.unwrap();
    assert!(frag_thread_html.contains("philosophy"), "should find thread by tag");

    // Empty/short query returns no results.
    let frag_short = client
        .get(&format!("http://{}/search/results?q=a", addr))
        .send()
        .await
        .unwrap();
    let frag_short_html = frag_short.text().await.unwrap();
    assert!(!frag_short_html.contains("<mark>"), "single char query should return no results");

    // No matches.
    let frag_none = client
        .get(&format!("http://{}/search/results?q=zzzznotfound", addr))
        .send()
        .await
        .unwrap();
    let frag_none_html = frag_none.text().await.unwrap();
    assert!(frag_none_html.contains("no results"), "should show no results message");
}

#[tokio::test]
async fn test_view_counts_increment_and_display() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Counts are kept in-memory and updated synchronously before the handler reads them,
    // so the displayed count is inclusive of the current request.
    // First view: 1
    let r1 = client.get(&format!("http://{}/", addr)).send().await.unwrap();
    assert!(r1.status().is_success());
    let html1 = r1.text().await.unwrap();
    assert!(html1.contains("1 views"), "first view should show 1 views, got: {html1}");

    // Second view: 2
    let r2 = client.get(&format!("http://{}/", addr)).send().await.unwrap();
    assert!(r2.status().is_success());
    let html2 = r2.text().await.unwrap();
    assert!(html2.contains("2 views"), "second view should show 2 views, got: {html2}");

    // Third view: 3
    let r3 = client.get(&format!("http://{}/", addr)).send().await.unwrap();
    assert!(r3.status().is_success());
    let html3 = r3.text().await.unwrap();
    assert!(html3.contains("3 views"), "third view should show 3 views, got: {html3}");

    // Ontology index /~ has its own counter — starts at 1 on first view
    let r4 = client.get(&format!("http://{}/~", addr)).send().await.unwrap();
    assert!(r4.status().is_success());
    let html4 = r4.text().await.unwrap();
    assert!(html4.contains("1 views"), "first /~ view should show 1 views, got: {html4}");

    // API and non-HTML routes must not bump any counter
    let _ = client.get(&format!("http://{}/healthz", addr)).send().await.unwrap();
    let r5 = client.get(&format!("http://{}/", addr)).send().await.unwrap();
    let html5 = r5.text().await.unwrap();
    assert!(html5.contains("4 views"), "after healthz, / should show 4 views, got: {html5}");
}

#[tokio::test]
async fn test_search_handles_multibyte_unicode() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Ingest data with multi-byte UTF-8 characters (em dashes, accented chars).
    let ingest_payload = serde_json::json!({
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n\
#unicode-test\n\
~/tools/symex-el { Siddhartha — a modal structural editing package for Emacs. Très élégant. }\n",
    });
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&ingest_payload)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Search should not panic on multi-byte content.
    let frag = client
        .get(&format!("http://{}/search/results?q=siddhartha", addr))
        .send()
        .await
        .unwrap();
    assert!(frag.status().is_success(), "search with multi-byte content should not panic");
    let html = frag.text().await.unwrap();
    assert!(html.contains("search-results"));

    // Search for a word near multi-byte chars.
    let frag2 = client
        .get(&format!("http://{}/search/results?q=modal", addr))
        .send()
        .await
        .unwrap();
    assert!(frag2.status().is_success());
}

#[tokio::test]
async fn test_rank_history() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest = |text: &'static str| {
        let client = client.clone();
        let addr = addr;
        async move {
            client
                .post(&format!("http://{}/api/v0/ingest", addr))
                .header("x-slug-key", "test-key:test-secret")
                .json(&serde_json::json!({ "text": text }))
                .send()
                .await
                .unwrap()
        }
    };

    // First ingest: rust vs python — two votes on rust in one doc (the multi-vote case).
    ingest(
        "@00000000-0000-0000-0000-000000000001:rig:test/model\n\
         #hist-test\n\
         ~/hist/rust { systems }\n\
         ~/hist/python { scripting }\n\
         ~/hist/go { concurrency }\n\
         ~/hist/rust 3:1 ~/hist/python { ownership over gc }\n\
         ~/hist/rust 2:1 ~/hist/go { performance over simplicity }\n",
    )
    .await;

    // History for rust should have one entry with two caused_by votes.
    let resp: serde_json::Value = client
        .get(&format!("http://{}/api/v0/rank-history", addr))
        .query(&[("item", "~/hist/rust")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["item"], "https://slug.social/~/hist/rust");
    let history = resp["history"].as_array().unwrap();
    assert_eq!(history.len(), 1, "one ingest → one history entry");
    let entry = &history[0];
    assert_eq!(entry["scope_rank"], 1, "rust should be #1 in scope after first ingest");
    assert_eq!(entry["scope_rank_delta"], 0, "delta is 0 on first appearance");
    let caused_by = entry["caused_by"].as_array().unwrap();
    assert_eq!(caused_by.len(), 2, "both votes in the ingest touched rust");

    // Second ingest: python beats go — rust not directly touched, so python gets a new entry.
    ingest(
        "@00000000-0000-0000-0000-000000000002:rig:test/model\n\
         #hist-test\n\
         ~/hist/python 3:1 ~/hist/go { dynamic typing is worth it }\n",
    )
    .await;

    // Python history: two entries (appeared in first ingest, then voted again here).
    let resp2: serde_json::Value = client
        .get(&format!("http://{}/api/v0/rank-history", addr))
        .query(&[("item", "~/hist/python")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hist2 = resp2["history"].as_array().unwrap();
    assert_eq!(hist2.len(), 2, "python touched in both ingests");

    // Second entry for python: caused_by has one vote (python 3:1 go).
    let caused_by2 = hist2[1]["caused_by"].as_array().unwrap();
    assert_eq!(caused_by2.len(), 1);
    assert!(caused_by2[0]["a"].as_str().unwrap().ends_with("python") ||
            caused_by2[0]["b"].as_str().unwrap().ends_with("python"));

    // Rust was NOT directly voted on in the second ingest — no new history entry.
    let resp3: serde_json::Value = client
        .get(&format!("http://{}/api/v0/rank-history", addr))
        .query(&[("item", "~/hist/rust")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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

    // Ingest 4 items with 1 vote (a vs b), leaving c and d as isolates.
    let doc = "@00000000-0000-0000-0000-000000000001:testrig:test/model\n\
               #connectivity-test\n\
               ~/conn/a { item a }\n\
               ~/conn/b { item b }\n\
               ~/conn/c { item c }\n\
               ~/conn/d { item d }\n\
               ~/conn/a 3:1 ~/conn/b { a is better }\n";
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .header("x-slug-key", "test-key:test-secret")
        .json(&serde_json::json!({ "text": doc }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "ingest should succeed");
    let resp_body: serde_json::Value = resp.json().await.unwrap();
    let passkey = resp_body["passkey"].as_str().unwrap().to_string();

    // Request pair under ~/conn — should include connectivity stats.
    let pair_resp = client
        .get(&format!("http://{}/api/v0/pair", addr))
        .query(&[("parent", "~/conn")])
        .send()
        .await
        .unwrap();
    assert!(pair_resp.status().is_success());
    let pair: serde_json::Value = pair_resp.json().await.unwrap();

    let conn = &pair["connectivity"];
    assert!(!conn.is_null(), "pair response should include connectivity stats");
    assert_eq!(conn["items"].as_u64().unwrap(), 4, "4 items in scope");
    // 1 component (a,b) + 2 isolates (c,d) = 3 components
    assert_eq!(conn["components"].as_u64().unwrap(), 3, "3 components (1 connected + 2 isolates)");
    assert_eq!(conn["comparisons_until_connected"].as_u64().unwrap(), 2, "need 2 more comparisons");
    assert_eq!(conn["pairs_voted"].as_u64().unwrap(), 1, "1 pair voted");
    assert_eq!(conn["pairs_possible"].as_u64().unwrap(), 6, "4*3/2 = 6 possible pairs");

    // Add a vote connecting c to a — should reduce components.
    let doc2 = "@00000000-0000-0000-0000-000000000001:testrig:test/model\n\
                #connectivity-test\n\
                ~/conn/c 2:1 ~/conn/a { c beats a }\n";
    let resp2 = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .header("x-slug-key", "test-key:test-secret")
        .header("x-slug-passkey", &passkey)
        .json(&serde_json::json!({ "text": doc2 }))
        .send()
        .await
        .unwrap();
    let resp2_status = resp2.status();
    let resp2_body: serde_json::Value = resp2.json().await.unwrap();
    assert!(resp2_status.is_success(), "second ingest failed: {}", resp2_body);

    let pair_resp2 = client
        .get(&format!("http://{}/api/v0/pair", addr))
        .query(&[("parent", "~/conn")])
        .send()
        .await
        .unwrap();
    let pair2: serde_json::Value = pair_resp2.json().await.unwrap();
    let conn2 = &pair2["connectivity"];
    // Now: component (a,b,c) + isolate (d) = 2 components
    assert_eq!(conn2["components"].as_u64().unwrap(), 2, "2 components after connecting c");
    assert_eq!(conn2["comparisons_until_connected"].as_u64().unwrap(), 1, "1 more comparison to connect");
    assert_eq!(conn2["pairs_voted"].as_u64().unwrap(), 2, "2 pairs voted now");
}

