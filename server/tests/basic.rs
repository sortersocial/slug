use slugsocial_server::{
    events::{Event, VoteCast},
    ranking::ranked_items,
    reducer::{GroupKey, ReducerState},
};

fn vote(ts: i64, tag: &str, aspect: &str, a: &str, b: &str, score: i32) -> Event {
    Event::VoteCast(VoteCast {
        ts,
        tag: tag.to_string(),
        aspect: aspect.to_string(),
        a: a.to_string(),
        b: b.to_string(),
        score,
        voter_key_id: "test".to_string(),
    })
}

#[test]
fn reducer_and_ranking_linear_chain() {
    // Prefer /a over /b over /c.
    let mut state = ReducerState::default();
    state.apply_event(vote(1, "#t", ":x", "/a", "/b", 40));
    state.apply_event(vote(2, "#t", ":x", "/b", "/c", 40));

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
fn ranking_cycle_is_nearly_equal() {
    // Rock-paper-scissors cycle.
    let mut state = ReducerState::default();
    state.apply_event(vote(1, "#rps", ":x", "/rock", "/scissors", 40)); // rock > scissors
    state.apply_event(vote(2, "#rps", ":x", "/scissors", "/paper", 40)); // scissors > paper
    state.apply_event(vote(3, "#rps", ":x", "/paper", "/rock", 40)); // paper > rock

    let key = GroupKey {
        tag: "rps".to_string(),
        aspect: "x".to_string(),
    };
    let mut group = state.groups.remove(&key).expect("group exists");

    let ranked = ranked_items(&mut group, 50000, 1e-9);
    assert_eq!(ranked.len(), 3);
    let mean = ranked.iter().map(|r| r.score).sum::<f64>() / 3.0;
    for r in ranked {
        assert!((r.score - mean).abs() < 0.05);
    }
}


