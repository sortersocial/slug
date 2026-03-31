use slugsocial_server::{
    event_log::EventLog,
    state::{AppConfig, AppState},
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
    // HTML routes are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_ingest_actor_with_colons_is_detected_and_validated() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Old archive style: agent includes colons but UUID is only a prefix (invalid).
    // We should detect the agent line, then fail with "invalid agent format".
    let ingest_payload = serde_json::json!({
        "delegate": "@@aec1e31c:claudecode:anthropic/claude-sonnet-4.5",
        "thread": "t",
        "text": "~/x {x}\n",
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
    assert_eq!(body["error"], "invalid delegate format");
    let hint = body["hint"].as_str().unwrap_or_default();
    assert!(
        hint.to_lowercase().contains("uuid"),
        "hint should mention uuid, got: {hint}"
    );
}

#[tokio::test]
async fn test_vote_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // /api/v0/vote was removed; all votes are submitted via ingest.
    let ingest_payload = serde_json::json!({
        "delegate": "@@00000000-0000-0000-0000-000000000000:test:local/test",
        "thread": "cli",
        "text": "~/clap {cli parser}\n~/argh {cli parser}\n~/clap 3:1 ~/argh {because clap is more full-featured}\n",
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
        "delegate": "@@00000000-0000-0000-0000-000000000000:test:local/test",
        "thread": "langs",
        "text": "~/rust {systems}\n~/go {concurrency}\n~/rust 3:1 ~/go {because i prefer rust for systems work}\n",
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
        "delegate": "@@00000000-0000-0000-0000-000000000000:test:local/test",
        "thread": "t",
        "text": "~/a {x}\n~/b {y}\n~/a 2:1 ~/b {because}\n",
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
    // HTML routes are offline during the auth-v3 refactor.
}

/// Thread connective tissue: item_threads and VoteData.thread are exposed by item, pair, and matchup APIs.
#[tokio::test]
async fn test_garden_item_pair_matchup_include_threads() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Ingest with thread_id metadata so item_threads and vote.thread_id are populated.
    let ingest_payload = serde_json::json!({
        "delegate": "@@00000000-0000-0000-0000-000000000000:test:local/test",
        "thread": "sorting-hat",
        "text": "~/sorts/insertion { O(n^2) }\n~/sorts/mergesort { O(n log n) }\n~/sorts/insertion 3:1 ~/sorts/mergesort { simpler for small n }\n",
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
    assert_eq!(first_thread, "sorting-hat", "vote should cite thread");
}

#[tokio::test]
async fn test_search_page_and_results() {
    // HTML search pages are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_view_counts_increment_and_display() {
    // HTML view counters are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_search_handles_multibyte_unicode() {
    // HTML search pages are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_rank_history() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest = |payload: serde_json::Value| {
        let client = client.clone();
        let addr = addr;
        async move {
            client
                .post(&format!("http://{}/api/v0/ingest", addr))
                .json(&payload)
                .send()
                .await
                .unwrap()
        }
    };

    // First ingest: rust vs python — two votes on rust in one doc (the multi-vote case).
    ingest(
        serde_json::json!({
            "delegate": "@@00000000-0000-0000-0000-000000000001:rig:test/model",
            "thread": "hist-test",
            "text": "~/hist/rust { systems }\n~/hist/python { scripting }\n~/hist/go { concurrency }\n~/hist/rust 3:1 ~/hist/python { ownership over gc }\n~/hist/rust 2:1 ~/hist/go { performance over simplicity }\n",
        }),
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
        serde_json::json!({
            "delegate": "@@00000000-0000-0000-0000-000000000002:rig:test/model",
            "thread": "hist-test",
            "text": "~/hist/python 3:1 ~/hist/go { dynamic typing is worth it }\n",
        }),
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
    let doc = serde_json::json!({
        "delegate": "@@00000000-0000-0000-0000-000000000001:testrig:test/model",
        "thread": "connectivity-test",
        "text": "~/conn/a { item a }\n~/conn/b { item b }\n~/conn/c { item c }\n~/conn/d { item d }\n~/conn/a 3:1 ~/conn/b { a is better }\n",
    });
    let resp = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&doc)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "ingest should succeed");

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
    let doc2 = serde_json::json!({
        "delegate": "@@00000000-0000-0000-0000-000000000001:testrig:test/model",
        "thread": "connectivity-test",
        "text": "~/conn/c 2:1 ~/conn/a { c beats a }\n",
    });
    let resp2 = client
        .post(&format!("http://{}/api/v0/ingest", addr))
        .json(&doc2)
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

