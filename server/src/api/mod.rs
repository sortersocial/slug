mod debug;
mod feed;
mod forum;
mod garden;
mod helpers;
mod ingest;
mod rank;
mod search;
// mod stream; // disabled while HTML/SSE views are offline

// Re-export all public items so `api::get_rank`, `api::post_ingest`, etc. still work.

pub use debug::{get_debug_query, DebugQuery};

pub use feed::{get_feed, FeedQuery};

pub use forum::{get_thread, get_threads, ThreadDetailQuery};

pub use garden::{
    get_item, get_leaves, get_matchup, get_paths, get_recent_votes,
    ItemQuery, LeavesQuery, MatchupQuery, PathsQuery, RecentVotesQuery,
};

pub use helpers::{
    api_error, compute_connectivity_stats, is_pair_voted, now_ms, paginate_rankings,
    parse_parent_specs, pick_random_distinct, sha256_hex, resolve_item, vote_touches_path,
};

pub use ingest::{post_check, post_ingest, validate_ingest_document, IngestQuery, ValidatedIngest};

pub use rank::{
    get_global_rank, get_pair, get_rank, get_rank_history,
    GlobalRankQuery, PairQuery, RankHistoryQuery, RankQuery,
};

pub use search::{get_search, SearchApiQuery};

// pub use stream::{get_html_stream, get_stream};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, Ingest};
    use crate::reducer::ReducerState;
    use axum::http::StatusCode;

    fn apply_ingest(reduced: &mut ReducerState, ts: i64, raw: &str) {
        reduced.apply_event(Event::Ingest(Ingest {
            ts,
            id: format!("test-{ts}"),
            raw: raw.to_string(),
            principal: "test".to_string(),
            delegate: "@00000000-0000-0000-0000-000000000000:test:local/test".to_string(),
            thread_id: "t".to_string(),
        }));
    }

    #[test]
    fn validate_ingest_document_requires_actor() {
        let reduced = ReducerState::default();
        let text = "~/t/a {a}\n~/t/b {b}\n";
        let v = validate_ingest_document(&reduced, text).unwrap();
        assert_eq!(v.raw_text.trim(), text.trim());
    }

    #[test]
    fn validate_ingest_document_parse_error() {
        let reduced = ReducerState::default();
        let text = "~/t/a { unclosed ";
        let err = validate_ingest_document(&reduced, text).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "parse error");
    }

    #[test]
    fn validate_ingest_document_accepts_valid_doc_with_existing_items() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "~/t/a {a}\n~/t/b {b}\n~/t/a 2:1 ~/t/b {because}\n",
        );
        let text = "~/t/a 1:1 ~/t/b {equal}\n";
        validate_ingest_document(&reduced, text).unwrap();
    }

    #[test]
    fn validate_ingest_document_rejects_vote_on_undefined_item() {
        let reduced = ReducerState::default();
        let text = "~/t/a {a}\n~/t/b 1:1 ~/t/missing {why}\n";
        let err = validate_ingest_document(&reduced, text).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("undefined item"));
    }

    #[test]
    fn validate_ingest_document_requires_tag() {
        let reduced = ReducerState::default();
        // tags are no longer DSL routing metadata; validation no longer requires them.
        let text = "~/t/a {a}\n~/t/b {b}\n";
        validate_ingest_document(&reduced, text).unwrap();
    }

    #[test]
    fn validate_ingest_document_accepts_quoted_thread_title() {
        let reduced = ReducerState::default();
        // quoted thread title is now prose; should still parse.
        let text = "\"This is a title\" { This is the body of the post }\n~/t/a {a}\n";
        validate_ingest_document(&reduced, text).unwrap();
    }

    #[test]
    fn validate_ingest_document_rejects_multiple_threads() {
        let reduced = ReducerState::default();
        // multiple thread declarations are now prose; should still parse.
        let text = "#one\n#two\n~/t/a {a}\n";
        validate_ingest_document(&reduced, text).unwrap();
    }

    #[test]
    fn validate_ingest_document_rejects_item_without_body() {
        let reduced = ReducerState::default();
        let text = "~/t/a\n";
        let err = validate_ingest_document(&reduced, text).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("missing body"));
    }
}
