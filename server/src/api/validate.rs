use axum::http::StatusCode;
use std::collections::HashSet;

use crate::{
    canonical_path::canonicalize_tag,
    dsl,
    path_types::ItemId,
    reducer::{ReducerState, ScopeId},
};
use slug_types::paths::GardenItemUrl;

use super::helpers::resolve_item;

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
    let room_wire = match scope {
        ScopeId::Public => "public",
        ScopeId::Room(r) => r.as_str(),
    };
    let public_content = reduced.public();
    let scoped_content = match scope {
        ScopeId::Public => None,
        _ => reduced.content_for_scope(scope),
    };
    validate_ingest_with_content_lookup(
        text,
        room_wire,
        |key| {
            scoped_content.map(|c| c.items.contains(key)).unwrap_or(false)
                || public_content.items.contains(key)
        },
        |key| {
            scoped_content.map(|c| c.item_bodies.contains_key(key)).unwrap_or(false)
                || public_content.item_bodies.contains_key(key)
        },
    )
}

/// Like [`validate_ingest_document`] for public ingest, but vote items may also
/// exist in a source private room's garden (cross-thread references within the room).
pub fn validate_ingest_document_for_graduation(
    reduced: &ReducerState,
    text: &str,
    source_room_id: &str,
) -> Result<ValidatedIngest, (StatusCode, String, Option<String>)> {
    let source_scope = ScopeId::Room(source_room_id.trim().to_string());
    let public_content = reduced.public();
    let source_content = reduced.content_for_scope(&source_scope);
    validate_ingest_with_content_lookup(
        text,
        "public",
        |key| {
            public_content.items.contains(key)
                || source_content.map(|c| c.items.contains(key)).unwrap_or(false)
        },
        |key| {
            public_content.item_bodies.contains_key(key)
                || source_content
                    .map(|c| c.item_bodies.contains_key(key))
                    .unwrap_or(false)
        },
    )
}

fn validate_ingest_with_content_lookup(
    text: &str,
    room_wire: &str,
    item_exists: impl Fn(&ItemId) -> bool,
    body_exists: impl Fn(&ItemId) -> bool,
) -> Result<ValidatedIngest, (StatusCode, String, Option<String>)> {
    let doc = match dsl::parse_full(text) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "parse error".to_string(),
                Some(format!("{e}")),
            ));
        }
    };

    let ts = super::helpers::now_ms();
    let mut defined_in_doc: HashSet<ItemId> = HashSet::new();

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
                        format!("item missing body: {}", GardenItemUrl::from_stored(&item, room_wire)),
                        Some("items must be declared with bodies, e.g. `~/path/item { ... }`".to_string()),
                    ));
                };
                if body_text.trim().is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: {}", GardenItemUrl::from_stored(&item, room_wire)),
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
                    .filter(|it| !defined_in_doc.contains(*it) && !item_exists(it))
                    .map(|it| GardenItemUrl::from_stored(it, room_wire).into_inner())
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
                    .filter(|it| !defined_in_doc.contains(*it) && !body_exists(it))
                    .map(|it| GardenItemUrl::from_stored(it, room_wire).into_inner())
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

/// Items introduced by `Stmt::Item` lines in a parsed ingest body.
pub fn items_defined_in_ingest(text: &str) -> HashSet<ItemId> {
    let Ok(doc) = dsl::parse_full(text) else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    for s in &doc.statements {
        if let dsl::Stmt::Item { title, body: Some(_) } = s {
            if let Ok(item) = resolve_item(title) {
                out.insert(item);
            }
        }
    }
    out
}

/// Items referenced by vote lines in a parsed ingest body.
pub fn items_voted_in_ingest(text: &str) -> HashSet<ItemId> {
    let Ok(doc) = dsl::parse_full(text) else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    for s in &doc.statements {
        if let dsl::Stmt::Vote { item1, item2, .. } = s {
            if let Ok(a) = resolve_item(item1) {
                out.insert(a);
            }
            if let Ok(b) = resolve_item(item2) {
                out.insert(b);
            }
        }
    }
    out
}

/// Synthetic public ingests that copy ontology items from a private room garden
/// when a graduating thread votes on items defined in other private threads.
pub fn seed_ingest_texts_for_graduation(
    reduced: &ReducerState,
    source_room_id: &str,
    source_ingest_ids: &[String],
) -> Vec<String> {
    let source_scope = ScopeId::Room(source_room_id.trim().to_string());
    let Some(source_content) = reduced.content_for_scope(&source_scope) else {
        return Vec::new();
    };
    let public = reduced.public();

    let mut referenced = HashSet::new();
    let mut defined_in_thread = HashSet::new();
    for id in source_ingest_ids {
        let Some(ing) = reduced.ingests_by_id.get(id) else {
            continue;
        };
        defined_in_thread.extend(items_defined_in_ingest(&ing.raw));
        referenced.extend(items_voted_in_ingest(&ing.raw));
    }

    let mut seeds: Vec<ItemId> = referenced
        .into_iter()
        .filter(|item| {
            !public.items.contains(item)
                && !defined_in_thread.contains(item)
                && source_content.items.contains(item)
                && source_content.item_bodies.contains_key(item)
        })
        .collect();
    seeds.sort();

    seeds
        .into_iter()
        .filter_map(|item| {
            let body = source_content.item_bodies.get(&item)?;
            Some(format!("{} {{{}}}", item.display_path(), body))
        })
        .collect()
}

pub fn normalize_room_and_thread(room: &str, thread_tag: &str) -> (String, String) {
    (
        room.trim().to_string(),
        canonicalize_tag(thread_tag),
    )
}
