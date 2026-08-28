mod auth;
mod helpers;
mod rpc;
mod stream;
mod ui_html;
mod validate;
pub(crate) mod write_actor;

pub use auth::{
    get_auth_callback, get_auth_complete, get_auth_login, get_choose_username, get_join_invite,
    get_logout, get_pending_session, get_web_login, get_whoami, optional_principal,
    post_choose_username, post_pending_session, resolve_web_session, session_cookie_header_value,
    verify_bearer_principal, WebSession, SLUG_SESSION_COOKIE,
};

pub use helpers::{
    api_error, compute_connectivity_stats, is_pair_voted, now_ms, paginate_rankings,
    parse_parent_specs, pick_random_distinct_item_pair, public_url, resolve_item, sha256_hex,
    vote_touches_path,
};

pub use rpc::{dispatch_rpc, handle_rpc_batch};

pub use stream::{get_html_stream, get_stream};

pub use validate::{normalize_room_and_thread, validate_ingest_document, ValidatedIngest};

pub use ui_html::post_ui_html;

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
            delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
            room_id: "public".to_string(),
            thread_tag: "t".to_string(),
        }));
    }

    #[test]
    fn validate_ingest_document_parse_error() {
        let reduced = ReducerState::default();
        let text = "~/t/a { unclosed ";
        let err =
            validate_ingest_document(&reduced, text, &crate::reducer::ScopeId::Public).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "parse error");
    }

    #[test]
    fn validate_ingest_document_accepts_valid_doc_with_existing_items() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "~/t/a {a}\n~/t/b {b}\n{because}\n~/t/a 2:1 ~/t/b\n",
        );
        let text = "{equal}\n~/t/a 1:1 ~/t/b\n";
        validate_ingest_document(&reduced, text, &crate::reducer::ScopeId::Public).unwrap();
    }

    #[test]
    fn validate_ingest_document_rejects_vote_on_undefined_item() {
        let reduced = ReducerState::default();
        let text = "~/t/a {x}\n{why}\n~/t/b 1:1 ~/t/missing\n";
        let err =
            validate_ingest_document(&reduced, text, &crate::reducer::ScopeId::Public).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("undefined item"));
    }

    #[test]
    fn validate_ingest_document_accepts_quoted_thread_title() {
        let reduced = ReducerState::default();
        let text = "\"This is a title\" { This is the body of the post }\n~/t/a {a}\n";
        validate_ingest_document(&reduced, text, &crate::reducer::ScopeId::Public).unwrap();
    }

    #[test]
    fn validate_ingest_document_rejects_multiple_threads() {
        let reduced = ReducerState::default();
        let text = "#one\n#two\n~/t/a {a}\n";
        validate_ingest_document(&reduced, text, &crate::reducer::ScopeId::Public).unwrap();
    }

    #[test]
    fn validate_ingest_document_rejects_item_without_body() {
        let reduced = ReducerState::default();
        let text = "~/t/a\n";
        let err =
            validate_ingest_document(&reduced, text, &crate::reducer::ScopeId::Public).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("missing body"));
    }

    #[test]
    fn recent_votes_parent_filter_uses_containment_not_path_prefix() {
        let mut reduced = ReducerState::default();
        apply_ingest(
            &mut reduced,
            1,
            "~/languages/rust {r}\n~/languages/python {p}\n{why}\n~/languages/rust 2:1 ~/languages/python\n",
        );
        let content = reduced.public();
        let rust = "https://slug.social/~/rust";
        let python = "https://slug.social/~/python";
        assert!(
            vote_touches_path(content, rust, python, "https://slug.social/~/languages"),
            "leaf members must count as touching their scope"
        );
        assert!(
            !vote_touches_path(content, rust, python, "https://slug.social/~/unrelated"),
            "a vote must not touch a scope neither side belongs to"
        );
    }
}
