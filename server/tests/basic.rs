use slugsocial_server::{
    event_log::EventLog,
    canonical_path::{canonicalize_item, canonicalize_tag},
    events::{Event, GrantAdded, Ingest, RoomCreated},
    ranking::ranked_items,
    reducer::{GroupState, ReducerState, ScopeId},
};


use slugsocial_server::path_types::ItemId;

use tempfile::TempDir;

#[inline]
fn item_id(s: &str) -> ItemId {
    ItemId::parse(s).unwrap()
}

fn ingest_event(ts: i64, raw: &str) -> Event {
    Event::Ingest(Ingest {
        ts,
        // Stable ID for deterministic tests.
        id: format!("test-{ts}"),
        raw: raw.to_string(),
        principal: "test".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        room_id: "public".to_string(),
        thread_tag: "t".to_string(),
    })
}

fn vote_doc(tag: &str, a: &str, b: &str, left: i32, right: i32) -> String {
    format!(
        "~/{tag}/{a} {{body a}}\n~/{tag}/{b} {{body b}}\n{{because test}}\n~/{tag}/{a} {left}:{right} ~/{tag}/{b}\n"
    )
}

// ============================================================================
// Reducer Tests
// ============================================================================

#[test]
fn reducer_external_namespace_ranking() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         -/github.com/iss/1 { one }\n\
         -/github.com/iss/2 { two }\n\
         { because }\n         -/github.com/iss/1 2:1 -/github.com/iss/2\n",
    ));

    let content = state.public();
    let g = &content.ranking_group;
    assert_eq!(g.idx_to_item.len(), 2);

    let parent = slugsocial_server::path_types::ItemId::parse("https://github.com/iss").unwrap();
    let children = slugsocial_server::scope_rank::build_children_rankings(content, &parent);
    assert_eq!(children.component_rankings.len(), 1);
    let names: Vec<&str> = children.component_rankings[0]
        .ranked
        .iter()
        .map(|r| r.item.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "https://github.com/iss/1",
            "https://github.com/iss/2"
        ]
    );
}

#[test]
fn reducer_and_ranking_linear_chain() {
    // Prefer /a over /b over /c.
    let mut state = ReducerState::default();

    // First ingest: define items + vote a > b.
    state.apply_event(ingest_event(1, "~/t/a {a}\n~/t/b {b}\n{because}\n~/t/a 3:1 ~/t/b\n"));
    // Second ingest: define c + vote b > c.
    state.apply_event(ingest_event(2, "~/t/c {c}\n{because}\n~/t/b 3:1 ~/t/c\n"));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].item.as_str(), "https://slug.social/~/t/a");
    assert_eq!(ranked[1].item.as_str(), "https://slug.social/~/t/b");
    assert_eq!(ranked[2].item.as_str(), "https://slug.social/~/t/c");
}

#[test]
fn reducer_canonicalizes_identifiers() {
    let mut state = ReducerState::default();

    // Mix of formats across ingests (case + sigils).
    state.apply_event(ingest_event(
        1,
        "~/Tag/Item-A {x}\n~/Tag/Item-B {y}\n{because}\n~/Tag/Item-A 2:1 ~/Tag/Item-B\n",
    ));
    state.apply_event(ingest_event(
        2,
        "{because}\n~/TAG/ITEM-A 2:1 ~/TAG/ITEM-B\n",
    ));
    state.apply_event(ingest_event(
        3,
        "{because}\n~/tag/item-a 2:1 ~/tag/item-b\n",
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
    assert!(content.items.contains(&item_id("https://slug.social/~/t/test-item")));
    assert_eq!(
        content.item_bodies.get(&item_id("https://slug.social/~/t/test-item")),
        Some(&"Description here".to_string())
    );
    assert!(content
        .item_children
        .get(&item_id("https://slug.social/~/t"))
        .map(|c| c.contains(&item_id("https://slug.social/~/t/test-item")))
        .unwrap_or(false));
}

#[test]
fn reducer_indexes_item_threads_and_vote_thread() {
    let mut state = ReducerState::default();
    // Thread routing is metadata (ingest.thread_tag), not parsed from raw.
    let mut ev = match ingest_event(1, "~/sorts/insertion { O(n^2) }\n~/sorts/mergesort { O(n log n) }\n{ simpler for small n }\n~/sorts/insertion 3:1 ~/sorts/mergesort\n") {
        Event::Ingest(i) => i,
        _ => unreachable!(),
    };
    ev.thread_tag = "sorting-hat".to_string();
    state.apply_event(Event::Ingest(ev));

    let content = state.public();
    let threads_for_insertion = content.item_threads.get(&item_id("https://slug.social/~/sorts/insertion")).unwrap();
    assert!(threads_for_insertion.contains("sorting-hat"));
    let threads_for_mergesort = content.item_threads.get(&item_id("https://slug.social/~/sorts/mergesort")).unwrap();
    assert!(threads_for_mergesort.contains("sorting-hat"));

    let vote = content.item_votes.get(&item_id("https://slug.social/~/sorts/insertion")).unwrap().front().unwrap();
    assert_eq!(vote.thread_tag, "sorting-hat");
}

#[test]
fn reducer_aggregates_multiple_votes() {
    let mut state = ReducerState::default();

    // Multiple votes between same pair should accumulate weights.
    for ts in 1..=3 {
        state.apply_event(ingest_event(ts, &vote_doc("t", "a", "b", 2, 1)));
    }

    let group = &state.public().ranking_group;
    let a_idx = group.item_to_idx[&item_id("https://slug.social/~/t/a")];
    let b_idx = group.item_to_idx[&item_id("https://slug.social/~/t/b")];

    // Should have accumulated edge weights in both directions.
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
}

#[test]
fn reducer_clamps_score_bounds() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/a {a}\n~/t/b {b}\n{huge}\n~/t/a 1000:1 ~/t/b\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n{huge}\n~/t/a 1:1000 ~/t/b\n",
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
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/rps/rock {r}\n~/rps/scissors {s}\n{because}\n~/rps/rock 3:1 ~/rps/scissors\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/rps/paper {p}\n{because}\n~/rps/scissors 3:1 ~/rps/paper\n",
    ));
    state.apply_event(ingest_event(
        3,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n{because}\n~/rps/paper 3:1 ~/rps/rock\n",
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
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/champion {c}\n~/t/b {b}\n~/t/c {c}\n~/t/d {d}\n{because}\n~/t/champion 10:1 ~/t/b\n{because}\n~/t/champion 10:1 ~/t/c\n{because}\n~/t/champion 10:1 ~/t/d\n{because}\n~/t/b 2:1 ~/t/c\n{because}\n~/t/c 2:1 ~/t/d\n",
    ));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked[0].item.as_str(), "https://slug.social/~/t/champion");
    assert!(ranked[0].score > ranked[1].score);
}

#[test]
fn ranking_neutral_votes_produce_equal_scores() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/a {a}\n~/t/b {b}\n~/t/c {c}\n{neutral}\n~/t/a 1:1 ~/t/b\n{neutral}\n~/t/b 1:1 ~/t/c\n{neutral}\n~/t/c 1:1 ~/t/a\n",
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
        ingest_event(1, "~/a {x}\n~/b {y}\n{because}\n~/a 2:1 ~/b\n"),
        ingest_event(2, "{because}\n~/b 3:1 ~/c\n"),
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
    log.append(&ingest_event(1, "~/a {x}\n~/b {y}\n{because}\n~/a 2:1 ~/b\n"))
        .await
        .unwrap();

    use std::fs;
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(f, "not json at all").unwrap();

    log.append(&ingest_event(2, "{because}\n~/b 3:1 ~/c\n"))
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

    log.append(&ingest_event(1, "~/a {x}\n~/b {y}\n{because}\n~/a 2:1 ~/b\n"))
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
        "~/langs/rust {Systems language}\n~/langs/go {Simple concurrency}\n{because}\n~/langs/rust 3:1 ~/langs/go\n",
    ));

    let mut group = state.public().ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item.as_str(), "https://slug.social/~/langs/rust"); // Should win
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
    let ai_models_children = content.item_children.get(&item_id("https://slug.social/~/ai-models")).expect("ai-models should have children");
    assert!(
        ai_models_children.contains(&item_id("https://slug.social/~/ai-models/anthropic")),
        "ai-models/anthropic should be a child of ai-models"
    );

    // The leaf items should still be children of "https://slug.social/~/ai-models/anthropic".
    let anthropic_children = content.item_children.get(&item_id("https://slug.social/~/ai-models/anthropic")).expect("ai-models/anthropic should have children");
    assert!(anthropic_children.contains(&item_id("https://slug.social/~/ai-models/anthropic/claude-opus")));
    assert!(anthropic_children.contains(&item_id("https://slug.social/~/ai-models/anthropic/claude-sonnet")));

    // Root should contain "https://slug.social/~/ai-models".
    let root_children = content.item_children.get(&item_id("https://slug.social/~")).expect("root should have children");
    assert!(root_children.contains(&item_id("https://slug.social/~/ai-models")));

    // The phantom intermediates should NOT be in the items set (they weren't explicitly created).
    assert!(!content.items.contains(&item_id("https://slug.social/~/ai-models")));
    assert!(!content.items.contains(&item_id("https://slug.social/~/ai-models/anthropic")));
    // But the leaf items should be.
    assert!(content.items.contains(&item_id("https://slug.social/~/ai-models/anthropic/claude-opus")));
    assert!(content.items.contains(&item_id("https://slug.social/~/ai-models/anthropic/claude-sonnet")));
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
        "~/norm/a {a}\n~/norm/b {b}\n{vote}\n~/norm/a 3:1 ~/norm/b\n",
    ));

    let mut state_many = ReducerState::default();
    state_many.apply_event(ingest_event(
        1,
        "~/norm/a {a}\n~/norm/b {b}\n{vote1}\n~/norm/a 3:1 ~/norm/b\n",
    ));
    state_many.apply_event(ingest_event(
        2,
        "{vote2}\n~/norm/a 3:1 ~/norm/b\n",
    ));
    state_many.apply_event(ingest_event(
        3,
        "{vote3}\n~/norm/a 3:1 ~/norm/b\n",
    ));

    let mut group_once = state_once.public().ranking_group.clone();
    let mut group_many = state_many.public().ranking_group.clone();
    let ranked_once = ranked_items(&mut group_once, 20000, 1e-9);
    let ranked_many = ranked_items(&mut group_many, 20000, 1e-9);

    // Same winner regardless of how many times voted.
    assert_eq!(ranked_once[0].item, ranked_many[0].item);
    assert_eq!(ranked_once[0].item.as_str(), "https://slug.social/~/norm/a");
    assert_eq!(ranked_once[1].item, ranked_many[1].item);
    assert_eq!(ranked_once[1].item.as_str(), "https://slug.social/~/norm/b");

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
    // DSL-looking line that fails parse (unclosed item body), not plain prose
    state.apply_event(ingest_event(1, "~/t/a { unclosed "));
    // State should remain empty — no panic, no items, no ingest id leakage
    let content = state.public();
    assert!(content.items.is_empty());
    assert!(content.ranking_group.idx_to_item.is_empty());
    assert!(
        state.ingests_by_id.is_empty(),
        "malformed ingest must not be recorded in ingests_by_id"
    );
}

#[test]
fn dsl_parse_rejects_zero_zero_vote_ratio() {
    let err = slugsocial_server::dsl::parse_full(
        "~/t/a {a}\n~/t/b {b}\n{zero}\n~/t/a 0:0 ~/t/b\n",
    )
    .expect_err("0:0 vote must be rejected by the parser");
    let msg = match err {
        slugsocial_server::dsl::DslError::Parse(m) => m,
    };
    assert!(
        msg.contains("0:0"),
        "expected message about invalid 0:0 ratio, got: {msg}"
    );

    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n{zero}\n~/t/a 0:0 ~/t/b\n",
    ));
    let content = state.public();
    assert!(
        content.items.is_empty() && content.ranking_group.idx_to_item.is_empty(),
        "parse failure must skip entire ingest; got items={:?}",
        content.items.len(),
    );
}

#[test]
fn reducer_negative_ratio_clamped_to_zero() {
    let _state = ReducerState::default();
    // GroupState::apply_vote clamps negatives to 0, then 0:0 -> 1:1
    let mut group = GroupState::new();
    group.apply_vote(slugsocial_server::reducer::VoteData {
        ts: 1,
        a: slugsocial_server::path_types::ItemId::parse("https://slug.social/~/t/a").unwrap(),
        b: slugsocial_server::path_types::ItemId::parse("https://slug.social/~/t/b").unwrap(),
        ratio_left: -5,
        ratio_right: -3,
        body: "negative".to_string(),
        principal: "test".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        thread_tag: "t".to_string(),
    });
    assert_eq!(group.idx_to_item.len(), 2);
    // Both edges should exist (negatives clamped to 0, then 0:0 -> 1:1)
    let a_idx = group.item_to_idx[&item_id("https://slug.social/~/t/a")];
    let b_idx = group.item_to_idx[&item_id("https://slug.social/~/t/b")];
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
        .get(&item_id("https://slug.social/~"))
        .expect("~/ scope should have children");
    assert!(tilde_scope.contains(&item_id("https://slug.social/~/a")));

    let a_children = content.item_children.get(&item_id("https://slug.social/~/a")).expect("a should have children");
    assert!(a_children.contains(&item_id("https://slug.social/~/a/b")));

    let ab_children = content.item_children.get(&item_id("https://slug.social/~/a/b")).expect("a/b should have children");
    assert!(ab_children.contains(&item_id("https://slug.social/~/a/b/c")));

    let abc_children = content.item_children.get(&item_id("https://slug.social/~/a/b/c")).expect("a/b/c should have children");
    assert!(abc_children.contains(&item_id("https://slug.social/~/a/b/c/d")));

    // Only the leaf should be in items set
    assert!(content.items.contains(&item_id("https://slug.social/~/a/b/c/d")));
    assert!(!content.items.contains(&item_id("https://slug.social/~/a")));
    assert!(!content.items.contains(&item_id("https://slug.social/~/a/b")));
    assert!(!content.items.contains(&item_id("https://slug.social/~/a/b/c")));
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
        "~/t/a {a}\n~/t/b {b}\n{because}\n~/t/a 3:1 ~/t/b\n",
    ));
    let mut group = state.public().ranking_group.clone();
    // Very tight tolerance but huge max_iters — should still converge fast
    let ranked = ranked_items(&mut group, 1_000_000, 1e-15);
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item.as_str(), "https://slug.social/~/t/a");
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
    assert_eq!(state.public().item_bodies.get(&item_id("https://slug.social/~/t/x")), Some(&"first".to_string()));

    state.apply_event(ingest_event(
        2,
        "~/t/x {second}\n",
    ));
    assert_eq!(
        state.public().item_bodies.get(&item_id("https://slug.social/~/t/x")),
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
    assert!(content.items.contains(&item_id("https://slug.social/~/t/blank")), "item should exist");
    assert!(
        !content.item_bodies.contains_key(&item_id("https://slug.social/~/t/blank")),
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
    let count = content.items.iter().filter(|i| i.as_str() == "https://slug.social/~/t/dup").count();
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
    let key = (ScopeId::Public, "my-thread".to_string());
    let mut ev1 = match ingest_event(100, "~/t/a {a}\n") { Event::Ingest(i) => i, _ => unreachable!() };
    ev1.thread_tag = "my-thread".to_string();
    state.apply_event(Event::Ingest(ev1));
    assert_eq!(state.forum_threads.get(&key).unwrap().last_activity_ts, 100);

    let mut ev2 = match ingest_event(200, "~/t/b {b}\n") { Event::Ingest(i) => i, _ => unreachable!() };
    ev2.thread_tag = "my-thread".to_string();
    state.apply_event(Event::Ingest(ev2));
    assert_eq!(state.forum_threads.get(&key).unwrap().last_activity_ts, 200);

    // Ingest with earlier timestamp should NOT regress
    let mut ev3 = match ingest_event(150, "~/t/c {c}\n") { Event::Ingest(i) => i, _ => unreachable!() };
    ev3.thread_tag = "my-thread".to_string();
    state.apply_event(Event::Ingest(ev3));
    assert_eq!(
        state.forum_threads.get(&key).unwrap().last_activity_ts,
        200,
        "thread timestamp should not regress to an earlier value"
    );
}

#[test]
fn test_thread_id_is_used_for_votes_and_indexes() {
    let mut state = ReducerState::default();
    let mut ev = match ingest_event(1, "~/t/a {a}\n~/t/b {b}\n{reason}\n~/t/a 2:1 ~/t/b\n") {
        Event::Ingest(i) => i,
        _ => unreachable!(),
    };
    ev.thread_tag = "first".to_string();
    state.apply_event(Event::Ingest(ev));

    let content = state.public();
    let vote = content
        .item_votes
        .get(&item_id("https://slug.social/~/t/a"))
        .unwrap()
        .front()
        .unwrap();
    assert_eq!(vote.thread_tag, "first");
    assert!(state.ingests_by_scope_thread.contains_key(&(ScopeId::Public, "first".to_string())));
    assert!(state
        .public()
        .item_threads
        .get(&item_id("https://slug.social/~/t/a"))
        .is_some_and(|threads| threads.contains("first")));
}

#[test]
fn test_rank_history_created_for_voted_items() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n{reason}\n~/t/a 3:1 ~/t/b\n",
    ));
    assert!(
        state.public().rank_history.contains_key(&item_id("https://slug.social/~/t/a")),
        "rank_history should have entry for voted item a"
    );
    assert!(
        state.public().rank_history.contains_key(&item_id("https://slug.social/~/t/b")),
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
        !state.public().rank_history.contains_key(&item_id("https://slug.social/~/t/c")),
        "rank_history should NOT have entry for item with no votes"
    );
}

#[test]
fn test_rank_history_first_entry_delta_zero() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "~/t/a {a}\n~/t/b {b}\n{reason}\n~/t/a 3:1 ~/t/b\n",
    ));
    let history_a = state.public().rank_history.get(&item_id("https://slug.social/~/t/a")).unwrap();
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
        ev.thread_tag = "order-thread".to_string();
        state.apply_event(Event::Ingest(ev));
    }
    let thread_ingests = state
        .ingests_by_scope_thread
        .get(&(ScopeId::Public, "order-thread".to_string()))
        .unwrap();
    assert_eq!(thread_ingests.len(), 3);
    // Most recent first (push_front ordering)
    assert_eq!(thread_ingests[0], "test-300");
    assert_eq!(thread_ingests[1], "test-200");
    assert_eq!(thread_ingests[2], "test-100");
}

#[test]
fn posts_by_actor_indexes_and_profile_visibility() {
    let mut state = ReducerState::default();
    state.apply_event(Event::RoomCreated(RoomCreated {
        ts: 1,
        room_id: "ab12cd/private-room".to_string(),
        slug: "private-room".to_string(),
        owner: "alice".to_string(),
    }));
    state.apply_event(Event::GrantAdded(GrantAdded {
        ts: 2,
        room_id: "ab12cd/private-room".to_string(),
        username: "bob".to_string(),
        capabilities: vec![slugsocial_server::events::ThreadCapability::View],
        granted_by: "alice".to_string(),
    }));

    let mut ev1 = match ingest_event(10, "~/pub/a {x}\n") {
        Event::Ingest(i) => i,
        _ => unreachable!(),
    };
    ev1.principal = "tommy".to_string();
    ev1.thread_tag = "demo".to_string();
    state.apply_event(Event::Ingest(ev1));

    let mut ev2 = match ingest_event(20, "~/priv/x {secret}\n") {
        Event::Ingest(i) => i,
        _ => unreachable!(),
    };
    ev2.principal = "tommy".to_string();
    ev2.room_id = "ab12cd/private-room".to_string();
    ev2.thread_tag = "secret-thread".to_string();
    state.apply_event(Event::Ingest(ev2));

    assert_eq!(state.posts_by_actor.get("tommy").map(|q| q.len()), Some(2));

    let anon = state.visible_posts_for_actor("tommy", None);
    assert_eq!(anon.len(), 1);
    assert!(state.ingests_by_id[&anon[0]].room_id == "public");

    let bob = state.visible_posts_for_actor("tommy", Some("bob"));
    assert_eq!(bob.len(), 2);
}

// ============================================================================
// Feed redacted-post filtering regression tests
// ============================================================================

/// Helper: simulate the feed filtering logic from `GetFeed` in rpc.rs.
/// Returns (total, posts) where `total` is the count of visible (non-redacted)
/// matching posts and `posts` is the page of IDs up to `limit`.
fn feed_query(state: &ReducerState, cutoff: i64, limit: usize) -> (usize, Vec<String>) {
    let matching: Vec<&str> = state.ingests_ordered.iter().rev()
        .map(|id| id.as_str())
        .take_while(|id| state.ingests_by_id.get(*id).map_or(false, |ing| ing.ts > cutoff))
        .filter(|id| {
            state.ingests_by_id.get(*id).is_some_and(|ing| {
                let scope = slugsocial_server::reducer::scope_from_room_wire(&ing.room_id);
                match scope {
                    ScopeId::Public => true,
                    ScopeId::Room(_) => false,
                }
            })
        })
        .filter(|id| !state.redacted_posts.contains(*id))
        .collect();
    let total = matching.len();
    let posts: Vec<String> = matching.into_iter()
        .take(limit)
        .map(|id| id.to_string())
        .collect();
    (total, posts)
}

#[test]
fn feed_total_excludes_redacted_posts() {
    use slugsocial_server::events::PostRedacted;

    let mut state = ReducerState::default();
    // Create 5 posts with increasing timestamps.
    for ts in 1..=5 {
        state.apply_event(ingest_event(ts, "~/t/a {a}\n"));
    }
    assert_eq!(state.ingests_ordered.len(), 5);

    // Redact posts 2 and 4.
    state.apply_event(Event::PostRedacted(PostRedacted {
        ts: 100,
        post_id: "test-2".to_string(),
        principal: "test".to_string(),
    }));
    state.apply_event(Event::PostRedacted(PostRedacted {
        ts: 101,
        post_id: "test-4".to_string(),
        principal: "test".to_string(),
    }));

    // Feed with cutoff=0 (all posts), limit=10.
    let (total, posts) = feed_query(&state, 0, 10);
    assert_eq!(total, 3, "total must exclude redacted posts");
    assert_eq!(posts.len(), 3, "returned posts must exclude redacted posts");
    assert!(!posts.contains(&"test-2".to_string()), "redacted post test-2 must not appear");
    assert!(!posts.contains(&"test-4".to_string()), "redacted post test-4 must not appear");
}

#[test]
fn feed_limit_applied_after_redacted_filter() {
    use slugsocial_server::events::PostRedacted;

    let mut state = ReducerState::default();
    // Create 6 posts.
    for ts in 1..=6 {
        state.apply_event(ingest_event(ts, "~/t/a {a}\n"));
    }

    // Redact the 3 most recent posts (test-6, test-5, test-4).
    for id in ["test-6", "test-5", "test-4"] {
        state.apply_event(Event::PostRedacted(PostRedacted {
            ts: 200,
            post_id: id.to_string(),
            principal: "test".to_string(),
        }));
    }

    // Request limit=2. The 3 most recent are redacted, so the feed should
    // still return 2 posts from the remaining 3 (test-3, test-2, test-1).
    // BUG before fix: .take(limit) was applied before the redacted filter,
    // so .take(2) would grab test-6 and test-5, both redacted, yielding 0 posts.
    let (total, posts) = feed_query(&state, 0, 2);
    assert_eq!(total, 3, "total must count only non-redacted posts");
    assert_eq!(
        posts.len(), 2,
        "limit should be applied AFTER filtering redacted posts, not before"
    );
    // The 2 most recent non-redacted posts (reverse chronological order).
    assert_eq!(posts[0], "test-3");
    assert_eq!(posts[1], "test-2");
}

