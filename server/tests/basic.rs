use slugsocial_server::{
    event_log::EventLog,
    events::{canonicalize_aspect, canonicalize_item, canonicalize_tag, Event, Ingest},
    ranking::ranked_items,
    reducer::{GroupKey, GroupState, ReducerState},
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
        "@test\n#{tag}\n:{aspect}\n/{a} {{body a}}\n/{b} {{body b}}\n/{a} {left}:{right} /{b} {{because test}}\n"
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
    state.apply_event(ingest_event(1, "@test\n#t\n:x\n/a {a}\n/b {b}\n/a 3:1 /b {because}\n"));
    // Second ingest: define c + vote b > c.
    state.apply_event(ingest_event(2, "@test\n#t\n:x\n/c {c}\n/b 3:1 /c {because}\n"));

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");

    let ranked = ranked_items(&mut group, 20000, 1e-9);
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].item, "a");
    assert_eq!(ranked[1].item, "b");
    assert_eq!(ranked[2].item, "c");
}

#[test]
fn reducer_canonicalizes_identifiers() {
    let mut state = ReducerState::default();

    // Mix of formats across ingests (case + sigils).
    state.apply_event(ingest_event(
        1,
        "@test\n#Tag\n:Aspect\n/Item-A {x}\n/Item-B {y}\n/Item-A 2:1 /Item-B {because}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@test\nTAG\nASPECT\nITEM-A 2:1 ITEM-B {because}\n",
    ));
    state.apply_event(ingest_event(
        3,
        "@test\n#tag\n:aspect\n/item-a 2:1 /item-b {because}\n",
    ));

    let key = GroupKey {
        tag: canonicalize_tag("tag"),
        aspect: canonicalize_aspect("aspect"),
    };
    assert!(state.groups.contains_key(&key));
    let group = &state.groups[&key];
    assert_eq!(group.idx_to_item.len(), 2); // Should dedupe to 2 items
}

#[test]
fn reducer_handles_item_and_body_from_ingest() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@test\n#t\n/test-item {Description here}\n",
    ));

    assert!(state.items.contains("test-item"));
    assert_eq!(
        state.item_bodies.get("test-item"),
        Some(&"Description here".to_string())
    );
    assert!(state.tags.get("t").unwrap().contains("test-item"));
}

#[test]
fn reducer_aggregates_multiple_votes() {
    let mut state = ReducerState::default();

    // Multiple votes between same pair should accumulate weights.
    for ts in 1..=3 {
        state.apply_event(ingest_event(ts, &vote_doc("t", "x", "a", "b", 2, 1)));
    }

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let group = &state.groups[&key];
    let a_idx = group.item_to_idx["a"];
    let b_idx = group.item_to_idx["b"];

    // Should have accumulated edge weights in both directions.
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
}

#[test]
fn reducer_clamps_score_bounds() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@test\n#t\n:x\n/a {a}\n/b {b}\n/a 1000:1 /b {huge}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@test\n#t\n:x\n/a 1:1000 /b {huge}\n",
    ));

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let group = &state.groups[&key];
    // Should still work, scores clamped internally.
    assert_eq!(group.idx_to_item.len(), 2);
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
        "@test\n#rps\n:x\n/rock {r}\n/scissors {s}\n/rock 3:1 /scissors {because}\n",
    ));
    state.apply_event(ingest_event(
        2,
        "@test\n#rps\n:x\n/scissors 3:1 /paper {because}\n",
    ));
    state.apply_event(ingest_event(
        3,
        "@test\n#rps\n:x\n/paper 3:1 /rock {because}\n",
    ));

    let key = GroupKey {
        tag: "rps".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");

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
    let mut group = GroupState::new("t".to_string(), "x".to_string());
    let ranked = ranked_items(&mut group, 1000, 1e-9);
    assert_eq!(ranked.len(), 0);
}

#[test]
fn ranking_dominant_item_wins() {
    // Item A beats everyone strongly, others have mixed results.
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@test\n#t\n:x\n/champion {c}\n/b {b}\n/c {c}\n/d {d}\n/champion 10:1 /b {because}\n/champion 10:1 /c {because}\n/champion 10:1 /d {because}\n/b 2:1 /c {because}\n/c 2:1 /d {because}\n",
    ));

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked[0].item, "champion");
    assert!(ranked[0].score > ranked[1].score);
}

#[test]
fn ranking_neutral_votes_produce_equal_scores() {
    let mut state = ReducerState::default();
    state.apply_event(ingest_event(
        1,
        "@test\n#t\n:x\n/a {a}\n/b {b}\n/c {c}\n/a 1:1 /b {neutral}\n/b 1:1 /c {neutral}\n/c 1:1 /a {neutral}\n",
    ));

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");
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

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let group = state.groups.get(&key).expect("group exists").clone();

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
        ingest_event(1, "@test\n#t\n:x\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n"),
        ingest_event(2, "@test\n#t\n:x\n/b 3:1 /c {because}\n"),
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
    log.append(&ingest_event(1, "@test\n#t\n:x\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n"))
        .await
        .unwrap();

    use std::fs;
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(f, "not json at all").unwrap();

    log.append(&ingest_event(2, "@test\n#t\n:x\n/b 3:1 /c {because}\n"))
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

    log.append(&ingest_event(1, "@test\n#t\n:x\n/a {x}\n/b {y}\n/a 2:1 /b {because}\n"))
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
        "@test\n#langs\n:speed\n/rust {Systems language}\n/go {Simple concurrency}\n/rust 3:1 /go {because}\n",
    ));

    let key = GroupKey {
        tag: "langs".to_string(),
        aspect: "speed".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item, "rust"); // Should win
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

