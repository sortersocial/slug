mod support;

use support::*;

#[tokio::test]
async fn test_post_rejects_thread_tag_with_slash() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let b = test_bearer();

    let batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "foo/bar",
            "text": "hello from a bad tag\n",
            "return_rank_diff": false
        }
    }]);
    let body = rpc_batch(&client, addr, Some(&b), batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], false);
    let err = line["error"].as_str().unwrap_or_default();
    assert!(
        err.contains('/') || err.to_lowercase().contains("slash") || err.contains("thread tag"),
        "expected slash rejection, got: {err}"
    );
}

#[tokio::test]
async fn test_ingest_actor_with_colons_is_detected_and_validated() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let b = test_bearer();

    // Old archive style: agent includes colons but UUID is only a prefix (invalid).
    let bad_delegate = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "t",
            "delegate": "aec1e31c:claudecode:anthropic/claude-sonnet-4.5",
            "text": "~/x {x}\n",
            "return_rank_diff": false
        }
    }]);
    let body = rpc_batch(&client, addr, Some(&b), bad_delegate).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], false);
    assert_eq!(line["error"], "invalid delegate format");
    let hint = line["hint"].as_str().unwrap_or_default();
    assert!(
        hint.to_lowercase().contains("uuid"),
        "hint should mention uuid, got: {hint}"
    );

    let at_batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "t",
            "delegate": "@00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/x {x}\n",
            "return_rank_diff": false
        }
    }]);
    let at_body = rpc_batch(&client, addr, Some(&b), at_batch).await;
    let at_line = &at_body["results"][0];
    assert_eq!(at_line["ok"], false);
    let at_hint = at_line["hint"].as_str().unwrap_or_default();
    assert!(
        at_hint.contains('@'),
        "hint should reject '@' in delegate, got: {at_hint}"
    );
}

#[tokio::test]
async fn test_post_redact_removes_garden_and_marks_thread() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let post_body = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "redact-test",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "~/del-a {a}\n~/del-b {b}\n{vote line}\n~/del-a 2:1 ~/del-b\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    let line = &post_body["results"][0];
    assert_eq!(line["ok"], true, "post ingest: {:?}", line);

    let thread_before = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": "public",
                "thread_tag": "redact-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let items_before = thread_before["results"][0]["result"]["ForumThread"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items_before.len(), 1);
    let post_id = items_before[0]["id"].as_str().unwrap().to_string();
    assert_eq!(items_before[0]["redacted"], serde_json::Value::Bool(false));

    let rank_before = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenRank": {
                "room": "public",
                "parent_path": "~",
                "depth": 1
            }
        }]),
    )
    .await;
    let rank_line = &rank_before["results"][0];
    assert_eq!(rank_line["ok"], true);
    let ranking = rank_line["result"]["GardenRank"]["components"][0]["ranking"]
        .as_array()
        .unwrap();
    assert_eq!(ranking.len(), 2);

    let redact = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "PostRedact": { "post_id": post_id }
        }]),
    )
    .await;
    assert_eq!(redact["results"][0]["ok"], true, "redact: {:?}", redact);

    let rank_after = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenRank": {
                "room": "public",
                "parent_path": "~",
                "depth": 1
            }
        }]),
    )
    .await;
    let ra_line = &rank_after["results"][0];
    assert_eq!(ra_line["ok"], true);
    let comps = ra_line["result"]["GardenRank"]["components"]
        .as_array()
        .unwrap();
    assert!(
        comps.is_empty() || comps[0]["ranking"].as_array().unwrap().is_empty(),
        "votes from redacted post should be removed: {:?}",
        ra_line
    );

    let thread_after = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": "public",
                "thread_tag": "redact-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let item = &thread_after["results"][0]["result"]["ForumThread"]["items"][0];
    assert_eq!(item["redacted"], serde_json::Value::Bool(true));
    assert_eq!(item["body"].as_str().unwrap(), "");
}

#[tokio::test]
async fn test_vote_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "cli",
            "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/clap {cli parser}\n~/argh {cli parser}\n{because clap is more full-featured}\n~/clap 3:1 ~/argh\n",
            "return_rank_diff": true
        }
    }]);
    let body = rpc_batch(&client, addr, Some(&test_bearer()), batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], true);
    let post = &line["result"]["PostOk"];
    assert!(post["next"].is_object());
}

#[tokio::test]
async fn test_rank_endpoint() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "langs",
            "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/rust {systems}\n~/go {concurrency}\n{because i prefer rust for systems work}\n~/rust 3:1 ~/go\n",
            "return_rank_diff": false
        }
    }]);
    rpc_batch(&client, addr, Some(&test_bearer()), ingest).await;

    let rank_batch = serde_json::json!([{
        "GetGardenRank": {
            "room": "public",
            "parent_path": "~",
            "depth": 1
        }
    }]);
    let body = rpc_batch(&client, addr, None, rank_batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], true);
    let rank = &line["result"]["GardenRank"];
    assert!(rank["components"].is_array());
    let components = rank["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    let ranking = components[0]["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 2);
    assert_eq!(ranking[0]["item"], "https://slug.social/~/rust");
    assert!(rank["unranked_items"].is_array());
}

#[tokio::test]
async fn test_check_endpoint_does_not_commit() {
    let (addr, tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let check_batch = serde_json::json!([{
        "Check": {
            "room": "public",
            "text": "~/a {x}\n~/b {y}\n{because}\n~/a 2:1 ~/b\n",
        }
    }]);
    let resp_body = rpc_batch(&client, addr, None, check_batch).await;
    let line = &resp_body["results"][0];
    assert_eq!(line["ok"], true);

    // It should not write events.jsonl.
    let log_path = tmp.path().join("events.jsonl");
    let content = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(content.trim().is_empty(), "check must not append events");

    // And the live state should still have no forum threads (check is dry-run).
    let list_batch = serde_json::json!([{
        "ListForumThreads": { "room": "public" }
    }]);
    let threads_body = rpc_batch(&client, addr, None, list_batch).await;
    let tline = &threads_body["results"][0];
    assert_eq!(tline["ok"], true);
    let threads = tline["result"]["ForumThreads"]["threads"]
        .as_array()
        .unwrap();
    assert!(threads.is_empty());
}

#[tokio::test]
async fn test_garden_item_pair_matchup_include_threads() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let ingest_batch = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "sorting-hat",
            "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
            "text": "~/sorts/insertion { O(n^2) }\n~/sorts/mergesort { O(n log n) }\n{ simpler for small n }\n~/sorts/insertion 3:1 ~/sorts/mergesort\n",
            "return_rank_diff": false
        }
    }]);
    let ing = rpc_batch(&client, addr, Some(&test_bearer()), ingest_batch).await;
    assert_eq!(ing["results"][0]["ok"], true, "ingest should succeed");

    let item_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenItem": {
                "room": "public",
                "item_path": "~/sorts/insertion",
                "full": true
            }
        }]),
    )
    .await;
    let item = &item_body["results"][0]["result"]["GardenItem"];
    assert_eq!(item["item"], "https://slug.social/~/sorts/insertion");
    assert!(item["body"].as_str().unwrap().contains("O(n^2)"));
    let threads: Vec<&str> = item["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(
        threads.contains(&"sorting-hat"),
        "item threads should contain sorting-hat: {:?}",
        threads
    );

    let pair_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": "~/sorts"
            }
        }]),
    )
    .await;
    let pair = &pair_body["results"][0]["result"]["Pair"];
    let pair_threads: Vec<&str> = pair["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(
        pair_threads.contains(&"sorting-hat"),
        "pair threads should contain sorting-hat: {:?}",
        pair_threads
    );

    let matchup_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetMatchup": {
                "room": "public",
                "item_path": "~/sorts/insertion"
            }
        }]),
    )
    .await;
    let matchup = &matchup_body["results"][0]["result"]["Matchup"];
    assert_eq!(matchup["item"], "https://slug.social/~/sorts/insertion");
    let votes = matchup["votes"].as_array().unwrap();
    assert!(!votes.is_empty(), "matchup should have at least one vote");
    let first_thread = votes[0]["thread"].as_str().unwrap();
    assert_eq!(first_thread, "sorting-hat", "vote should cite thread");
}

/// `GetGardenRank` and `GetPair` must agree on whether a parent path "exists" (empty garden).
#[tokio::test]
async fn test_garden_pair_and_rank_path_not_found_consistent() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bad_parent = "https://slug.social/~/no_such_garden_parent_for_scope_test";

    let rank = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenRank": {
                "room": "public",
                "parent_path": bad_parent,
                "depth": 1
            }
        }]),
    )
    .await;
    let rank_line = &rank["results"][0];
    assert_eq!(rank_line["ok"], false, "GetGardenRank: {:?}", rank_line);
    assert_eq!(rank_line["error"], "path not found");

    let pair = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": bad_parent
            }
        }]),
    )
    .await;
    let pair_line = &pair["results"][0];
    assert_eq!(pair_line["ok"], false, "GetPair: {:?}", pair_line);
    assert_eq!(pair_line["error"], "path not found");

    let ingest = rpc_batch(
        &client,
        addr,
        Some(&test_bearer()),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "scope-a",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "~/no_such_garden_parent_for_scope_test/x {a}\n~/no_such_garden_parent_for_scope_test/y {b}\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(ingest["results"][0]["ok"], true, "ingest: {:?}", ingest);

    let pair2 = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": bad_parent
            }
        }]),
    )
    .await;
    let pair2_line = &pair2["results"][0];
    assert_eq!(
        pair2_line["ok"], true,
        "GetPair after parent materialized: {:?}",
        pair2_line
    );
    assert!(pair2_line["result"]["Pair"].is_object());
}

#[tokio::test]
async fn test_rank_history() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let bearer = test_bearer();
    let ingest = |delegate: &str, text: &str| {
        let client = client.clone();
        let bearer = bearer.clone();
        let text = text.to_string();
        let delegate = delegate.to_string();
        async move {
            rpc_batch(
                &client,
                addr,
                Some(&bearer),
                serde_json::json!([{
                    "Post": {
                        "room": "public",
                        "thread_tag": "hist-test",
                        "delegate": delegate,
                        "text": text,
                        "return_rank_diff": false
                    }
                }]),
            )
            .await
        }
    };

    ingest(
        "00000000-0000-0000-0000-000000000001:rig:test/model",
        "~/hist/rust { systems }\n~/hist/python { scripting }\n~/hist/go { concurrency }\n{ ownership over gc }\n~/hist/rust 3:1 ~/hist/python\n{ performance over simplicity }\n~/hist/rust 2:1 ~/hist/go\n",
    )
    .await;

    let hist_rust = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetRankHistory": {
                "room": "public",
                "item_path": "~/hist/rust"
            }
        }]),
    )
    .await;
    let resp = &hist_rust["results"][0]["result"]["RankHistory"];
    assert_eq!(resp["item"], "https://slug.social/~/hist/rust");
    let history = resp["history"].as_array().unwrap();
    assert_eq!(history.len(), 1, "one ingest → one history entry");
    let entry = &history[0];
    assert_eq!(
        entry["scope_rank"], 1,
        "rust should be #1 in scope after first ingest"
    );
    assert_eq!(
        entry["scope_rank_delta"], 0,
        "delta is 0 on first appearance"
    );
    let caused_by = entry["caused_by"].as_array().unwrap();
    assert_eq!(caused_by.len(), 2, "both votes in the ingest touched rust");
    assert_eq!(
        entry["thread_post_index"], 0,
        "rank history links use same 0-based index as /t/hist-test/0"
    );

    ingest(
        "00000000-0000-0000-0000-000000000002:rig:test/model",
        "{ dynamic typing is worth it }\n~/hist/python 3:1 ~/hist/go\n",
    )
    .await;

    let hist_py = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetRankHistory": {
                "room": "public",
                "item_path": "~/hist/python"
            }
        }]),
    )
    .await;
    let resp2 = &hist_py["results"][0]["result"]["RankHistory"];
    let hist2 = resp2["history"].as_array().unwrap();
    assert_eq!(hist2.len(), 2, "python touched in both ingests");

    let caused_by2 = hist2[1]["caused_by"].as_array().unwrap();
    assert_eq!(caused_by2.len(), 1);
    assert!(
        caused_by2[0]["a"].as_str().unwrap().ends_with("python")
            || caused_by2[0]["b"].as_str().unwrap().ends_with("python")
    );
    assert_eq!(
        hist2[0]["thread_post_index"], 0,
        "first hist-test post is chronological index 0"
    );
    assert_eq!(
        hist2[1]["thread_post_index"], 1,
        "second ingest is chronological index 1"
    );

    let hist_rust2 = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetRankHistory": {
                "room": "public",
                "item_path": "~/hist/rust"
            }
        }]),
    )
    .await;
    let resp3 = &hist_rust2["results"][0]["result"]["RankHistory"];
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

    let doc = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "connectivity-test",
            "delegate": "00000000-0000-0000-0000-000000000001:testrig:test/model",
            "text": "~/conn/a { item a }\n~/conn/b { item b }\n~/conn/c { item c }\n~/conn/d { item d }\n{ a is better }\n~/conn/a 3:1 ~/conn/b\n",
            "return_rank_diff": false
        }
    }]);
    let resp = rpc_batch(&client, addr, Some(&test_bearer()), doc).await;
    assert_eq!(resp["results"][0]["ok"], true, "ingest should succeed");

    let pair_body = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": "~/conn"
            }
        }]),
    )
    .await;
    let pair = &pair_body["results"][0]["result"]["Pair"];

    let conn = &pair["connectivity"];
    assert!(
        !conn.is_null(),
        "pair response should include connectivity stats"
    );
    assert_eq!(conn["items"].as_u64().unwrap(), 4, "4 items in scope");
    assert_eq!(
        conn["components"].as_u64().unwrap(),
        3,
        "3 components (1 connected + 2 isolates)"
    );
    assert_eq!(
        conn["comparisons_until_connected"].as_u64().unwrap(),
        2,
        "need 2 more comparisons"
    );
    assert_eq!(conn["pairs_voted"].as_u64().unwrap(), 1, "1 pair voted");
    assert_eq!(
        conn["pairs_possible"].as_u64().unwrap(),
        6,
        "4*3/2 = 6 possible pairs"
    );

    let doc2 = serde_json::json!([{
        "Post": {
            "room": "public",
            "thread_tag": "connectivity-test",
            "delegate": "00000000-0000-0000-0000-000000000001:testrig:test/model",
            "text": "{ c beats a }\n~/conn/c 2:1 ~/conn/a\n",
            "return_rank_diff": false
        }
    }]);
    let resp2 = rpc_batch(&client, addr, Some(&test_bearer()), doc2).await;
    assert_eq!(
        resp2["results"][0]["ok"], true,
        "second ingest failed: {:?}",
        resp2
    );

    let pair_body2 = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetPair": {
                "room": "public",
                "parent_path": "~/conn"
            }
        }]),
    )
    .await;
    let pair2 = &pair_body2["results"][0]["result"]["Pair"];
    let conn2 = &pair2["connectivity"];
    assert_eq!(
        conn2["components"].as_u64().unwrap(),
        2,
        "2 components after connecting c"
    );
    assert_eq!(
        conn2["comparisons_until_connected"].as_u64().unwrap(),
        1,
        "1 more comparison to connect"
    );
    assert_eq!(
        conn2["pairs_voted"].as_u64().unwrap(),
        2,
        "2 pairs voted now"
    );
}
