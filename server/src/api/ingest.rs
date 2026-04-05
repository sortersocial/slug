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
    canonical_path::canonicalize_tag,
    dsl,
    events::{AgentBound, Event, Ingest, ThreadCapability, ThreadVisibility},
    identity::parse_agent,
    path_types::CanonicalItemUrl,
    reducer::{ReducerState, ScopeId},
    state::AppState,
};
use super::auth::verify_bearer_principal;
use super::helpers::{api_error, item_path_for_api, now_ms, resolve_item};

/// Result of validating an ingest document. Shared by post_ingest and post_check.
#[derive(Debug)]
pub struct ValidatedIngest {
    pub doc: dsl::Document,
    pub ts: i64,
    pub raw_text: String,
}

/// Parse and validate an ingest document against current reduced state.
/// Returns a validated struct for commit (ingest) or dry-run (check).
/// Error is (StatusCode, error message, optional hint).
///
/// Item existence is checked in `scope` first, then falls back to public.
pub fn validate_ingest_document(
    reduced: &ReducerState,
    text: &str,
    scope: &ScopeId,
) -> Result<ValidatedIngest, (StatusCode, String, Option<String>)> {
    let public_content = reduced.public();
    // For a private scope, also check items defined there.
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

    let ts = now_ms();
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
                map.insert(item.item.as_str().to_string(), Some(RankPosition { rank: i + 1, of: total }));
            }
        }
        for item in &rankings.unranked_items {
            map.insert(item.as_str().to_string(), None);
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
                item: item_path_for_api(&item),
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
        parent: if parent.is_empty() {
            "/".to_string()
        } else {
            item_path_for_api(parent)
        },
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

    let delegate: Option<String> = match &req.delegate {
        None => None,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => match parse_agent(s) {
            Ok(d) => Some(d),
            Err(msg) => {
                drop(reduced);
                return api_error(StatusCode::BAD_REQUEST, "invalid delegate format", Some(msg))
                    .into_response();
            }
        },
    };
    let thread_id = canonicalize_tag(&req.thread);

    let principal = match verify_bearer_principal(&headers, &reduced) {
        Ok(u) => u,
        Err((st, msg)) => {
            drop(reduced);
            return api_error(st, msg, None).into_response();
        }
    };

    // Determine thread visibility and scope for item lookups and cap enforcement.
    let visibility = reduced.threads.get(&thread_id)
        .map(|t| t.visibility)
        .unwrap_or(ThreadVisibility::Public);
    let scope = match visibility {
        ThreadVisibility::Public => ScopeId::Public,
        ThreadVisibility::Private => ScopeId::Thread(thread_id.clone()),
    };

    let v = match validate_ingest_document(&reduced, &req.text, &scope) {
        Ok(x) => x,
        Err((status, msg, hint)) => {
            drop(reduced);
            return api_error(status, msg, hint).into_response();
        }
    };

    // Enforce capabilities for private threads.
    if matches!(visibility, ThreadVisibility::Private) {
        let mut required: HashSet<ThreadCapability> = HashSet::new();
        required.insert(ThreadCapability::View);
        for stmt in &v.doc.statements {
            match stmt {
                dsl::Stmt::Vote { .. }  => { required.insert(ThreadCapability::Vote); }
                dsl::Stmt::Item { .. }  => { required.insert(ThreadCapability::AddItem); }
                dsl::Stmt::Prose { .. } => { required.insert(ThreadCapability::Post); }
            }
        }
        let missing: Vec<_> = required.iter()
            .filter(|cap| !reduced.user_has_cap(&thread_id, &principal, **cap))
            .collect();
        if !missing.is_empty() {
            drop(reduced);
            return api_error(StatusCode::FORBIDDEN, "insufficient capabilities for this thread", None).into_response();
        }
    }

    if let Some(ref d) = delegate {
        match reduced.agent_bindings.get(d) {
            Some(u) if u != &principal => {
                drop(reduced);
                return api_error(
                    StatusCode::FORBIDDEN,
                    "delegate already bound to another user",
                    None,
                )
                .into_response();
            }
            _ => {}
        }
    }
    let need_agent_bind = delegate
        .as_ref()
        .map(|d| reduced.agent_bindings.get(d).is_none())
        .unwrap_or(false);
    drop(reduced);

    let mut events_appended: usize = 0;

    if need_agent_bind {
        if let Some(agent_id) = delegate.clone() {
            let ab = Event::AgentBound(AgentBound {
                ts: now_ms(),
                agent: agent_id,
                username: principal.clone(),
            });
            if let Err(err) = event_log.append(&ab).await {
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
            }
            events_appended += 1;
            {
                let mut reduced = reduced_arc.write().await;
                reduced.apply_event(ab);
            }
        }
    }

    // Collect parent scopes for all voted items so we can compute ranking deltas.
    let voted_parent_scopes: Vec<CanonicalItemUrl> = {
        let mut parents: HashSet<CanonicalItemUrl> = HashSet::new();
        for s in &v.doc.statements {
            if let dsl::Stmt::Vote { item1, item2, .. } = s {
                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                    if let Some(p) = CanonicalItemUrl::parse(&a).and_then(|c| c.parent()) { parents.insert(p); }
                    if let Some(p) = CanonicalItemUrl::parse(&b).and_then(|c| c.parent()) { parents.insert(p); }
                }
            }
        }
        let mut out: Vec<CanonicalItemUrl> = parents.into_iter().collect();
        out.sort();
        out
    };

    // Snapshot rankings before the ingest event is applied.
    let pre_rankings: HashMap<CanonicalItemUrl, crate::scope_rank::ChildrenRankings> =
        if !voted_parent_scopes.is_empty() {
            let reduced = reduced_arc.read().await;
            let content = reduced.public();
            voted_parent_scopes
                .iter()
                .map(|p| (p.clone(), crate::scope_rank::build_children_rankings(content, p)))
                .collect()
        } else {
            HashMap::new()
        };

    let ingest_event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        principal: principal.clone(),
        delegate: delegate.clone(),
        thread_id: thread_id.clone(),
    });

    if let Err(err) = event_log.append(&ingest_event).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"), None);
    }
    events_appended += 1;

    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(ingest_event);
    }

    // Snapshot rankings after the event and compute per-scope deltas.
    let ranking_changes: Vec<ScopeRankChanges> = if !voted_parent_scopes.is_empty() {
        let reduced = reduced_arc.read().await;
        let content = reduced.public();
        voted_parent_scopes
            .iter()
            .filter_map(|p| {
                let before = pre_rankings.get(p)?;
                let after = crate::scope_rank::build_children_rankings(content, p);
                compute_scope_rank_changes(p.as_str(), before, &after)
            })
            .collect()
    } else {
        vec![]
    };

    Json(IngestResponse {
        ok: true,
        threads: vec![format!("#{}", thread_id)],
        events_appended,
        next: NextMoves {
            pair: "npx slugsocial pair".to_string(),
            rank: "npx slugsocial rank".to_string(),
            web: format!("https://slug.social/t/{}", thread_id),
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
    // check is auth-free; always validates against public scope.
    let v = match validate_ingest_document(&reduced, &req.text, &ScopeId::Public) {
        Ok(x) => x,
        Err((status, msg, hint)) => return api_error(status, msg, hint).into_response(),
    };
    drop(reduced);

    let delegate: Option<String> = match &req.delegate {
        None => None,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => match parse_agent(s) {
            Ok(d) => Some(d),
            Err(msg) => {
                return api_error(StatusCode::BAD_REQUEST, "invalid delegate format", Some(msg)).into_response();
            }
        },
    };
    let thread_id = canonicalize_tag(&req.thread);
    let principal = "placeholder".to_string();

    let event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        principal,
        delegate,
        thread_id: thread_id.clone(),
    });

    let mut simulated = { reduced_arc.read().await.clone() };
    simulated.apply_event(event);

    // Collect parent scopes touched by votes in this document.
    let voted_parents: Vec<CanonicalItemUrl> = {
        let mut parents: std::collections::HashSet<CanonicalItemUrl> = std::collections::HashSet::new();
        for s in &v.doc.statements {
            if let dsl::Stmt::Vote { item1, item2, .. } = s {
                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                    if let Some(p) = CanonicalItemUrl::parse(&a).and_then(|c| c.parent()) { parents.insert(p); }
                    if let Some(p) = CanonicalItemUrl::parse(&b).and_then(|c| c.parent()) { parents.insert(p); }
                }
            }
        }
        let mut out: Vec<CanonicalItemUrl> = parents.into_iter().collect();
        out.sort();
        out
    };

    // Show the scoped rankings for affected parent paths; fall back to empty if no votes.
    let rankings: Vec<slug_types::CheckScopeRanking> = voted_parents
        .iter()
        .map(|parent| {
            let scoped = crate::scope_rank::build_children_rankings(simulated.public(), parent);
            let components: Vec<RankComponent> = scoped
                .component_rankings
                .into_iter()
                .map(|comp| RankComponent {
                    pairs: comp.pairs,
                    ranking: comp
                        .ranked
                        .into_iter()
                        .map(|r| RankRow {
                            item: item_path_for_api(r.item.as_str()),
                            score: r.score,
                            percent: None,
                        })
                        .collect(),
                })
                .collect();
            slug_types::CheckScopeRanking {
                parent: item_path_for_api(parent.as_str()),
                components,
                unranked_items: scoped
                    .unranked_items
                    .into_iter()
                    .map(|it| item_path_for_api(it.as_str()))
                    .collect(),
            }
        })
        .collect();

    Json(CheckResponse {
        ok: true,
        threads: vec![format!("#{}", thread_id)],
        rankings,
        next: vec![
            "npx slugsocial ingest <file.sorter>".to_string(),
            "npx slugsocial threads".to_string(),
            format!("https://slug.social/t/{}", thread_id),
        ],
    }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    #[serde(default)]
    pub text: Option<String>,
}
