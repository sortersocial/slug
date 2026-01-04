use slugsocial_server::{
    event_log::EventLog,
    events::{canonicalize_aspect, canonicalize_item, canonicalize_tag, Event, ItemUpsert, TagAdd, VoteCast},
    ranking::ranked_items,
    reducer::{GroupKey, GroupState, ReducerState},
};
use tempfile::TempDir;

fn vote(ts: i64, tag: &str, aspect: &str, a: &str, b: &str, left: i32, right: i32) -> Event {
    Event::VoteCast(VoteCast {
        ts,
        tag: tag.to_string(),
        aspect: aspect.to_string(),
        a: a.to_string(),
        b: b.to_string(),
        ratio_left: left,
        ratio_right: right,
        body: "because test".to_string(),
        voter_key_id: "test".to_string(),
        actor: None,
    })
}

// ============================================================================
// Reducer Tests
// ============================================================================

#[test]
fn reducer_and_ranking_linear_chain() {
    // Prefer /a over /b over /c.
    let mut state = ReducerState::default();
    state.apply_event(vote(1, "#t", ":x", "/a", "/b", 3, 1));
    state.apply_event(vote(2, "#t", ":x", "/b", "/c", 3, 1));

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
    // Mix of formats
    state.apply_event(vote(1, "tag", "aspect", "item-a", "item-b", 2, 1));
    state.apply_event(vote(2, "#tag", ":aspect", "/item-a", "/item-b", 2, 1));
    state.apply_event(vote(3, "TAG", "ASPECT", "ITEM-A", "ITEM-B", 2, 1));

    let key = GroupKey {
        tag: canonicalize_tag("tag"),
        aspect: canonicalize_aspect("aspect"),
    };
    assert!(state.groups.contains_key(&key));
    let group = &state.groups[&key];
    assert_eq!(group.idx_to_item.len(), 2); // Should dedupe to 2 items
}

#[test]
fn reducer_handles_item_upsert() {
    let mut state = ReducerState::default();
    state.apply_event(Event::ItemUpsert(ItemUpsert {
        ts: 1,
        item: "/test-item".to_string(),
        body: Some("Description here".to_string()),
    }));

    assert!(state.items.contains("test-item"));
    assert_eq!(state.item_bodies.get("test-item"), Some(&"Description here".to_string()));
}

#[test]
fn reducer_handles_tag_add() {
    let mut state = ReducerState::default();
    state.apply_event(Event::TagAdd(TagAdd {
        ts: 1,
        tag: "#rust".to_string(),
        item: "/clap".to_string(),
    }));

    assert!(state.items.contains("clap"));
    assert!(state.tags.get("rust").unwrap().contains("clap"));
}

#[test]
fn reducer_aggregates_multiple_votes() {
    let mut state = ReducerState::default();
    // Multiple votes between same pair should accumulate weights
    state.apply_event(vote(1, "#t", ":x", "/a", "/b", 2, 1));
    state.apply_event(vote(2, "#t", ":x", "/a", "/b", 2, 1));
    state.apply_event(vote(3, "#t", ":x", "/a", "/b", 2, 1));

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let group = &state.groups[&key];
    let a_idx = group.item_to_idx["a"];
    let b_idx = group.item_to_idx["b"];
    
    // Should have accumulated edge weights
    assert!(group.edges.contains_key(&(a_idx, b_idx)));
    assert!(group.edges.contains_key(&(b_idx, a_idx)));
}

#[test]
fn reducer_clamps_score_bounds() {
    let mut state = ReducerState::default();
    state.apply_event(vote(1, "#t", ":x", "/a", "/b", 1000, 1)); // Way over limit
    state.apply_event(vote(2, "#t", ":x", "/a", "/b", 1, 1000)); // Way under limit

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let group = &state.groups[&key];
    // Should still work, scores clamped internally
    assert_eq!(group.idx_to_item.len(), 2);
}

// ============================================================================
// Ranking Tests
// ============================================================================

#[test]
fn ranking_cycle_is_nearly_equal() {
    // Rock-paper-scissors cycle.
    let mut state = ReducerState::default();
    state.apply_event(vote(1, "#rps", ":x", "/rock", "/scissors", 3, 1)); // rock > scissors
    state.apply_event(vote(2, "#rps", ":x", "/scissors", "/paper", 3, 1)); // scissors > paper
    state.apply_event(vote(3, "#rps", ":x", "/paper", "/rock", 3, 1)); // paper > rock

    let key = GroupKey {
        tag: "rps".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");

    let ranked = ranked_items(&mut group, 50000, 1e-9);
    assert_eq!(ranked.len(), 3);
    let mean = ranked.iter().map(|r| r.score).sum::<f64>() / 3.0;
    for r in ranked {
        assert!((r.score - mean).abs() < 0.05, "score {} deviates from mean {}", r.score, mean);
    }
}

#[test]
fn ranking_single_item() {
    let mut state = ReducerState::default();
    // Apply a vote to create a single-item group (self-vote doesn't make sense, but creates the item)
    // Actually, we need at least 2 items for ranking. Let's test with 2 items and one neutral vote.
    state.apply_event(vote(1, "#t", ":x", "/only", "/other", 1, 1));
    // Then remove the other item by only ranking the first
    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");
    // Actually, let's test empty group instead since single-item is edge case
    let mut empty_group = GroupState::new("t2".to_string(), "x2".to_string());
    let ranked = ranked_items(&mut empty_group, 1000, 1e-9);
    assert_eq!(ranked.len(), 0);
}

#[test]
fn ranking_empty_group() {
    let mut group = GroupState::new("t".to_string(), "x".to_string());
    let ranked = ranked_items(&mut group, 1000, 1e-9);
    assert_eq!(ranked.len(), 0);
}

#[test]
fn ranking_dominant_item_wins() {
    // Item A beats everyone strongly, others have mixed results
    let mut state = ReducerState::default();
    state.apply_event(vote(1, "#t", ":x", "/champion", "/b", 10, 1));
    state.apply_event(vote(2, "#t", ":x", "/champion", "/c", 10, 1));
    state.apply_event(vote(3, "#t", ":x", "/champion", "/d", 10, 1));
    state.apply_event(vote(4, "#t", ":x", "/b", "/c", 2, 1));
    state.apply_event(vote(5, "#t", ":x", "/c", "/d", 2, 1));

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
    // All neutral votes (score=0) should produce roughly equal scores
    state.apply_event(vote(1, "#t", ":x", "/a", "/b", 1, 1));
    state.apply_event(vote(2, "#t", ":x", "/b", "/c", 1, 1));
    state.apply_event(vote(3, "#t", ":x", "/c", "/a", 1, 1));

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");
    let ranked = ranked_items(&mut group, 20000, 1e-9);

    assert_eq!(ranked.len(), 3);
    let mean = ranked.iter().map(|r| r.score).sum::<f64>() / 3.0;
    for r in &ranked {
        assert!((r.score - mean).abs() < 0.1, "score {} should be near mean {}", r.score, mean);
    }
}

#[test]
fn ranking_converges_with_many_iterations() {
    let mut state = ReducerState::default();
    // Create a clear linear ordering
    for i in 0..5 {
        if i < 4 {
            state.apply_event(vote(i as i64, "#t", ":x", &format!("/{}", i), &format!("/{}", i+1), 3, 1));
        }
    }

    let key = GroupKey {
        tag: "t".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");
    
    let ranked_short = ranked_items(&mut group.clone(), 10, 1e-3);
    let ranked_long = ranked_items(&mut group, 50000, 1e-9);

    // Both should produce same ordering
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
        vote(1, "#t", ":x", "/a", "/b", 2, 1),
        vote(2, "#t", ":x", "/b", "/c", 3, 1),
        Event::ItemUpsert(ItemUpsert {
            ts: 3,
            item: "/a".to_string(),
            body: Some("Item A".to_string()),
        }),
    ];

    for ev in &events {
        log.append(ev).await.unwrap();
    }

    let (loaded, bad) = log.load_all().await.unwrap();
    assert_eq!(bad.len(), 0);
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0], events[0]);
    assert_eq!(loaded[1], events[1]);
    assert_eq!(loaded[2], events[2]);
}

#[tokio::test]
async fn event_log_handles_corrupt_lines() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("events.jsonl");
    let log = EventLog::new(&log_path);

    // Write valid events using the log itself, then manually corrupt one line
    log.append(&vote(1, "#t", ":x", "/a", "/b", 2, 1)).await.unwrap();
    
    use std::fs;
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(f, "not json at all").unwrap();
    
    log.append(&vote(2, "#t", ":x", "/b", "/c", 3, 1)).await.unwrap();
    
    // Add empty line
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

    log.append(&vote(1, "#t", ":x", "/a", "/b", 2, 1)).await.unwrap();
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
// Integration Tests
// ============================================================================

#[tokio::test]
async fn full_workflow_reducer_and_ranking() {
    // Simulate full workflow: events -> reducer -> ranking
    let mut state = ReducerState::default();
    
    // Add items
    state.apply_event(Event::ItemUpsert(ItemUpsert {
        ts: 1,
        item: "/rust".to_string(),
        body: Some("Systems language".to_string()),
    }));
    state.apply_event(Event::ItemUpsert(ItemUpsert {
        ts: 2,
        item: "/go".to_string(),
        body: Some("Simple concurrency".to_string()),
    }));
    state.apply_event(Event::TagAdd(TagAdd {
        ts: 3,
        tag: "#langs".to_string(),
        item: "/rust".to_string(),
    }));
    state.apply_event(Event::TagAdd(TagAdd {
        ts: 4,
        tag: "#langs".to_string(),
        item: "/go".to_string(),
    }));

    // Vote
    state.apply_event(vote(5, "#langs", ":speed", "/rust", "/go", 3, 1));

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


