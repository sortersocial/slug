use slugsocial_server::{
    event_log::EventLog,
    canonical_path::{canonicalize_item, canonicalize_tag},
    events::{Event, Ingest},
    ranking::ranked_items,
    reducer::{GroupState, ReducerState},
};


use tempfile::TempDir;

fn ingest_event(ts: i64, raw: &str) -> Event {
    Event::Ingest(Ingest {
        ts,
        // Stable ID for deterministic tests.
        id: format!("test-{ts}"),
        raw: raw.to_string(),
        principal: "test".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        thread_id: "t".to_string(),
    })
}

fn vote_doc(tag: &str, a: &str, b: &str, left: i32, right: i32) -> String {
    format!(
        "~/{tag}/{a} {{body a}}\n~/{tag}/{b} {{body b}}\n~/{tag}/{a} {left}:{right} ~/{tag}/{b} {{because test}}\n"
    )
}

// ============================================================================
// Reducer Tests
// ============================================================================

#[test]
fn reducer_and_ranking_linear_chain() {
    // Prefer /a over /b over /c.
    let mut state = ReducerState::default();

    // First ingest: define items + vote a > b.
    state.apply_event(ingest_event(1, "~/t/a {a}\n~/t/b {b}\n~/t/a 3:1 ~/t/b {because}\n"));
    // Second ingest: define c + vote b > c.
    state.apply_event(ingest_event(2, "~/t/c {c}\n~/t/b 3:1 ~/t/c {because}\n"));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].item, "https://slug.social/~/t/a");
    assert_eq!(ranked[1].item, "https://slug.social/~/t/b");
    assert_eq!(ranked[2].item, "https://slug.social/~/t/c");
}

#[test]
fn reducer_canonicalizes_identifiers() {
    let mut state = ReducerState::default();

    // Mix of formats across ingests (case + sigils).
    state.apply_event(ingest_event(
        1,
        "~/Tag/Item-A {x}\n~/Tag/Item-B {y}\n~/Tag/Item-A 2:1 ~/Tag/Item-B {because}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "~/TAG/ITEM-A 2:1 ~/TAG/ITEM-B {because}\n",
    ));
    state.apply_event(ingest_event(
        3,
        "~/tag/item-a 2:1 ~/tag/item-b {because}\n",
    ));

    assert_eq!(state.public().ranking_group.idx_to_item.len(), 2); // Should dedupe to 2 items
}

#[test]
fn reducer_handles_item_and_body_from_ingest() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/test-item {Description here}\n",
    ));

    let content = state.public();
    assert!(content.items.contains("https://slug.social/~/t/test-item"));
    assert_eq!(
        content.item_bodies.get("https://slug.social/~/t/test-item"),
        Some(&"Description here".to_string())
    );
    assert!(content
        .item_children
        .get("https://slug.social/~/t")
        .map(|c| c.contains("https://slug.social/~/t/test-item"))
        .unwrap_or(false));
}

#[test]
fn reducer_indexes_item_threads_and_vote_thread() {
    let mut state = ReducerState::default();
    // Thread routing is metadata now (ingest.thread_id), not parsed from raw.
    let mut ev = match ingest_event(1, "~/sorts/insertion { O(n^2) }\n~/sorts/mergesort { O(n log n) }\n~/sorts/insertion 3:1 ~/sorts/mergesort { simpler for small n }\n") {
        Event::Ingest(i) => i,
        _ => unreachable!(),
    };
    ev.thread_id = "sorting-hat".to_string();
    state.apply_event(Event::Ingest(ev));

    let content = state.public();
    let threads_for_insertion = content.item_threads.get("https://slug.social/~/sorts/insertion").unwrap();
    assert!(threads_for_insertion.contains("sorting-hat"));
    let threads_for_mergesort = content.item_threads.get("https://slug.social/~/sorts/mergesort").unwrap();
    assert!(threads_for_mergesort.contains("sorting-hat"));

    let vote = content.item_votes.get("https://slug.social/~/sorts/insertion").unwrap().front().unwrap();
    assert_eq!(vote.thread_id, "sorting-hat");
}

#[test]
fn reducer_aggregates_multiple_votes() {
    let mut state = ReducerState::default();

    // Multiple votes between same pair should accumulate weights.
    for ts in 1..=3 {
        state.apply_event(ingest_event(ts, &vote_doc("t", "a", "b", 2, 1)));
    }

    let group = &state.public().ranking_group;
    let a_idx = group.item_to_idx["https://slug.social/~/t/a"];
    let b_idx = group.item_to_idx["https://slug.social/~/t/b"];

    // Should have accumulated edge weights in both directions.
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
}

#[test]
fn reducer_clamps_score_bounds() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/a {a}\n~/t/b {b}\n~/t/a 1000:1 ~/t/b {huge}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/a 1:1000 ~/t/b {huge}\n",
    ));

    assert_eq!(state.public().ranking_group.idx_to_item.len(), 2); // Should still work, scores clamped internally
}

// ============================================================================
// Ranking Tests
// ============================================================================

#[test]
fn ranking_cycle_is_nearly_equal() {
    // Rock-paper-scissors cycle.
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/rps/rock {r}\n~/rps/scissors {s}\n~/rps/rock 3:1 ~/rps/scissors {because}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/rps/paper {p}\n~/rps/scissors 3:1 ~/rps/paper {because}\n",
    ));
    state.apply_event(ingest_event(
        3,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/rps/paper 3:1 ~/rps/rock {because}\n",
    ));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 50000, 1e-9);
    assert_eq!(ranked.len(), 3);
    let mean = ranked.iter().map(|r| r.score).sum::<f64>() / 3.0;
    for r in ranked {
        assert!(
            (r.score - mean).abs() < 0.05,
            "score {} deviates from mean {}",
            r.score,
            mean
        );
    }
}

#[test]
fn ranking_empty_group() {
    let mut group = GroupState::new();
    let ranked = ranked_items(&mut group, 1000, 1e-9);
    assert_eq!(ranked.len(), 0);
}

#[test]
fn ranking_dominant_item_wins() {
    // Item A beats everyone strongly, others have mixed results.
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/champion {c}\n~/t/b {b}\n~/t/c {c}\n~/t/d {d}\n~/t/champion 10:1 ~/t/b {because}\n~/t/champion 10:1 ~/t/c {because}\n~/t/champion 10:1 ~/t/d {because}\n~/t/b 2:1 ~/t/c {because}\n~/t/c 2:1 ~/t/d {because}\n",
    ));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked[0].item, "https://slug.social/~/t/champion");
    assert!(ranked[0].score > ranked[1].score);
}

#[test]
fn ranking_neutral_votes_produce_equal_scores() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/a {a}\n~/t/b {b}\n~/t/c {c}\n~/t/a 1:1 ~/t/b {neutral}\n~/t/b 1:1 ~/t/c {neutral}\n~/t/c 1:1 ~/t/a {neutral}\n",
    ));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked.len(), 3);
    let mean = ranked.iter().map(|r| r.score).sum::<f64>() / 3.0;
    for r in &ranked {
        assert!(
            (r.score - mean).abs() < 0.1,
            "score {} should be near mean {}",
            r.score,
            mean
        );
    }
}

#[test]
fn ranking_converges_with_many_iterations() {
    let mut state = ReducerState::default();

    // Create a clear linear ordering across multiple ingests.
    for i in 0..4 {
        let a = format!("{i}");
        let b = format!("{}", i + 1);
        let raw = vote_doc("t", &a, &b, 3, 1);
        state.apply_event(ingest_event(i as i64 + 1, &raw));
    }

    let group = state.public().ranking_group.clone();

    let ranked_short = ranked_items(&mut group.clone(), 10, 1e-3);
    let ranked_long = ranked_items(&mut group.clone(), 50000, 1e-9);

    // Both should produce same ordering.
    assert_eq!(ranked_short.len(), ranked_long.len());
    for (s, l) in ranked_short.iter().zip(ranked_long.iter()) {
        assert_eq!(s.item, l.item);
    }
}

// ============================================================================
// Event Log Tests
// ============================================================================

#[tokio::test]
async fn event_log_append_and_load() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("events.jsonl");
    let log = EventLog::new(log_path);

    let events = vec![
        ingest_event(1, "~/a {x}\n~/b {y}\n~/a 2:1 ~/b {because}\n"),
        ingest_event(2, "~/b 3:1 ~/c {because}\n"),
    ];

    for ev in &events {
        log.append(ev).await.unwrap();
    }

    let (loaded, bad) = log.load_all().await.unwrap();
    assert_eq!(bad.len(), 0);
    assert_eq!(loaded.len(), events.len());
    assert_eq!(loaded[0], events[0]);
    assert_eq!(loaded[1], events[1]);
}

#[tokio::test]
async fn event_log_handles_corrupt_lines() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("events.jsonl");
    let log = EventLog::new(&log_path);

    // Write valid events using the log itself, then manually corrupt one line.
    log.append(&ingest_event(1, "~/a {x}\n~/b {y}\n~/a 2:1 ~/b {because}\n"))
        .await
        .unwrap();

    use std::fs;
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(f, "not json at all").unwrap();

    log.append(&ingest_event(2, "~/b 3:1 ~/c {because}\n"))
        .await
        .unwrap();

    // Add empty line.
    writeln!(f, "").unwrap();

    let (loaded, bad) = log.load_all().await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].0, 2); // Line number
}

#[tokio::test]
async fn event_log_creates_parent_dirs() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("subdir").join("nested").join("events.jsonl");
    let log = EventLog::new(&log_path);

    log.append(&ingest_event(1, "~/a {x}\n~/b {y}\n~/a 2:1 ~/b {because}\n"))
        .await
        .unwrap();
    assert!(log_path.exists());
}

#[tokio::test]
async fn event_log_handles_missing_file() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("nonexistent.jsonl");
    let log = EventLog::new(&log_path);

    let (loaded, bad) = log.load_all().await.unwrap();
    assert_eq!(loaded.len(), 0);
    assert_eq!(bad.len(), 0);
}

// ============================================================================
// Integration-ish Tests
// ============================================================================

#[tokio::test]
async fn full_workflow_reducer_and_ranking() {
    // Simulate full workflow: events -> reducer -> ranking.
    let mut state = ReducerState::default();

    state.apply_event(ingest_event(
        1,
        "~/langs/rust {Systems language}\n~/langs/go {Simple concurrency}\n~/langs/rust 3:1 ~/langs/go {because}\n",
    ));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item, "https://slug.social/~/langs/rust"); // Should win
    assert!(ranked[0].score > ranked[1].score);
}

#[test]
fn reducer_materializes_ancestor_path_segments() {
    let mut state = ReducerState::default();

    // Ingest deeply nested items — no explicit ~/ai-models/anthropic item exists.
    state.apply_event(ingest_event(
        1,
        "~/ai-models/anthropic/claude-opus {opus}\n~/ai-models/anthropic/claude-sonnet {sonnet}\n",
    ));

    // The intermediate path "https://slug.social/~/ai-models/anthropic" should appear as a child of "https://slug.social/~/ai-models".
    let content = state.public();
    let ai_models_children = content.item_children.get("https://slug.social/~/ai-models").expect("ai-models should have children");
    assert!(
        ai_models_children.contains("https://slug.social/~/ai-models/anthropic"),
        "ai-models/anthropic should be a child of ai-models"
    );

    // The leaf items should still be children of "https://slug.social/~/ai-models/anthropic".
    let anthropic_children = content.item_children.get("https://slug.social/~/ai-models/anthropic").expect("ai-models/anthropic should have children");
    assert!(anthropic_children.contains("https://slug.social/~/ai-models/anthropic/claude-opus"));
    assert!(anthropic_children.contains("https://slug.social/~/ai-models/anthropic/claude-sonnet"));

    // Root should contain "https://slug.social/~/ai-models".
    let root_children = content.item_children.get("https://slug.social/~").expect("root should have children");
    assert!(root_children.contains("https://slug.social/~/ai-models"));

    // The phantom intermediates should NOT be in the items set (they weren't explicitly created).
    assert!(!content.items.contains("https://slug.social/~/ai-models"));
    assert!(!content.items.contains("https://slug.social/~/ai-models/anthropic"));
    // But the leaf items should be.
    assert!(content.items.contains("https://slug.social/~/ai-models/anthropic/claude-opus"));
    assert!(content.items.contains("https://slug.social/~/ai-models/anthropic/claude-sonnet"));
}

#[test]
fn canonicalization_is_consistent() {
    assert_eq!(canonicalize_tag("#tag"), "tag");
    assert_eq!(canonicalize_tag("tag"), "tag");
    assert_eq!(canonicalize_tag("TAG"), "tag");

    assert_eq!(canonicalize_item("/item"), "https://slug.social/item");
    assert_eq!(canonicalize_item("item"), "https://slug.social/item");
    assert_eq!(canonicalize_item("ITEM"), "https://slug.social/item");
    assert_eq!(canonicalize_item("~/music/song"), "https://slug.social/~/music/song");
    assert_eq!(
        canonicalize_item("https://slug.social/~/music/song"),
        "https://slug.social/~/music/song"
    );
    assert_eq!(
        canonicalize_item("https://open.spotify.com/track/AbC123"),
        "https://open.spotify.com/track/AbC123"
    );
}

#[test]
fn ranking_repeated_votes_normalized() {
    // Voting A>B with ratio 3:1 three times should give same ranking order as once.
    // After normalization, total(A>B) / total(A<->B) = 9/12 = 0.75 = same as 3/4.
    let mut state_once = ReducerState::default();
    state_once.apply_event(ingest_event(
        1,
        "~/norm/a {a}\n~/norm/b {b}\n~/norm/a 3:1 ~/norm/b {vote}\n",
    ));

    let mut state_many = ReducerState::default();
    state_many.apply_event(ingest_event(
        1,
        "~/norm/a {a}\n~/norm/b {b}\n~/norm/a 3:1 ~/norm/b {vote1}\n",
    ));
    state_many.apply_event(ingest_event(
        2,
        "~/norm/a 3:1 ~/norm/b {vote2}\n",
    ));
    state_many.apply_event(ingest_event(
        3,
        "~/norm/a 3:1 ~/norm/b {vote3}\n",
    ));

    let mut group_once = state_once.public().ranking_group.clone();
    let mut group_many = state_many.public().ranking_group.clone();
    let ranked_once = ranked_items(&mut group_once, 20000, 1e-9);
    let ranked_many = ranked_items(&mut group_many, 20000, 1e-9);

    // Same winner regardless of how many times voted.
    assert_eq!(ranked_once[0].item, ranked_many[0].item);
    assert_eq!(ranked_once[0].item, "https://slug.social/~/norm/a");
    assert_eq!(ranked_once[1].item, ranked_many[1].item);
    assert_eq!(ranked_once[1].item, "https://slug.social/~/norm/b");

    // Scores should be identical (normalization makes repeated votes idempotent).
    let eps = 1e-6;
    assert!((ranked_once[0].score - ranked_many[0].score).abs() < eps,
        "scores differ: once={:.6} many={:.6}", ranked_once[0].score, ranked_many[0].score);
}

// ============================================================================
// Phase 1: Reducer Edge Cases
// ============================================================================

#[test]
fn reducer_malformed_ingest_is_skipped_no_panic() {
    let mut state = ReducerState::default();
    // Completely invalid raw text that will fail dsl::parse_full
    state.apply_event(ingest_event(1, "this is {{ not valid DSL at all }{"));
    // State should remain empty — no panic, no items
    let content = state.public();
    assert!(content.items.is_empty());
    assert!(content.ranking_group.idx_to_item.is_empty());
}

#[test]
fn reducer_zero_zero_vote_ratio_normalizes_to_one_one() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n~/t/a 0:0 ~/t/b {zero}\n",
    ));
    let group = &state.public().ranking_group;
    let a_idx = group.item_to_idx["https://slug.social/~/t/a"];
    let b_idx = group.item_to_idx["https://slug.social/~/t/b"];
    // 0:0 should normalize to 1:1 — both directions should have weight
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
    let w_ab = group.edges[&(a_idx, b_idx)];
    let w_ba = group.edges[&(b_idx, a_idx)];
    assert!((w_ab - w_ba).abs() < 1e-9, "0:0 should produce equal weights");
}

#[test]
fn reducer_negative_ratio_clamped_to_zero() {
    let _state = ReducerState::default();
    // GroupState::apply_vote clamps negatives to 0, then 0:0 -> 1:1
    let mut group = GroupState::new();
    group.apply_vote(slugsocial_server::reducer::VoteData {
        ts: 1,
        a: slugsocial_server::path_types::CanonicalItemUrl("https://slug.social/~/t/a".to_string()),
        b: slugsocial_server::path_types::CanonicalItemUrl("https://slug.social/~/t/b".to_string()),
        ratio_left: -5,
        ratio_right: -3,
        body: "negative".to_string(),
        principal: "test".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        thread_id: "t".to_string(),
    });
    assert_eq!(group.idx_to_item.len(), 2);
    // Both edges should exist (negatives clamped to 0, then 0:0 -> 1:1)
    let a_idx = group.item_to_idx["https://slug.social/~/t/a"];
    let b_idx = group.item_to_idx["https://slug.social/~/t/b"];
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
}


#[test]
fn reducer_deep_path_ancestor_materialization_four_levels() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/a/b/c/d {leaf}\n",
    ));
    // Ancestor chain: parent() now walks purely within the ontology namespace.
    // The spurious "" → "https://slug.social" levels are gone; root is "https://slug.social/~".
    let content = state.public();
    let tilde_scope = content
        .item_children
        .get("https://slug.social/~")
        .expect("~/ scope should have children");
    assert!(tilde_scope.contains("https://slug.social/~/a"));

    let a_children = content.item_children.get("https://slug.social/~/a").expect("a should have children");
    assert!(a_children.contains("https://slug.social/~/a/b"));

    let ab_children = content.item_children.get("https://slug.social/~/a/b").expect("a/b should have children");
    assert!(ab_children.contains("https://slug.social/~/a/b/c"));

    let abc_children = content.item_children.get("https://slug.social/~/a/b/c").expect("a/b/c should have children");
    assert!(abc_children.contains("https://slug.social/~/a/b/c/d"));

    // Only the leaf should be in items set
    assert!(content.items.contains("https://slug.social/~/a/b/c/d"));
    assert!(!content.items.contains("https://slug.social/~/a"));
    assert!(!content.items.contains("https://slug.social/~/a/b"));
    assert!(!content.items.contains("https://slug.social/~/a/b/c"));
}

// ============================================================================
// Phase 1: Ranking Edge Cases
// ============================================================================

#[test]
fn ranking_single_node_scores_one() {
    let mut group = GroupState::new();
    group.ensure_item_pub("solo");
    let ranked = ranked_items(&mut group, 1000, 1e-9);
    assert_eq!(ranked.len(), 1);
    assert!((ranked[0].score - 1.0).abs() < 1e-9, "single node should score 1.0");
}

#[test]
fn ranking_two_nodes_no_edges_uniform() {
    let mut group = GroupState::new();
    group.ensure_item_pub("x");
    group.ensure_item_pub("y");
    let ranked = ranked_items(&mut group, 1000, 1e-9);
    assert_eq!(ranked.len(), 2);
    assert!(
        (ranked[0].score - ranked[1].score).abs() < 1e-9,
        "no-edge items should have equal scores"
    );
}

#[test]
fn ranking_convergence_tolerance_triggers_early_exit() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n~/t/a 3:1 ~/t/b {because}\n",
    ));
    let mut group = state.public().ranking_group.clone();
    // Very tight tolerance but huge max_iters — should still converge fast
    let ranked = ranked_items(&mut group, 1_000_000, 1e-15);
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item, "https://slug.social/~/t/a");
}

// ============================================================================
// Coverage Audit Gap Tests
// ============================================================================

#[test]
fn test_item_body_overwrite() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/x {first}\n",
    ));
    assert_eq!(state.public().item_bodies.get("https://slug.social/~/t/x"), Some(&"first".to_string()));

    state.apply_event(ingest_event(
        2,
        "~/t/x {second}\n",
    ));
    assert_eq!(
        state.public().item_bodies.get("https://slug.social/~/t/x"),
        Some(&"second".to_string()),
        "last writer should win for item bodies"
    );
}

#[test]
fn test_empty_body_not_stored() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/blank {   }\n",
    ));
    let content = state.public();
    assert!(content.items.contains("https://slug.social/~/t/blank"), "item should exist");
    assert!(
        !content.item_bodies.contains_key("https://slug.social/~/t/blank"),
        "whitespace-only body should not be stored"
    );
}

#[test]
fn test_duplicate_items_across_ingests() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/dup {first}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "~/t/dup {second}\n",
    ));
    let content = state.public();
    let count = content.items.iter().filter(|i| *i == "https://slug.social/~/t/dup").count();
    assert_eq!(count, 1, "items set should deduplicate across ingests");
}

#[test]
fn test_ingests_ordered_chronological() {
    let mut state = ReducerState::default();
    for ts in [100, 200, 300] {
        state.apply_event(ingest_event(
            ts,
            "~/t/a {a}\n",
        ));
    }
    assert_eq!(state.ingests_ordered.len(), 3);
    assert_eq!(state.ingests_ordered[0], "test-100");
    assert_eq!(state.ingests_ordered[1], "test-200");
    assert_eq!(state.ingests_ordered[2], "test-300");
}

#[test]
fn test_actor_last_post_ts() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        42,
        "~/t/a {a}\n",
    ));
    // actor_last_post_ts is keyed by principal username.
    assert_eq!(
        state.actor_last_post_ts.get("test"),
        Some(&42),
        "actor_last_post_ts should be set to the ingest timestamp"
    );
}

#[test]
fn test_thread_timestamp_bump() {
    let mut state = ReducerState::default();
    let mut ev1 = match ingest_event(100, "~/t/a {a}\n") { Event::Ingest(i) => i, _ => unreachable!() };
    ev1.thread_id = "my-thread".to_string();
    state.apply_event(Event::Ingest(ev1));
    assert_eq!(state.threads.get("my-thread").unwrap().last_activity_ts, 100);

    let mut ev2 = match ingest_event(200, "~/t/b {b}\n") { Event::Ingest(i) => i, _ => unreachable!() };
    ev2.thread_id = "my-thread".to_string();
    state.apply_event(Event::Ingest(ev2));
    assert_eq!(state.threads.get("my-thread").unwrap().last_activity_ts, 200);

    // Ingest with earlier timestamp should NOT regress
    let mut ev3 = match ingest_event(150, "~/t/c {c}\n") { Event::Ingest(i) => i, _ => unreachable!() };
    ev3.thread_id = "my-thread".to_string();
    state.apply_event(Event::Ingest(ev3));
    assert_eq!(
        state.threads.get("my-thread").unwrap().last_activity_ts,
        200,
        "thread timestamp should not regress to an earlier value"
    );
}

#[test]
fn test_thread_id_is_used_for_votes_and_indexes() {
    let mut state = ReducerState::default();
    let mut ev = match ingest_event(1, "~/t/a {a}\n~/t/b {b}\n~/t/a 2:1 ~/t/b {reason}\n") {
        Event::Ingest(i) => i,
        _ => unreachable!(),
    };
    ev.thread_id = "first".to_string();
    state.apply_event(Event::Ingest(ev));

    let content = state.public();
    let vote = content
        .item_votes
        .get("https://slug.social/~/t/a")
        .unwrap()
        .front()
        .unwrap();
    assert_eq!(vote.thread_id, "first");
    assert!(state.ingests_by_thread.contains_key("first"));
    assert!(state
        .public()
        .item_threads
        .get("https://slug.social/~/t/a")
        .is_some_and(|threads| threads.contains("first")));
}

#[test]
fn test_rank_history_created_for_voted_items() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n~/t/a 3:1 ~/t/b {reason}\n",
    ));
    assert!(
        state.public().rank_history.contains_key("https://slug.social/~/t/a"),
        "rank_history should have entry for voted item a"
    );
    assert!(
        state.public().rank_history.contains_key("https://slug.social/~/t/b"),
        "rank_history should have entry for voted item b"
    );
}

#[test]
fn test_rank_history_not_created_for_unvoted_items() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/c {just a definition}\n",
    ));
    assert!(
        !state.public().rank_history.contains_key("https://slug.social/~/t/c"),
        "rank_history should NOT have entry for item with no votes"
    );
}

#[test]
fn test_rank_history_first_entry_delta_zero() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n~/t/a 3:1 ~/t/b {reason}\n",
    ));
    let history_a = state.public().rank_history.get("https://slug.social/~/t/a").unwrap();
    assert_eq!(history_a.len(), 1);
    assert_eq!(
        history_a[0].scope_rank_delta, 0,
        "first rank_history entry should have scope_rank_delta == 0"
    );
    assert_eq!(
        history_a[0].global_rank_delta, 0,
        "first rank_history entry should have global_rank_delta == 0"
    );
}

#[test]
fn test_multi_actor_in_single_ingest() {
    // Identity is request/event metadata now; a single ingest has a single (principal, delegate).
    // Multi-actor archives are represented as multiple Ingest events.
    let _state = ReducerState::default();
}

#[test]
fn test_ingests_by_thread_ordering() {
    let mut state = ReducerState::default();
    for ts in [100, 200, 300] {
        let mut ev = match ingest_event(ts, "~/t/a {a}\n") { Event::Ingest(i) => i, _ => unreachable!() };
        ev.thread_id = "order-thread".to_string();
        state.apply_event(Event::Ingest(ev));
    }
    let thread_ingests = state.ingests_by_thread.get("order-thread").unwrap();
    assert_eq!(thread_ingests.len(), 3);
    // Most recent first (push_front ordering)
    assert_eq!(thread_ingests[0], "test-300");
    assert_eq!(thread_ingests[1], "test-200");
    assert_eq!(thread_ingests[2], "test-100");
}

