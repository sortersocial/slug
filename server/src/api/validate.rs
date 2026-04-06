use axum::http::StatusCode;
use std::collections::HashSet;

use crate::{
    canonical_path::canonicalize_tag,
    dsl,
    path_types::CanonicalItemUrl,
    reducer::{ReducerState, ScopeId},
};

use super::helpers::{item_path_for_api, resolve_item};

#[derive(Debug)]
pub struct ValidatedIngest {
    pub doc: dsl::Document,
    pub ts: i64,
    pub raw_text: String,
}

pub fn validate_ingest_document(
    reduced: &ReducerState,
    text: &str,
    scope: &ScopeId,
) -> Result<ValidatedIngest, (StatusCode, String, Option<String>)> {
    let public_content = reduced.public();
    let scoped_content = match scope {
        ScopeId::Public => None,
        _ => reduced.content_for_scope(scope),
    };
    let item_exists = |key: &CanonicalItemUrl| {
        scoped_content.map(|c| c.items.contains(key)).unwrap_or(false)
            || public_content.items.contains(key)
    };
    let body_exists = |key: &CanonicalItemUrl| {
        scoped_content.map(|c| c.item_bodies.contains_key(key)).unwrap_or(false)
            || public_content.item_bodies.contains_key(key)
    };
    let doc = match dsl::parse_full(text) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "parse error".to_string(),
                Some(format!("{}", e)),
            ));
        }
    };

    let ts = super::helpers::now_ms();
    let mut defined_in_doc: HashSet<String> = HashSet::new();

    for s in &doc.statements {
        match s {
            dsl::Stmt::Item { title, body } => {
                let item = match resolve_item(title) {
                    Ok(v) => v,
                    Err(msg) => {
                        return Err((StatusCode::BAD_REQUEST, "invalid item path".to_string(), Some(msg)));
                    }
                };
                let Some(body_text) = body else {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("item missing body: {}", item_path_for_api(&item)),
                        Some("items must be declared with bodies, e.g. `~/path/item { ... }`".to_string()),
                    ));
                };
                if body_text.trim().is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: {}", item_path_for_api(&item)),
                        Some("write at least one sentence inside `{ ... }`".to_string()),
                    ));
                }
                defined_in_doc.insert(item);
            }
            dsl::Stmt::Vote { item1, item2, .. } => {
                let a = match resolve_item(item1) {
                    Ok(v) => v,
                    Err(msg) => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path".to_string(),
                            Some(msg),
                        ));
                    }
                };
                let b = match resolve_item(item2) {
                    Ok(v) => v,
                    Err(msg) => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "invalid vote item path".to_string(),
                            Some(msg),
                        ));
                    }
                };
                let missing: Vec<String> = [&a, &b]
                    .into_iter()
                    .filter(|it| {
                        let key = CanonicalItemUrl((*it).clone());
                        !defined_in_doc.contains(*it) && !item_exists(&key)
                    })
                    .map(|it| item_path_for_api(it))
                    .collect();
                if !missing.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "vote references undefined item(s)".to_string(),
                        Some(format!(
                            "define items with bodies before voting. missing: {}",
                            missing.join(", ")
                        )),
                    ));
                }
                let missing_body: Vec<String> = [&a, &b]
                    .into_iter()
                    .filter(|it| {
                        let key = CanonicalItemUrl((*it).clone());
                        !defined_in_doc.contains(*it) && !body_exists(&key)
                    })
                    .map(|it| item_path_for_api(it))
                    .collect();
                if !missing_body.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "vote references item(s) without bodies".to_string(),
                        Some(format!(
                            "missing bodies: {}. Declare each item as `/item {{ ... }}`",
                            missing_body.join(", ")
                        )),
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(ValidatedIngest {
        doc,
        ts,
        raw_text: text.to_string(),
    })
}

pub fn normalize_room_and_thread(room: &str, thread_tag: &str) -> (String, String) {
    (
        room.trim().to_string(),
        canonicalize_tag(thread_tag),
    )
}
