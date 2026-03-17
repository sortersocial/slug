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
async fn create_test_server_from_log(log_path: PathBuf) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let cfg = AppConfig {
        data_dir: log_path.parent().unwrap().to_string_lossy().to_string(),
        event_log_path: log_path.to_string_lossy().to_string(),
        keys: vec![KeyRecord {
            id: "test-key".to_string(),
            secret: "test-secret".to_string(),
        }],
        rate_limit_per_minute: 1000,
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
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n#cli\n/clap {cli parser}\n/argh {cli parser}\n/clap 3:1 /argh {because clap is more full-featured}\n",
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
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n#langs\n/rust {systems}\n/go {concurrency}\n/rust 3:1 /go {because i prefer rust for systems work}\n",
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
        "text": "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n",
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
        .query(&[("item", "sorts/insertion")])
        .send()
        .await
        .unwrap();
    assert!(item_resp.status().is_success());
    let item: serde_json::Value = item_resp.json().await.unwrap();
    assert_eq!(item["item"], "/sorts/insertion");
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
        .query(&[("parent", "sorts")])
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
        .query(&[("item", "sorts/insertion")])
        .send()
        .await
        .unwrap();
    assert!(matchup_resp.status().is_success());
    let matchup: serde_json::Value = matchup_resp.json().await.unwrap();
    assert_eq!(matchup["item"], "/sorts/insertion");
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

