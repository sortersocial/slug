use slugsocial_server::{
    event_log::EventLog,
    events::{canonicalize_aspect, canonicalize_item, canonicalize_tag, Event, Ingest},
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
        voter_key_id: "test".to_string(),
        // Required field; reducer will canonicalize and may be overridden by `@actor` in raw.
        actor: "test".to_string(),
    })
}

fn vote_doc(tag: &str, aspect: &str, a: &str, b: &str, left: i32, right: i32) -> String {
    format!(
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:{aspect}\n~/{tag}/{a} {{body a}}\n~/{tag}/{b} {{body b}}\n~/{tag}/{a} {left}:{right} ~/{tag}/{b} {{because test}}\n"
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
    state.apply_event(ingest_event(1, "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/t/a {a}\n~/t/b {b}\n~/t/a 3:1 ~/t/b {because}\n"));
    // Second ingest: define c + vote b > c.
    state.apply_event(ingest_event(2, "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/t/c {c}\n~/t/b 3:1 ~/t/c {because}\n"));

    let mut group = state.ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].item, "t/a");
    assert_eq!(ranked[1].item, "t/b");
    assert_eq!(ranked[2].item, "t/c");
}

#[test]
fn reducer_canonicalizes_identifiers() {
    let mut state = ReducerState::default();

    // Mix of formats across ingests (case + sigils).
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:Aspect\n~/Tag/Item-A {x}\n~/Tag/Item-B {y}\n~/Tag/Item-A 2:1 ~/Tag/Item-B {because}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:ASPECT\n~/TAG/ITEM-A 2:1 ~/TAG/ITEM-B {because}\n",
    ));
    state.apply_event(ingest_event(
        3,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:aspect\n~/tag/item-a 2:1 ~/tag/item-b {because}\n",
    ));

    assert_eq!(state.ranking_group.idx_to_item.len(), 2); // Should dedupe to 2 items
}

#[test]
fn reducer_handles_item_and_body_from_ingest() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t/test-item {Description here}\n",
    ));

    assert!(state.items.contains("t/test-item"));
    assert_eq!(
        state.item_bodies.get("t/test-item"),
        Some(&"Description here".to_string())
    );
    assert!(state.item_children.get("t").map(|c| c.contains("t/test-item")).unwrap_or(false));
}

#[test]
fn reducer_aggregates_multiple_votes() {
    let mut state = ReducerState::default();

    // Multiple votes between same pair should accumulate weights.
    for ts in 1..=3 {
        state.apply_event(ingest_event(ts, &vote_doc("t", "x", "a", "b", 2, 1)));
    }

    let group = &state.ranking_group;
    let a_idx = group.item_to_idx["t/a"];
    let b_idx = group.item_to_idx["t/b"];

    // Should have accumulated edge weights in both directions.
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
}

#[test]
fn reducer_clamps_score_bounds() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/t/a {a}\n~/t/b {b}\n~/t/a 1000:1 ~/t/b {huge}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/t/a 1:1000 ~/t/b {huge}\n",
    ));

    assert_eq!(state.ranking_group.idx_to_item.len(), 2); // Should still work, scores clamped internally
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
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/rps/rock {r}\n~/rps/scissors {s}\n~/rps/rock 3:1 ~/rps/scissors {because}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/rps/paper {p}\n~/rps/scissors 3:1 ~/rps/paper {because}\n",
    ));
    state.apply_event(ingest_event(
        3,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/rps/paper 3:1 ~/rps/rock {because}\n",
    ));

    let mut group = state.ranking_group.clone();
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
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/t/champion {c}\n~/t/b {b}\n~/t/c {c}\n~/t/d {d}\n~/t/champion 10:1 ~/t/b {because}\n~/t/champion 10:1 ~/t/c {because}\n~/t/champion 10:1 ~/t/d {because}\n~/t/b 2:1 ~/t/c {because}\n~/t/c 2:1 ~/t/d {because}\n",
    ));

    let mut group = state.ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked[0].item, "t/champion");
    assert!(ranked[0].score > ranked[1].score);
}

#[test]
fn ranking_neutral_votes_produce_equal_scores() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:x\n~/t/a {a}\n~/t/b {b}\n~/t/c {c}\n~/t/a 1:1 ~/t/b {neutral}\n~/t/b 1:1 ~/t/c {neutral}\n~/t/c 1:1 ~/t/a {neutral}\n",
    ));

    let mut group = state.ranking_group.clone();
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
        let raw = vote_doc("t", "x", &a, &b, 3, 1);
        state.apply_event(ingest_event(i as i64 + 1, &raw));
    }

    let group = state.ranking_group.clone();

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
        ingest_event(1, "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n:x\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n"),
        ingest_event(2, "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n:x\n/b 3:1 /c {because}\n"),
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
    log.append(&ingest_event(1, "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n:x\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n"))
        .await
        .unwrap();

    use std::fs;
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(f, "not json at all").unwrap();

    log.append(&ingest_event(2, "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n:x\n/b 3:1 /c {because}\n"))
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

    log.append(&ingest_event(1, "@00000000-0000-0000-0000-000000000000:test:local/test\n#t\n:x\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n"))
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
        "@00000000-0000-0000-0000-000000000000:test:local/test\n:speed\n~/langs/rust {Systems language}\n~/langs/go {Simple concurrency}\n~/langs/rust 3:1 ~/langs/go {because}\n",
    ));

    let mut group = state.ranking_group.clone();
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item, "langs/rust"); // Should win
    assert!(ranked[0].score > ranked[1].score);
}

#[test]
fn canonicalization_is_consistent() {
    assert_eq!(canonicalize_tag("#tag"), "tag");
    assert_eq!(canonicalize_tag("tag"), "tag");
    assert_eq!(canonicalize_tag("TAG"), "tag");

    assert_eq!(canonicalize_aspect(":aspect"), "aspect");
    assert_eq!(canonicalize_aspect("aspect"), "aspect");
    assert_eq!(canonicalize_aspect("ASPECT"), "aspect");

    assert_eq!(canonicalize_item("/item"), "item");
    assert_eq!(canonicalize_item("item"), "item");
    assert_eq!(canonicalize_item("ITEM"), "item");
}

