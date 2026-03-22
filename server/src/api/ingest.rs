use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use slug_types::*;
use std::collections::{HashMap, HashSet};

use crate::{
    dsl,
    events::{canonicalize_actor, canonicalize_item, canonicalize_tag, item_parent_path, Event, Ingest},
    reducer::ReducerState,
    state::AppState,
};

use super::auth::validate_actor_format;
use super::helpers::{api_error, now_ms, resolve_item, sha256_hex};

/// Result of validating an ingest document. Shared by post_ingest and post_check.
#[derive(Debug)]
pub struct ValidatedIngest {
    pub doc: dsl::Document,
    pub ts: i64,
    pub voter_key_id: String,
    pub actor: String,
    pub threads: Vec<String>,
    pub raw_text: String,
}

/// Parse and validate an ingest document against current reduced state.
/// Returns a validated struct for commit (ingest) or dry-run (check).
/// Error is (StatusCode, error message, optional hint).
pub fn validate_ingest_document(
    reduced: &ReducerState,
    text: &str,
    require_actor_error: &str,
) -> Result<ValidatedIngest, (StatusCode, String, Option<String>)> {
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

    let ts = now_ms();
    let mut current_actor: Option<String> = None;
    let mut voter_key_id = "anon".to_string();
    let mut threads_seen: Vec<String> = Vec::new();
    let mut defined_in_doc: HashSet<String> = HashSet::new();

    for s in &doc.statements {
        match s {
            dsl::Stmt::Actor { name } => {
                if let Err(msg) = validate_actor_format(name) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "invalid actor format".to_string(),
                        Some(msg),
                    ));
                }
                let a = canonicalize_actor(name);
                current_actor = Some(a.clone());
                voter_key_id = a;
            }
            dsl::Stmt::Hashtag { name } => {
                let t = canonicalize_tag(name);
                if !threads_seen.contains(&t) {
                    threads_seen.push(t);
                }
            }
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
                        format!("item missing body: /{item}"),
                        Some("items must be declared with bodies, e.g. `~/path/item { ... }`".to_string()),
                    ));
                };
                if body_text.trim().is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("item body is empty: /{item}"),
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
                    .filter(|it| !defined_in_doc.contains(*it) && !reduced.items.contains(*it))
                    .map(|it| format!("/{it}"))
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
                    .filter(|it| !defined_in_doc.contains(*it) && !reduced.item_bodies.contains_key(*it))
                    .map(|it| format!("/{it}"))
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

    let actor = current_actor.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            require_actor_error.to_string(),
            Some("add `@yourname` at the start of your document".to_string()),
        )
    })?;

    let actor_u = crate::events::actor_uuid(&actor).to_string();

    // Private namespace ownership checks.
    for s in &doc.statements {
        match s {
            dsl::Stmt::Item { title, .. } => {
                let item = canonicalize_item(title);
                if let Some(owner) = crate::events::path_owner_uuid(&item) {
                    if owner != actor_u {
                        return Err((
                            StatusCode::FORBIDDEN,
                            "private namespace: path owner does not match actor".to_string(),
                            Some(format!(
                                "/{} belongs to UUID {}, but your actor UUID is {}",
                                item, owner, actor_u
                            )),
                        ));
                    }
                }
            }
            dsl::Stmt::Vote { item1, item2, .. } => {
                let a = canonicalize_item(item1);
                let b = canonicalize_item(item2);
                let a_owner = crate::events::path_owner_uuid(&a);
                let b_owner = crate::events::path_owner_uuid(&b);
                match (a_owner, b_owner) {
                    (None, None) => {} // both public, fine
                    (Some(oa), Some(ob)) if oa == ob => {
                        // both private, same owner -- actor must match
                        if oa != actor_u {
                            return Err((
                                StatusCode::FORBIDDEN,
                                "private namespace: vote owner does not match actor".to_string(),
                                Some(format!("these items belong to UUID {}, not {}", oa, actor_u)),
                            ));
                        }
                    }
                    _ => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "cross-votes between private and public items are not allowed".to_string(),
                            Some("votes must either be between two public items or two private items owned by the same actor".to_string()),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let threads: Vec<String> = threads_seen;
    if threads.len() > 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            "ingest may declare only one thread".to_string(),
            Some(format!(
                "found multiple thread declarations: {}",
                threads
                    .iter()
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ));
    }
    if threads.is_empty() {
        // Thread is optional only when every item and vote is under the actor's private namespace.
        // Pure prose posts and public items still require a #tag.
        let all_private = {
            let mut has_content = false;
            let mut all_under_actor = true;
            for s in &doc.statements {
                match s {
                    dsl::Stmt::Item { title, .. } => {
                        has_content = true;
                        let item = canonicalize_item(title);
                        if crate::events::path_owner_uuid(&item) != Some(actor_u.as_str()) {
                            all_under_actor = false;
                        }
                    }
                    dsl::Stmt::Vote { item1, item2, .. } => {
                        has_content = true;
                        let a = canonicalize_item(item1);
                        let b = canonicalize_item(item2);
                        if crate::events::path_owner_uuid(&a) != Some(actor_u.as_str())
                            || crate::events::path_owner_uuid(&b) != Some(actor_u.as_str())
                        {
                            all_under_actor = false;
                        }
                    }
                    _ => {}
                }
            }
            has_content && all_under_actor
        };
        if !all_private {
            return Err((
                StatusCode::BAD_REQUEST,
                "ingest requires at least one #tag".to_string(),
                Some("declare a thread with #tag (e.g. #sorting-hat) or a quoted title line (e.g. \"Sorting Hat\" { ... })\n(thread declaration is optional only when all items are in your private ~/uuid/ namespace)".to_string()),
            ));
        }
    }

    Ok(ValidatedIngest {
        doc,
        ts,
        voter_key_id,
        actor,
        threads,
        raw_text: text.to_string(),
    })
}

/// Compute ranking changes between two snapshots of a parent scope's rankings.
/// Returns None if nothing changed.
fn compute_scope_rank_changes(
    parent: &str,
    before: &crate::scope_rank::ChildrenRankings,
    after: &crate::scope_rank::ChildrenRankings,
) -> Option<ScopeRankChanges> {
    fn build_positions(rankings: &crate::scope_rank::ChildrenRankings) -> HashMap<String, Option<RankPosition>> {
        let mut map = HashMap::new();
        for comp in &rankings.component_rankings {
            let total = comp.ranked.len();
            for (i, item) in comp.ranked.iter().enumerate() {
                map.insert(item.item.clone(), Some(RankPosition { rank: i + 1, of: total }));
            }
        }
        for item in &rankings.unranked_items {
            map.insert(item.clone(), None);
        }
        map
    }

    let before_pos = build_positions(before);
    let after_pos = build_positions(after);

    let all_items: std::collections::BTreeSet<String> = before_pos.keys().cloned()
        .chain(after_pos.keys().cloned())
        .collect();

    let mut changes: Vec<RankChange> = Vec::new();
    for item in all_items {
        let b = before_pos.get(&item).cloned().flatten();
        let a = after_pos.get(&item).cloned().flatten();
        let changed = match (&b, &a) {
            (Some(bp), Some(ap)) => bp.rank != ap.rank || bp.of != ap.of,
            (None, Some(_)) => true,
            (Some(_), None) => true,
            (None, None) => false,
        };
        if changed {
            changes.push(RankChange {
                item: format!("/{}", item),
                before: b,
                after: a,
            });
        }
    }

    if changes.is_empty() {
        return None;
    }

    changes.sort_by(|a, b| match (&a.after, &b.after) {
        (Some(ap), Some(bp)) => ap.rank.cmp(&bp.rank),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.item.cmp(&b.item),
    });

    Some(ScopeRankChanges {
        parent: if parent.is_empty() { "/".to_string() } else { format!("/{}", parent) },
        changes,
    })
}

pub async fn post_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let event_log = state.event_log.clone();
    let reduced = reduced_arc.read().await;
    let v = match validate_ingest_document(
        &reduced,
        &req.text,
        "ingest requires @actor declaration",
    ) {
        Ok(x) => x,
        Err((status, msg, hint)) => return api_error(status, msg, hint).into_response(),
    };
    drop(reduced);

    // Passkey: header takes priority over JSON body field.
    let passkey: Option<String> = headers
        .get("x-slug-passkey")
        .and_then(|hv| hv.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| req.passkey.clone());

    // Passkey auth gate.
    // generated_passkey is Some when this is a new actor's first ingest (server-generated key).
    let generated_passkey: Option<String> = {
        let reduced = reduced_arc.read().await;
        match reduced.actor_keys.get(&v.actor) {
            Some(stored_hash) => {
                // Actor IS registered -- passkey required.
                match &passkey {
                    None => {
                        return api_error(
                            StatusCode::UNAUTHORIZED,
                            "this actor requires a passkey",
                            Some("pass --passkey <slug_sk_...> or set SLUG_PASSKEY".to_string()),
                        );
                    }
                    Some(pk) => {
                        if sha256_hex(pk) != *stored_hash {
                            return api_error(StatusCode::UNAUTHORIZED, "invalid passkey", None);
                        }
                    }
                }
                None
            }
            None => {
                // Actor NOT registered -- server generates passkey on first ingest.
                if passkey.is_some() {
                    return api_error(
                        StatusCode::UNAUTHORIZED,
                        "no passkey registered for this account",
                        Some("do not supply a passkey for a new actor; the server will generate one".to_string()),
                    );
                }
                Some(format!("slug_sk_{}", uuid::Uuid::new_v4().simple()))
            }
        }
    };

    // If registering: append ActorKeyRegistration event before the Ingest event.
    let mut events_appended: usize = 0;
    if let Some(ref pk) = generated_passkey {
        let key_hash = sha256_hex(pk);
        let reg_event = crate::events::Event::ActorKeyRegistration {
            ts: v.ts,
            actor: v.actor.clone(),
            key_hash,
        };
        if let Err(err) = event_log.append(&reg_event).await {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
        }
        {
            let mut reduced = reduced_arc.write().await;
            reduced.apply_event(reg_event);
        }
        events_appended += 1;
    }

    // Collect parent scopes for all voted items so we can compute ranking deltas.
    let voted_parent_scopes: Vec<String> = {
        let mut parents: HashSet<String> = HashSet::new();
        for s in &v.doc.statements {
            if let dsl::Stmt::Vote { item1, item2, .. } = s {
                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                    parents.insert(item_parent_path(&a).unwrap_or_default());
                    parents.insert(item_parent_path(&b).unwrap_or_default());
                }
            }
        }
        let mut out: Vec<String> = parents.into_iter().collect();
        out.sort();
        out
    };

    // Snapshot rankings before the ingest event is applied.
    let pre_rankings: HashMap<String, crate::scope_rank::ChildrenRankings> =
        if !voted_parent_scopes.is_empty() {
            let reduced = reduced_arc.read().await;
            voted_parent_scopes
                .iter()
                .map(|p| (p.clone(), crate::scope_rank::build_children_rankings(&reduced, p)))
                .collect()
        } else {
            HashMap::new()
        };

    let ingest_event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        voter_key_id: v.voter_key_id.clone(),
        actor: v.actor.clone(),
    });

    if let Err(err) = event_log.append(&ingest_event).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
    }
    events_appended += 1;

    let actor_for_stream = v.actor.clone();
    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(ingest_event);
    }

    // Snapshot rankings after the event and compute per-scope deltas.
    let ranking_changes: Vec<ScopeRankChanges> = if !voted_parent_scopes.is_empty() {
        let reduced = reduced_arc.read().await;
        voted_parent_scopes
            .iter()
            .filter_map(|p| {
                let before = pre_rankings.get(p)?;
                let after = crate::scope_rank::build_children_rankings(&reduced, p);
                compute_scope_rank_changes(p, before, &after)
            })
            .collect()
    } else {
        vec![]
    };

    let _ = state.stream_tx.send(crate::state::StreamEvent {
        ts: v.ts,
        actor: actor_for_stream,
        tags: v.threads.iter().map(|t| format!("#{t}")).collect(),
        snippet: v.raw_text.chars().take(200).collect(),
    });
    let html = crate::html::thread_feed_html(&state).await;
    let _ = state.html_tx.send(crate::state::HtmlFragment {
        selector: "#thread-feed".to_string(),
        html,
    });

    let primary_thread = v.threads.first().cloned().unwrap_or_else(|| "untagged".to_string());
    Json(IngestResponse {
        ok: true,
        threads: v.threads.iter().map(|t| format!("#{t}")).collect(),
        events_appended,
        registered: generated_passkey.is_some(),
        passkey: generated_passkey,
        next: NextMoves {
            pair: "npx slugsocial pair".to_string(),
            rank: "npx slugsocial rank".to_string(),
            web: format!("https://slug.social/t/{}", primary_thread),
        },
        ranking_changes,
    }).into_response()
}

pub async fn post_check(
    State(state): State<AppState>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let v = match validate_ingest_document(
        &reduced,
        &req.text,
        "check requires @actor declaration",
    ) {
        Ok(x) => x,
        Err((status, msg, hint)) => return api_error(status, msg, hint).into_response(),
    };

    // check is a true dry-run: no auth required, no registration, nothing persisted.
    drop(reduced);

    let event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        voter_key_id: v.voter_key_id.clone(),
        actor: v.actor.clone(),
    });

    let mut simulated = { reduced_arc.read().await.clone() };
    simulated.apply_event(event);

    // Collect parent scopes touched by votes in this document.
    let voted_parents: Vec<String> = {
        let mut parents: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in &v.doc.statements {
            if let dsl::Stmt::Vote { item1, item2, .. } = s {
                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                    parents.insert(item_parent_path(&a).unwrap_or_default());
                    parents.insert(item_parent_path(&b).unwrap_or_default());
                }
            }
        }
        let mut out: Vec<String> = parents.into_iter().collect();
        out.sort();
        out
    };

    // Show the scoped rankings for affected parent paths; fall back to empty if no votes.
    let rankings: Vec<slug_types::CheckScopeRanking> = voted_parents
        .iter()
        .map(|parent| {
            let scoped = crate::scope_rank::build_children_rankings(&simulated, parent);
            let components: Vec<RankComponent> = scoped
                .component_rankings
                .into_iter()
                .map(|comp| RankComponent {
                    pairs: comp.pairs,
                    ranking: comp
                        .ranked
                        .into_iter()
                        .map(|r| RankRow {
                            item: format!("/{}", r.item),
                            score: r.score,
                            percent: None,
                        })
                        .collect(),
                })
                .collect();
            slug_types::CheckScopeRanking {
                parent: if parent.is_empty() {
                    "/".to_string()
                } else {
                    format!("/{}", parent)
                },
                components,
                unranked_items: scoped
                    .unranked_items
                    .into_iter()
                    .map(|it| format!("/{}", it))
                    .collect(),
            }
        })
        .collect();

    let primary_thread = v.threads.first().cloned().unwrap_or_else(|| "untagged".to_string());

    Json(CheckResponse {
        ok: true,
        threads: v.threads.iter().map(|t| format!("#{t}")).collect(),
        rankings,
        next: vec![
            "npx slugsocial ingest <file.sorter>".to_string(),
            "npx slugsocial threads".to_string(),
            format!("https://slug.social/t/{}", primary_thread),
        ],
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    #[serde(default)]
    pub text: Option<String>,
}
