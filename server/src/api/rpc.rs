use std::collections::HashSet;
use std::sync::OnceLock;

use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use tokio::sync::oneshot;
use rand::seq::SliceRandom;
use slug_types::paths::{ForumThreadUrl, GardenItemUrl, TildeOntologyPath};
use slug_types::*;

use crate::{
    canonical_path::{canonicalize_item, canonicalize_tag},
    dsl,
    events::{Event, Ingest, ThreadCapability},
    identity::{parse_agent, parse_username},
    path_types::CanonicalItemUrl,
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    reducer::{scope_from_room_wire, ReducerState, ScopeId},
    state::{AppState, InviteState},
    write_cmd::WriteCmd,
};

use super::auth::{parse_bearer, verify_bearer_principal};
use super::helpers::{
    compute_connectivity_stats, is_pair_voted, now_ms, paginate_rankings, parse_parent_specs,
    pick_random_distinct_canonical, resolve_item, vote_touches_path,
};
use super::validate::validate_ingest_document;

fn empty_content() -> &'static crate::reducer::ContentState {
    static E: OnceLock<crate::reducer::ContentState> = OnceLock::new();
    E.get_or_init(Default::default)
}

fn content_for_room<'a>(reduced: &'a ReducerState, room: &str) -> &'a crate::reducer::ContentState {
    let scope = scope_from_room_wire(room);
    reduced.content.get(&scope).unwrap_or(empty_content())
}

type RpcErr = (String, Option<String>);

fn line_ok(r: RpcResult) -> RpcLine {
    RpcLine {
        ok: true,
        result: Some(r),
        error: None,
        hint: None,
    }
}

fn line_err(msg: impl Into<String>, hint: Option<String>) -> RpcLine {
    RpcLine {
        ok: false,
        result: None,
        error: Some(msg.into()),
        hint,
    }
}

fn can_view_scope(reduced: &ReducerState, scope: &ScopeId, principal: Option<&str>) -> bool {
    match scope {
        ScopeId::Public => true,
        ScopeId::Room(room_id) => {
            principal.is_some_and(|u| reduced.user_has_cap(room_id, u, ThreadCapability::View))
        }
    }
}

fn principal_from_optional_bearer(headers: &HeaderMap, reduced: &ReducerState) -> Result<Option<String>, RpcErr> {
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        verify_bearer_principal(headers, reduced)
            .map(Some)
            .map_err(|(_, m)| (m, None))
    } else {
        Ok(None)
    }
}

/// Authorize room-scoped reads.
///
/// Public room reads are always allowed. Private room reads require a valid bearer token and
/// explicit View capability. Unknown and unauthorized private rooms are both returned as
/// "not found"
/// to avoid resource-enumeration leaks.
fn authorize_room_read(reduced: &ReducerState, headers: &HeaderMap, room: &str) -> Result<Option<String>, RpcErr> {
    let scope = scope_from_room_wire(room);
    let ScopeId::Room(room_id) = scope else {
        return Ok(None);
    };
    let principal = match verify_bearer_principal(headers, reduced) {
        Ok(p) => p,
        Err(_) => return Err(("room not found".into(), None)),
    };
    if !reduced.rooms.contains(&room_id)
        || !reduced.user_has_cap(&room_id, &principal, ThreadCapability::View)
    {
        return Err(("room not found".into(), None));
    }
    Ok(Some(principal))
}

fn parse_capability(s: &str) -> Result<ThreadCapability, String> {
    match s {
        "view" => Ok(ThreadCapability::View),
        "post" => Ok(ThreadCapability::Post),
        "vote" => Ok(ThreadCapability::Vote),
        "add_item" => Ok(ThreadCapability::AddItem),
        "manage" => Ok(ThreadCapability::Manage),
        other => Err(format!("unknown capability: {other}")),
    }
}

fn gen_invite_token() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let tail: String = (0..16).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect();
    format!("inv_{tail}")
}

const INVITE_TTL_MS: i64 = 86_400_000;

fn capability_wire(c: ThreadCapability) -> String {
    match c {
        ThreadCapability::View => "view",
        ThreadCapability::Post => "post",
        ThreadCapability::Vote => "vote",
        ThreadCapability::AddItem => "add_item",
        ThreadCapability::Manage => "manage",
    }
    .to_string()
}

fn build_rank_response_for_content(
    content: &crate::reducer::ContentState,
    parent: Option<&str>,
    depth: usize,
    offset: usize,
    limit: Option<usize>,
    want_percent: bool,
    room_wire: &str,
) -> Result<RankResponse, RpcErr> {
    let parent_owned = parent.map(|s| s.to_string());
    let specs = parse_parent_specs(parent_owned.as_ref());
    let is_global = parent.map(|p| p.trim() == "~").unwrap_or(false);

    if !is_global && !specs.is_empty() {
        let none_exist = specs.iter().all(|spec| {
            let Some(canon) = CanonicalItemUrl::parse(spec) else { return true };
            !content.items.contains(&canon) && !content.item_children.contains_key(&canon)
        });
        if none_exist {
            return Err((
                "path not found".into(),
                Some(format!("{} does not exist", specs.join(", "))),
            ));
        }
    }

    let depth = depth.max(1);
    let rankings = if is_global {
        let all_items: Vec<CanonicalItemUrl> = content.items.iter().cloned().collect();
        crate::scope_rank::build_rankings_for_item_set(content, &all_items)
    } else if specs.is_empty() {
        crate::scope_rank::build_children_rankings(content, &CanonicalItemUrl::ontology_root())
    } else if depth > 1 {
        let items = crate::scope_rank::resolve_scope_recursive(content, &specs, depth);
        crate::scope_rank::build_rankings_for_item_set(content, &items)
    } else {
        let items = crate::scope_rank::resolve_scope(content, &specs);
        crate::scope_rank::build_rankings_for_item_set(content, &items)
    };

    let components: Vec<RankComponent> = rankings
        .component_rankings
        .into_iter()
        .map(|c| {
            let max_score = c.ranked.first().map(|r| r.score).unwrap_or(1.0).max(1e-12);
            RankComponent {
                pairs: c.pairs,
                ranking: c
                    .ranked
                    .into_iter()
                    .map(|r| RankRow {
                        item: GardenItemUrl::from_stored(&r.item, room_wire),
                        percent: if want_percent {
                            Some((r.score / max_score) * 100.0)
                        } else {
                            None
                        },
                        score: r.score,
                    })
                    .collect(),
            }
        })
        .collect();

    let prefixed_unranked: Vec<GardenItemUrl> = rankings
        .unranked_items
        .into_iter()
        .map(|s| GardenItemUrl::from_stored(&s, room_wire))
        .collect();

    let (components, unranked_items) = if offset > 0 || limit.is_some() {
        paginate_rankings(components, prefixed_unranked, offset, limit)
    } else {
        (components, prefixed_unranked)
    };

    Ok(RankResponse {
        components,
        unranked_items,
    })
}

pub async fn rpc_post_redact(
    state: &AppState,
    headers: &HeaderMap,
    post_id: String,
) -> Result<RpcResult, RpcErr> {
    let bearer = parse_bearer(headers).map_err(|(_, m)| (m, None))?;
    let (tx, rx) = oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::Redact {
            post_id: post_id.trim().to_string(),
            bearer,
            reply: tx,
        })
        .await
        .map_err(|_| ("writer unavailable".into(), None))?;
    rx.await
        .map_err(|_| ("writer dropped".into(), None))?
}

pub async fn rpc_room_delete(
    state: &AppState,
    headers: &HeaderMap,
    room: String,
) -> Result<RpcResult, RpcErr> {
    let bearer = parse_bearer(headers).map_err(|(_, m)| (m, None))?;
    let (tx, rx) = oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::RoomDelete {
            room: room.trim().to_string(),
            bearer,
            reply: tx,
        })
        .await
        .map_err(|_| ("writer unavailable".into(), None))?;
    rx.await
        .map_err(|_| ("writer dropped".into(), None))?
}

async fn rpc_post(
    state: &AppState,
    headers: &HeaderMap,
    room: String,
    thread_tag: String,
    delegate_opt: Option<String>,
    text: String,
    return_rank_diff: bool,
) -> Result<RpcResult, RpcErr> {
    let bearer = parse_bearer(headers).map_err(|(_, m)| (m, None))?;
    let (tx, rx) = oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::Post {
            room,
            thread_tag,
            delegate_opt,
            text,
            return_rank_diff,
            bearer,
            reply: tx,
        })
        .await
        .map_err(|_| ("writer unavailable".into(), None))?;
    rx.await
        .map_err(|_| ("writer dropped".into(), None))?
}

/// Post forum content using a raw bearer token (CLI `Authorization` header or browser session cookie).
pub async fn rpc_post_with_bearer(
    state: &AppState,
    bearer_token: &str,
    room: String,
    thread_tag: String,
    text: String,
) -> Result<RpcResult, RpcErr> {
    use axum::http::{header, HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    let hv = HeaderValue::from_str(&format!("Bearer {bearer_token}"))
        .map_err(|_| ("invalid session token".into(), None))?;
    headers.insert(header::AUTHORIZATION, hv);
    rpc_post(state, &headers, room, thread_tag, None, text, false).await
}

async fn rpc_check(
    state: &AppState,
    headers: &HeaderMap,
    room: String,
    text: String,
) -> Result<RpcResult, RpcErr> {
    let reduced_arc = state.reduced.clone();
    let room_key = room.trim().to_string();
    let scope = scope_from_room_wire(&room_key);
    let reduced = reduced_arc.read().await;
    let principal = authorize_room_read(&reduced, headers, &room_key)?;
    let thread_id = "check".to_string();
    let v = validate_ingest_document(&reduced, &text, &scope).map_err(|(_, m, h)| (m, h))?;

    if let ScopeId::Room(_) = scope {
        let principal = principal
            .as_ref()
            .ok_or_else(|| ("room not found".to_string(), None))?;
        let mut required: HashSet<ThreadCapability> = HashSet::new();
        required.insert(ThreadCapability::View);
        for stmt in &v.doc.statements {
            match stmt {
                dsl::Stmt::Vote { .. } => {
                    required.insert(ThreadCapability::Vote);
                }
                dsl::Stmt::Item { .. } => {
                    required.insert(ThreadCapability::AddItem);
                }
                dsl::Stmt::Prose { .. } => {
                    required.insert(ThreadCapability::Post);
                }
            }
        }
        let missing: Vec<_> = required
            .iter()
            .filter(|cap| !reduced.user_has_cap(&room_key, principal, **cap))
            .collect();
        if !missing.is_empty() {
            return Err(("insufficient capabilities for this room".into(), None));
        }
    }
    drop(reduced);

    let delegate: Option<String> = None;
    let principal = principal.unwrap_or_else(|| "placeholder".to_string());
    let event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        principal,
        delegate,
        room_id: room_key.clone(),
        thread_tag: thread_id.clone(),
    });

    let mut simulated = { reduced_arc.read().await.clone() };
    simulated.apply_event(event);

    let voted_parents: Vec<CanonicalItemUrl> = {
        let mut parents: HashSet<CanonicalItemUrl> = HashSet::new();
        for s in &v.doc.statements {
            if let dsl::Stmt::Vote { item1, item2, .. } = s {
                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                    if let Some(p) = a.parent() { parents.insert(p); }
                    if let Some(p) = b.parent() { parents.insert(p); }
                }
            }
        }
        let mut out: Vec<CanonicalItemUrl> = parents.into_iter().collect();
        out.sort();
        out
    };

    let rankings: Vec<CheckScopeRanking> = voted_parents
        .iter()
        .map(|parent| {
            let scoped_content = simulated
                .content_for_scope(&scope)
                .unwrap_or_else(|| simulated.public());
            let scoped = crate::scope_rank::build_children_rankings(scoped_content, parent);
            let components: Vec<RankComponent> = scoped
                .component_rankings
                .into_iter()
                .map(|comp| RankComponent {
                    pairs: comp.pairs,
                    ranking: comp
                        .ranked
                        .into_iter()
                        .map(|r| RankRow {
                            item: GardenItemUrl::from_stored(&r.item, &room_key),
                            score: r.score,
                            percent: None,
                        })
                        .collect(),
                })
                .collect();
            CheckScopeRanking {
                parent: GardenItemUrl::from_stored(parent, &room_key).into_inner(),
                components,
                unranked_items: scoped
                    .unranked_items
                    .into_iter()
                    .map(|it| GardenItemUrl::from_stored(&it, &room_key))
                    .collect(),
            }
        })
        .collect();

    let check_next = if room_key == "public" {
        vec![
            "npx slugsocial public forum post <TAG> --delegate <uuid:rig:model>".to_string(),
            "npx slugsocial public forum list".to_string(),
            ForumThreadUrl::from_room_tag("public", &thread_id).into_inner(),
        ]
    } else {
        vec![
            format!("npx slugsocial private {room_key} forum post <TAG> --delegate <uuid:rig:model>"),
            format!("npx slugsocial private {room_key} forum list"),
            ForumThreadUrl::from_room_tag(&room_key, &thread_id).into_inner(),
        ]
    };

    Ok(RpcResult::CheckOk {
        rankings,
        threads: vec![format!("#{}", thread_id)],
        next: check_next,
    })
}

fn forum_scope_from_room(room: &str) -> ScopeId {
    scope_from_room_wire(room)
}

fn rpc_list_forum_threads(reduced: &ReducerState, room: &str) -> ThreadsResponse {
    let scope = forum_scope_from_room(room);
    let mut out: Vec<ThreadSummary> = reduced
        .forum_threads
        .iter()
        .filter(|((s, _), _)| s == &scope)
        .map(|((_, tag), ts)| ThreadSummary {
            thread: format!("#{tag}"),
            last_activity_ts: ts.last_activity_ts,
            web: ForumThreadUrl::from_room_tag(room, tag),
        })
        .collect();
    out.sort_by(|a, b| b.last_activity_ts.cmp(&a.last_activity_ts));
    ThreadsResponse { threads: out }
}

fn rpc_forum_thread_detail(
    reduced: &ReducerState,
    room: &str,
    thread_tag: &str,
    offset: usize,
    limit: usize,
    since: Option<i64>,
    before: Option<i64>,
    actor_prefix: &str,
    post_id: Option<&str>,
) -> Result<ThreadDetailResponse, RpcErr> {
    let scope = forum_scope_from_room(room);
    let tag = canonicalize_tag(thread_tag);
    let key = (scope.clone(), tag.clone());

    if let Some(pid) = post_id {
        let thread_ids = reduced.ingests_by_scope_thread.get(&key);
        let index = thread_ids.and_then(|ids| {
            ids.iter().rev().enumerate().find(|(_, id)| *id == pid).map(|(i, _)| i)
        });
        return match index.and_then(|idx| reduced.ingests_by_id.get(pid).map(|ing| (idx, ing))) {
            None => Err(("post not found".into(), None)),
            Some((idx, ing)) => {
                let redacted = reduced.redacted_posts.contains(&ing.id);
                let redacted_at_ts = if redacted {
                    reduced.post_redact_ts.get(&ing.id).copied()
                } else {
                    None
                };
                Ok(ThreadDetailResponse {
                    thread: format!("#{}", tag),
                    items: vec![ThreadItem::Post {
                        id: ing.id.clone(),
                        index: idx,
                        ts: ing.ts,
                        actor: ing.principal.clone(),
                        body: if redacted { String::new() } else { ing.raw.clone() },
                        truncated: false,
                        redacted,
                        redacted_at_ts,
                    }],
                    total: 1,
                    offset: idx,
                })
            }
        };
    }

    let all_ids: Vec<String> = reduced
        .ingests_by_scope_thread
        .get(&key)
        .map(|q| q.iter().rev().cloned().collect())
        .unwrap_or_default();

    let filtered: Vec<(usize, _)> = all_ids
        .into_iter()
        .enumerate()
        .filter_map(|(idx, id)| reduced.ingests_by_id.get(&id).map(|ing| (idx, ing.clone())))
        .filter(|(_, ing)| since.map_or(true, |s| ing.ts >= s))
        .filter(|(_, ing)| before.map_or(true, |b| ing.ts < b))
        .filter(|(_, ing)| actor_prefix.is_empty() || ing.principal.to_lowercase().starts_with(actor_prefix))
        .collect();

    let total = filtered.len();
    const MAX_BODY: usize = 2000;
    let items: Vec<ThreadItem> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(idx, ing)| {
            let redacted = reduced.redacted_posts.contains(&ing.id);
            let redacted_at_ts = if redacted {
                reduced.post_redact_ts.get(&ing.id).copied()
            } else {
                None
            };
            let (body, truncated) = if redacted {
                (String::new(), false)
            } else if ing.raw.len() > MAX_BODY {
                (ing.raw[..MAX_BODY].to_string(), true)
            } else {
                (ing.raw.clone(), false)
            };
            ThreadItem::Post {
                id: ing.id.clone(),
                index: idx,
                ts: ing.ts,
                actor: ing.principal.clone(),
                body,
                truncated,
                redacted,
                redacted_at_ts,
            }
        })
        .collect();

    Ok(ThreadDetailResponse {
        thread: format!("#{}", tag),
        items,
        total,
        offset,
    })
}

fn tokenize_query(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn text_contains_all(text: &str, words: &[String]) -> bool {
    let lower = text.to_lowercase();
    words.iter().all(|w| lower.contains(w.as_str()))
}

fn text_contains_any(text: &str, words: &[String]) -> usize {
    let lower = text.to_lowercase();
    words.iter().filter(|w| lower.contains(w.as_str())).count()
}

fn snippet_around(text: &str, words: &[String], max_len: usize) -> String {
    let lower = text.to_lowercase();
    let first_pos = words.iter().filter_map(|w| lower.find(w.as_str())).min().unwrap_or(0);
    let start = first_pos.saturating_sub(max_len / 3);
    let start = if start > 0 {
        let mut i = start;
        while i < text.len() && !text.is_char_boundary(i) { i += 1; }
        text[i..].find(' ').map(|j| i + j + 1).unwrap_or(i)
    } else {
        0
    };
    let mut end = (start + max_len).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) { end += 1; }
    text[start..end].to_string()
}

fn rpc_search(reduced: &ReducerState, q: &str, limit: usize, principal: Option<&str>) -> SearchResponse {
    let words = tokenize_query(q);
    if words.is_empty() {
        return SearchResponse {
            items: vec![],
            threads: vec![],
            posts: vec![],
        };
    }
    let content = reduced.public();
    let mut scored_items: Vec<(u32, SearchItemHit)> = Vec::new();
    for item in &content.items {
        let mut score: u32 = 0;
        if text_contains_all(item.as_str(), &words) { score += 10; }
        else if text_contains_any(item.as_str(), &words) > 0 { score += 5; }
        if let Some(body) = content.item_bodies.get(item) {
            if text_contains_all(body, &words) { score += 6; }
            else {
                let any = text_contains_any(body, &words);
                if any > 0 { score += any as u32; }
            }
        }
        if score > 0 {
            scored_items.push((score, SearchItemHit {
                path: GardenItemUrl::from_storage_str(item.as_str(), "public"),
                body: content.item_bodies.get(item).map(|b| snippet_around(b, &words, 120)),
            }));
        }
    }
    let mut scored_threads: Vec<(u32, i64, SearchThreadHit)> = Vec::new();
    for ((scope, tag), ts) in &reduced.forum_threads {
        if scope != &ScopeId::Public { continue; }
        let mut score: u32 = 0;
        if text_contains_all(tag, &words) { score += 8; }
        else if text_contains_any(tag, &words) > 0 { score += 4; }
        if score > 0 {
            let post_count = reduced
                .ingests_by_scope_thread
                .get(&(ScopeId::Public, tag.clone()))
                .map(|q| q.len())
                .unwrap_or(0);
            scored_threads.push((score, ts.last_activity_ts, SearchThreadHit {
                tag: format!("#{tag}"),
                post_count,
                last_activity: ts.last_activity_ts,
            }));
        }
    }
    let mut scored_posts: Vec<(u32, i64, SearchPostHit)> = Vec::new();
    for (id, ingest) in &reduced.ingests_by_id {
        let mut score: u32 = 0;
        if text_contains_all(&ingest.raw, &words) { score += 4; }
        else {
            let any = text_contains_any(&ingest.raw, &words);
            if any > 0 { score += any as u32; }
        }
        if score > 0 {
            let Some((scope, tag)) = reduced
                .ingests_by_scope_thread
                .iter()
                .find_map(|((scope, tag), ids)| {
                    if ids.contains(id) {
                        Some((scope.clone(), tag.clone()))
                    } else {
                        None
                    }
                }) else {
                    continue;
                };
            if !can_view_scope(reduced, &scope, principal) {
                continue;
            }
            let thread = match scope {
                ScopeId::Public => format!("#{tag}"),
                ScopeId::Room(rid) => format!("{rid}/#{tag}"),
            };
            scored_posts.push((score, ingest.ts, SearchPostHit {
                thread,
                actor: ingest.principal.clone(),
                snippet: snippet_around(&ingest.raw, &words, 160),
                ts: ingest.ts,
            }));
        }
    }
    scored_items.sort_by(|a, b| b.0.cmp(&a.0));
    scored_threads.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    scored_posts.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    scored_items.truncate(limit);
    scored_threads.truncate(limit);
    scored_posts.truncate(limit);
    SearchResponse {
        items: scored_items.into_iter().map(|(_, h)| h).collect(),
        threads: scored_threads.into_iter().map(|(_, _, h)| h).collect(),
        posts: scored_posts.into_iter().map(|(_, _, h)| h).collect(),
    }
}

async fn rpc_get_pair(state: &AppState, room: String, parent_path: String) -> Result<RpcResult, RpcErr> {
    let scope = scope_from_room_wire(&room);
    let reduced_arc = state.reduced.clone();
    let pool: Vec<CanonicalItemUrl> = {
        let reduced = reduced_arc.read().await;
        let content = content_for_room(&reduced, &room);
        let tmp = if parent_path.trim().is_empty() {
            None
        } else {
            Some(parent_path.clone())
        };
        let specs = parse_parent_specs(tmp.as_ref());
        if specs.is_empty() {
            content.ranking_group.idx_to_item.clone()
        } else {
            crate::scope_rank::resolve_scope(content, &specs)
        }
    };
    if pool.len() < 2 {
        return Err((
            format!("need at least 2 items under parent /{}", parent_path.trim()),
            Some("add items via ingest".into()),
        ));
    }
    let selected: Option<(CanonicalItemUrl, CanonicalItemUrl)> = {
        let mut reduced = reduced_arc.write().await;
        let content = reduced.content.entry(scope.clone()).or_default();
        let group = &mut content.ranking_group;
        if group.idx_to_item.is_empty() {
            pick_random_distinct_canonical(&pool)
        } else {
            let mut rng = rand::thread_rng();
            let idxs: Vec<usize> = pool
                .iter()
                .filter_map(|it| group.item_to_idx.get(it).copied())
                .collect();
            let ranked = ranked_items_subset(group, &idxs, 10000, 1e-8);
            let ranked_set: HashSet<CanonicalItemUrl> = ranked.iter().map(|r| r.item.clone()).collect();
            let unsorted: Vec<CanonicalItemUrl> = pool
                .iter()
                .filter(|it| !ranked_set.contains(*it))
                .cloned()
                .collect();
            let mut pick: Option<(CanonicalItemUrl, CanonicalItemUrl)> = None;
            if !unsorted.is_empty() {
                if let Some(left) = unsorted.choose(&mut rng).cloned() {
                    let mut candidates: Vec<CanonicalItemUrl> = if !ranked.is_empty() {
                        ranked.iter().map(|r| r.item.clone()).collect()
                    } else {
                        pool.clone()
                    };
                    candidates.retain(|c| c != &left);
                    if let Some(right) = candidates.choose(&mut rng).cloned() {
                        pick = Some((left, right));
                    }
                }
            } else if ranked.len() >= 2 {
                for i in 0..(ranked.len().saturating_sub(1)) {
                    let a = ranked[i].item.as_str();
                    let b = ranked[i + 1].item.as_str();
                    if a != b && !is_pair_voted(group, a, b) {
                        pick = Some((ranked[i].item.clone(), ranked[i + 1].item.clone()));
                        break;
                    }
                }
                if pick.is_none() {
                    for _ in 0..64 {
                        let (Some(a), Some(b)) = (pool.choose(&mut rng).cloned(), pool.choose(&mut rng).cloned()) else { break; };
                        if a != b && !is_pair_voted(group, a.as_str(), b.as_str()) {
                            pick = Some((a, b));
                            break;
                        }
                    }
                }
            }
            pick.or_else(|| pick_random_distinct_canonical(&pool))
        }
    };
    let Some((left, right)) = selected else {
        return Err(("need at least 2 items".into(), None));
    };
    let reduced = reduced_arc.read().await;
    let content = content_for_room(&reduced, &room);
    let left_key = left.clone();
    let right_key = right.clone();
    let lb = content.item_bodies.get(&left_key).cloned();
    let rb = content.item_bodies.get(&right_key).cloned();
    let th: Vec<String> = content
        .item_threads
        .get(&left_key)
        .into_iter()
        .chain(content.item_threads.get(&right_key))
        .flat_map(|s| s.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let cs = compute_connectivity_stats(&content.ranking_group, &pool);
    Ok(RpcResult::Pair(PairResponse {
        left: GardenItemUrl::from_stored(&left, &room),
        right: GardenItemUrl::from_stored(&right, &room),
        left_body: lb,
        right_body: rb,
        threads: th,
        connectivity: Some(cs),
    }))
}

pub async fn handle_rpc_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(RpcBatch(commands)): Json<RpcBatch>,
) -> impl IntoResponse {
    let mut results = Vec::with_capacity(commands.len());

    for cmd in commands {
        let line = match cmd {
            RpcCommand::Post {
                room,
                thread_tag,
                delegate,
                text,
                return_rank_diff,
            } => match rpc_post(&state, &headers, room, thread_tag, delegate, text, return_rank_diff).await {
                Ok(r) => line_ok(r),
                Err((e, h)) => line_err(e, h),
            },
            RpcCommand::Check { room, text } => match rpc_check(&state, &headers, room, text).await {
                Ok(r) => line_ok(r),
                Err((e, h)) => line_err(e, h),
            },
            RpcCommand::GetGardenRank {
                room,
                parent_path,
                depth,
                offset,
                limit,
                percent,
            } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let popt = if parent_path.trim().is_empty() {
                    None
                } else {
                    Some(parent_path.as_str())
                };
                match build_rank_response_for_content(
                    content,
                    popt,
                    depth.unwrap_or(1),
                    offset.unwrap_or(0),
                    limit,
                    percent.unwrap_or(false),
                    &room,
                ) {
                    Ok(r) => line_ok(RpcResult::GardenRank(r)),
                    Err((e, h)) => line_err(e, h),
                }
                }
            }
            RpcCommand::GetGardenItem {
                room,
                item_path,
                full,
            } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let item_str = canonicalize_item(&item_path);
                let item = CanonicalItemUrl(item_str.clone());
                if !content.items.contains(&item) {
                    line_err(
                        "item not found",
                        Some(format!("{} does not exist", GardenItemUrl::from_storage_str(&item_str, &room))),
                    )
                } else {
                    let want_full = full.unwrap_or(false);
                    let (body, truncated, body_len) = match content.item_bodies.get(&item) {
                        None => (None, false, 0),
                        Some(raw) => {
                            let char_len = raw.chars().count();
                            if !want_full && char_len > MAX_ITEM_BODY_PREVIEW_CHARS {
                                let byte_end = raw
                                    .char_indices()
                                    .nth(MAX_ITEM_BODY_PREVIEW_CHARS)
                                    .map(|(i, _)| i)
                                    .unwrap_or(raw.len());
                                (Some(raw[..byte_end].to_string()), true, char_len)
                            } else {
                                (Some(raw.clone()), false, 0)
                            }
                        }
                    };
                    let threads: Vec<String> = content
                        .item_threads
                        .get(&item)
                        .map(|s| s.iter().cloned().collect())
                        .unwrap_or_default();
                    line_ok(RpcResult::GardenItem(ItemResponse {
                        item: GardenItemUrl::from_storage_str(&item_str, &room),
                        body,
                        truncated,
                        body_len,
                        threads,
                    }))
                }
                }
            }
            RpcCommand::GetForumThread {
                room,
                thread_tag,
                offset,
                limit,
                since,
                before,
                actor,
                post_id,
            } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let actor_prefix = match actor.as_deref().map(str::trim) {
                    None | Some("") => Ok(String::new()),
                    Some(s) => parse_username(s).map_err(|msg| ("invalid actor filter".to_string(), Some(msg))),
                };
                match actor_prefix {
                    Err((e, h)) => line_err(e, h),
                    Ok(actor_prefix) => {
                        let offset = offset.unwrap_or(0);
                        let limit = limit.unwrap_or(10).clamp(1, 500);
                        match rpc_forum_thread_detail(
                            &reduced,
                            &room,
                            &thread_tag,
                            offset,
                            limit,
                            since,
                            before,
                            &actor_prefix,
                            post_id.as_deref(),
                        ) {
                            Ok(r) => line_ok(RpcResult::ForumThread(r)),
                            Err((e, h)) => line_err(e, h),
                        }
                    }
                }
                }
            }
            RpcCommand::ListForumThreads { room } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                    line_ok(RpcResult::ForumThreads(rpc_list_forum_threads(&reduced, &room)))
                }
            }
            RpcCommand::RoomCreate { slug } => {
                match parse_bearer(&headers) {
                    Err((_, m)) => line_err(m, None),
                    Ok(bearer) => {
                        let (tx, rx) = oneshot::channel();
                        match state
                            .write_tx
                            .send(WriteCmd::RoomCreate {
                                slug,
                                bearer,
                                reply: tx,
                            })
                            .await
                        {
                            Err(_) => line_err("writer unavailable", None),
                            Ok(()) => match rx.await {
                                Err(_) => line_err("writer dropped", None),
                                Ok(Ok(r)) => line_ok(r),
                                Ok(Err((msg, hint))) => line_err(msg, hint),
                            },
                        }
                    }
                }
            }
            RpcCommand::RoomGrant {
                room,
                username,
                capabilities,
            } => {
                match parse_bearer(&headers) {
                    Err((_, m)) => line_err(m, None),
                    Ok(bearer) => {
                        let (tx, rx) = oneshot::channel();
                        match state
                            .write_tx
                            .send(WriteCmd::Grant {
                                room,
                                username,
                                capabilities,
                                bearer,
                                reply: tx,
                            })
                            .await
                        {
                            Err(_) => line_err("writer unavailable", None),
                            Ok(()) => match rx.await {
                                Err(_) => line_err("writer dropped", None),
                                Ok(Ok(r)) => line_ok(r),
                                Ok(Err((msg, hint))) => line_err(msg, hint),
                            },
                        }
                    }
                }
            }
            // Invite links are stored in AppState only (not JSONL); they do not survive restart.
            RpcCommand::RoomMintInvite {
                room,
                capabilities,
                max_uses,
            } => {
                let principal = {
                    let reduced = state.reduced.read().await;
                    verify_bearer_principal(&headers, &*reduced)
                };
                match principal {
                    Err((_, m)) => line_err(m, None),
                    Ok(principal) => {
                        let can_manage = {
                            let reduced = state.reduced.read().await;
                            reduced.user_has_cap(&room, &principal, ThreadCapability::Manage)
                        };
                        if !can_manage {
                            line_err("requires Manage capability", None)
                        } else if capabilities.is_empty() {
                            line_err("capabilities must not be empty", None)
                        } else {
                            match capabilities
                                .iter()
                                .map(|c| parse_capability(c.trim()))
                                .collect::<Result<Vec<ThreadCapability>, String>>()
                            {
                                Err(msg) => line_err(msg, None),
                                Ok(caps) => {
                                    let max_uses = max_uses.max(1).min(100_000);
                                    let now = now_ms();
                                    let expires_at_ms = now + INVITE_TTL_MS;
                                    let token = loop {
                                        let t = gen_invite_token();
                                        let taken = {
                                            let invites = state.invites.read().await;
                                            invites.contains_key(&t)
                                        };
                                        if !taken {
                                            break t;
                                        }
                                    };
                                    let inv = InviteState {
                                        room_id: room.clone(),
                                        capabilities: caps,
                                        expires_at_ms,
                                        max_uses,
                                        current_uses: 0,
                                        inviter: principal,
                                    };
                                    state.invites.write().await.insert(token.clone(), inv);
                                    let public_url = std::env::var("SLUG_PUBLIC_URL")
                                        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
                                    let invite_url = format!("{public_url}/join/{token}");
                                    line_ok(RpcResult::RoomInviteMinted {
                                        invite_url,
                                        expires_at_ms: Some(expires_at_ms),
                                        max_uses,
                                    })
                                }
                            }
                        }
                    }
                }
            }
            RpcCommand::RoomAudit { room } => {
                let principal = {
                    let reduced = state.reduced.read().await;
                    verify_bearer_principal(&headers, &*reduced)
                };
                match principal {
                    Err((_, m)) => line_err(m, None),
                    Ok(principal) => {
                        let reduced = state.reduced.read().await;
                        if !reduced.rooms.contains(&room) {
                            line_err("unknown room", None)
                        } else {
                            let can_audit = reduced.user_has_cap(&room, &principal, ThreadCapability::View)
                                || reduced.user_has_cap(&room, &principal, ThreadCapability::Manage);
                            if !can_audit {
                                line_err("requires View or Manage capability", None)
                            } else {
                                let grants: Vec<RoomAuditEntry> = reduced
                                    .grants
                                    .get(&room)
                                    .map(|m| {
                                        let mut v: Vec<RoomAuditEntry> = m
                                            .iter()
                                            .map(|(username, caps)| {
                                                let mut c: Vec<String> =
                                                    caps.iter().copied().map(capability_wire).collect();
                                                c.sort();
                                                RoomAuditEntry {
                                                    username: username.clone(),
                                                    capabilities: c,
                                                }
                                            })
                                            .collect();
                                        v.sort_by(|a, b| a.username.cmp(&b.username));
                                        v
                                    })
                                    .unwrap_or_default();
                                line_ok(RpcResult::RoomAudit(RoomAuditResponse { room, grants }))
                            }
                        }
                    }
                }
            }
            RpcCommand::RoomList => {
                let principal = {
                    let reduced = state.reduced.read().await;
                    verify_bearer_principal(&headers, &*reduced)
                };
                match principal {
                    Err((_, m)) => line_err(m, None),
                    Ok(principal) => {
                        let reduced = state.reduced.read().await;
                        let rooms: Vec<String> = reduced
                            .grants
                            .iter()
                            .filter(|(_, members)| members.contains_key(&principal))
                            .map(|(room, _)| room.clone())
                            .collect();
                        line_ok(RpcResult::RoomList(RoomListResponse { rooms }))
                    }
                }
            }
            RpcCommand::RoomDelete { room } => match rpc_room_delete(&state, &headers, room).await {
                Ok(r) => line_ok(r),
                Err((e, h)) => line_err(e, h),
            },
            RpcCommand::RoomRevoke {
                room,
                username,
                capability,
            } => {
                match parse_bearer(&headers) {
                    Err((_, m)) => line_err(m, None),
                    Ok(bearer) => {
                        let (tx, rx) = oneshot::channel();
                        match state
                            .write_tx
                            .send(WriteCmd::Revoke {
                                room,
                                username,
                                capability,
                                bearer,
                                reply: tx,
                            })
                            .await
                        {
                            Err(_) => line_err("writer unavailable", None),
                            Ok(()) => match rx.await {
                                Err(_) => line_err("writer dropped", None),
                                Ok(Ok(r)) => line_ok(r),
                                Ok(Err((msg, hint))) => line_err(msg, hint),
                            },
                        }
                    }
                }
            },
            RpcCommand::GetGlobalRank {
                room,
                limit,
                offset,
                percent,
            } => {
                {
                    let reduced = state.reduced.read().await;
                    if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                        results.push(line_err(e, h));
                        continue;
                    }
                }
                const DEFAULT_LIMIT: usize = 50;
                const MAX_LIMIT: usize = 500;
                let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
                let offset = offset.unwrap_or(0);
                let want_percent = percent.unwrap_or(false);
                let reduced = state.reduced.read().await;
                let mut content = content_for_room(&reduced, &room).clone();
                let group = &mut content.ranking_group;
                let n = group.idx_to_item.len();
                let (mut comps, _) = connected_components_from_voted_pairs(
                    n, group.voted_pairs.iter().copied(),
                );
                comps.sort_by(|a, b| b.len().cmp(&a.len()));

                let mut ranked: Vec<RankRow> = Vec::new();
                for comp in &comps {
                    let items = ranked_items_subset(group, comp, 10000, 1e-8);
                    let top = items.first().map(|r| r.score).unwrap_or(1.0);
                    let bot = items.last().map(|r| r.score).unwrap_or(0.0);
                    let range = (top - bot).max(1e-12);
                    for r in items {
                        let pct = want_percent.then(|| ((r.score - bot) / range * 100.0).clamp(0.0, 100.0));
                        ranked.push(RankRow {
                            item: GardenItemUrl::from_stored(&r.item, &room),
                            score: r.score,
                            percent: pct,
                        });
                    }
                }

                let ranked_total = ranked.len();
                let mut unranked: Vec<CanonicalItemUrl> = content
                    .items
                    .iter()
                    .filter(|it| !group.item_to_idx.contains_key(*it))
                    .cloned()
                    .collect();
                unranked.sort();
                let unranked_total = unranked.len();

                let page: Vec<RankRow> = ranked
                    .into_iter()
                    .chain(unranked.into_iter().map(|it| RankRow {
                        item: GardenItemUrl::from_stored(&it, &room),
                        score: 0.0,
                        percent: want_percent.then_some(0.0),
                    }))
                    .skip(offset)
                    .take(limit)
                    .collect();

                line_ok(RpcResult::GlobalRank(GlobalRankResponse {
                    ranked_total,
                    unranked_total,
                    offset,
                    limit,
                    items: page,
                }))
            }
            RpcCommand::GetPair { room, parent_path } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                    drop(reduced);
                    match rpc_get_pair(&state, room, parent_path).await {
                        Ok(r) => line_ok(r),
                        Err((e, h)) => line_err(e, h),
                    }
                }
            }
            RpcCommand::GetMatchup {
                room,
                item_path,
                limit,
            } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let item_str = canonicalize_item(&item_path);
                let item = CanonicalItemUrl(item_str.clone());
                let limit = limit.unwrap_or(50).clamp(1, 200);
                if !content.items.contains(&item) {
                    line_err(
                        "item not found",
                        Some(format!("{} does not exist", GardenItemUrl::from_storage_str(&item_str, &room))),
                    )
                } else {
                    let votes: Vec<VoteRow> = content
                        .item_votes
                        .get(&item)
                        .map(|q| {
                            q.iter()
                                .take(limit)
                                .map(|v| VoteRow {
                                    ts: v.ts,
                                    a: GardenItemUrl::from_stored(&v.a, &room),
                                    b: GardenItemUrl::from_stored(&v.b, &room),
                                    ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                                    actor: Some(v.principal.clone()),
                                    body: v.body.clone(),
                                    thread: Some(v.thread_tag.clone()),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    line_ok(RpcResult::Matchup(MatchupResponse {
                        item: GardenItemUrl::from_storage_str(&item_str, &room),
                        votes,
                    }))
                }
                }
            },
            RpcCommand::GetRankHistory { room, item_path } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let scope = scope_from_room_wire(&room);
                let item_str = canonicalize_item(&item_path);
                let item = CanonicalItemUrl(item_str.clone());
                let entries = content.rank_history.get(&item).cloned().unwrap_or_default();
                let history: Vec<RankHistoryRow> = entries.iter().map(|e| {
                    let caused_by: Vec<VoteRow> = reduced.ingests_by_id.get(&e.post_id)
                        .and_then(|ing| crate::dsl::parse_full(&ing.raw).ok())
                        .map(|doc| {
                            doc.statements.into_iter().filter_map(|s| {
                                if let crate::dsl::Stmt::Vote { item1, item2, ratio_left, ratio_right, explanation } = s {
                                    let a = canonicalize_item(&item1);
                                    let b = canonicalize_item(&item2);
                                    if a == item_str || b == item_str {
                                        Some(VoteRow {
                                            ts: e.ts,
                                            a: GardenItemUrl::from_storage_str(&a, &room),
                                            b: GardenItemUrl::from_storage_str(&b, &room),
                                            ratio: format!("{}:{}", ratio_left, ratio_right),
                                            actor: reduced.ingests_by_id.get(&e.post_id).map(|ing| ing.principal.clone()),
                                            body: explanation,
                                            thread: Some(format!("#{}", e.thread)),
                                        })
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            }).collect()
                        })
                        .unwrap_or_default();
                    let thread_post_index = reduced
                        .ingests_by_scope_thread
                        .get(&(scope.clone(), e.thread.clone()))
                        .and_then(|q| q.iter().rev().position(|id| id == &e.post_id))
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    RankHistoryRow {
                        ts: e.ts,
                        scope_rank: e.scope_rank,
                        scope_rank_delta: e.scope_rank_delta,
                        scope_total: e.scope_total,
                        global_rank: e.global_rank,
                        global_rank_delta: e.global_rank_delta,
                        global_total: e.global_total,
                        score: e.score,
                        thread: format!("#{}", e.thread),
                        thread_post_index,
                        caused_by,
                    }
                }).collect();
                line_ok(RpcResult::RankHistory(RankHistoryResponse {
                    item: GardenItemUrl::from_storage_str(&item_str, &room),
                    history,
                }))
                }
            },
            RpcCommand::GetLeaves { room } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let parents: HashSet<&str> = content.item_children.keys().map(|s| s.as_str()).collect();
                let mut paths: Vec<GardenItemUrl> = content
                    .items
                    .iter()
                    .filter(|p| !parents.contains(p.as_str()))
                    .map(|p| GardenItemUrl::from_stored(p, &room))
                    .collect();
                paths.sort();
                line_ok(RpcResult::Leaves(LeavesResponse { paths }))
                }
            },
            RpcCommand::GetPaths { room } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let out: Vec<PathSummary> = content
                    .item_children
                    .get(&CanonicalItemUrl::ontology_root())
                    .map(|roots| {
                        let mut v: Vec<PathSummary> = roots.iter()
                            .map(|path| {
                                let children = content.item_children.get(path.as_str()).map(|s| s.len()).unwrap_or(0);
                                PathSummary {
                                    path: TildeOntologyPath::from_stored(path),
                                    children,
                                    web: GardenItemUrl::from_stored(path, &room),
                                }
                            }).collect();
                        v.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
                        v
                    })
                    .unwrap_or_default();
                line_ok(RpcResult::Paths(PathsResponse { paths: out }))
                }
            },
            RpcCommand::GetRecentVotes {
                room,
                parent,
                limit,
            } => {
                let reduced = state.reduced.read().await;
                if let Err((e, h)) = authorize_room_read(&reduced, &headers, &room) {
                    line_err(e, h)
                } else {
                let content = content_for_room(&reduced, &room);
                let group = &content.ranking_group;
                let limit = limit.unwrap_or(25).clamp(1, 200);
                let iter = group.recent_votes.iter();
                let iter: Box<dyn Iterator<Item = _>> = if let Some(p) = &parent {
                    let parent_can = canonicalize_item(p);
                    Box::new(iter.filter(move |v| vote_touches_path(v.a.as_str(), v.b.as_str(), &parent_can)))
                } else {
                    Box::new(iter)
                };
                let out: Vec<VoteRow> = iter
                    .take(limit)
                    .map(|v| VoteRow {
                        ts: v.ts,
                        a: GardenItemUrl::from_stored(&v.a, &room),
                        b: GardenItemUrl::from_stored(&v.b, &room),
                        ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                        actor: Some(v.principal.clone()),
                        body: v.body.clone(),
                        thread: Some(v.thread_tag.clone()),
                    }).collect();
                line_ok(RpcResult::RecentVotes(RecentVotesResponse { votes: out }))
                }
            },
            RpcCommand::Search { query } => {
                let reduced = state.reduced.read().await;
                let limit = 50usize.min(200);
                match principal_from_optional_bearer(&headers, &reduced) {
                    Ok(principal) => line_ok(RpcResult::Search(rpc_search(&reduced, &query, limit, principal.as_deref()))),
                    Err((e, h)) => line_err(e, h),
                }
            },
            RpcCommand::GetFeed {
                delegate,
                since,
                limit,
            } => {
                const DEFAULT_LIMIT: usize = 50;
                const MAX_LIMIT: usize = 200;
                let reduced = state.reduced.read().await;
                match verify_bearer_principal(&headers, &reduced) {
                    Err((_, m)) => line_err(m, None),
                    Ok(viewer) => {
                        let delegate_parsed: Option<Result<String, String>> = delegate
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(parse_agent);
                        match delegate_parsed {
                            Some(Err(msg)) => {
                                drop(reduced);
                                line_err("invalid delegate", Some(msg))
                            }
                            Some(Ok(delegate_stored)) => {
                                let line = if reduced.agent_bindings.get(&delegate_stored) != Some(&viewer) {
                                    line_err(
                                        "not your delegate",
                                        Some("this delegate is not bound to your signed-in account".into()),
                                    )
                                } else {
                                    let since_default = reduced
                                        .ingests_ordered
                                        .iter()
                                        .rev()
                                        .filter_map(|id| reduced.ingests_by_id.get(id))
                                        .find(|ing| {
                                            if ing.delegate.as_deref() != Some(delegate_stored.as_str()) {
                                                return false;
                                            }
                                            let scope = scope_from_room_wire(&ing.room_id);
                                            can_view_scope(&reduced, &scope, Some(viewer.as_str()))
                                        })
                                        .map(|ing| ing.ts);
                                    let since = since.or(since_default);
                                    let cutoff = since.unwrap_or(0);
                                    let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
                                    let matching: Vec<&str> = reduced.ingests_ordered.iter().rev()
                                        .map(|id| id.as_str())
                                        .take_while(|id| reduced.ingests_by_id.get(*id).map_or(false, |ing| ing.ts > cutoff))
                                        .filter(|id| {
                                            reduced.ingests_by_id.get(*id).is_some_and(|ing| {
                                                let scope = scope_from_room_wire(&ing.room_id);
                                                can_view_scope(&reduced, &scope, Some(viewer.as_str()))
                                            })
                                        })
                                        .filter(|id| !reduced.redacted_posts.contains(*id))
                                        .collect();
                                    let total = matching.len();
                                    let posts: Vec<FeedPost> = matching.into_iter()
                                        .take(limit)
                                        .filter_map(|id| reduced.ingests_by_id.get(id))
                                        .map(|ing| {
                                            let scope = scope_from_room_wire(&ing.room_id);
                                            let thread_post_index = reduced
                                                .ingests_by_scope_thread
                                                .get(&(scope, ing.thread_tag.clone()))
                                                .and_then(|q| {
                                                    q.iter().rev().position(|pid| pid == &ing.id).map(|i| i + 1)
                                                });
                                            FeedPost {
                                                ts: ing.ts,
                                                id: ing.id.clone(),
                                                thread: Some(ing.thread_tag.clone()),
                                                thread_post_index,
                                                body: ing.raw.clone(),
                                            }
                                        })
                                        .collect();
                                    line_ok(RpcResult::Feed(FeedResponse {
                                        delegate: Some(delegate_stored),
                                        since,
                                        posts,
                                        total,
                                    }))
                                };
                                drop(reduced);
                                line
                            }
                            None => {
                                // Session catch-up: last time *you* posted anything (delegate or not), so revisiting
                                // an old chat with only a token still gets a sane cutoff.
                                let since_default = reduced
                                    .ingests_ordered
                                    .iter()
                                    .rev()
                                    .filter_map(|id| reduced.ingests_by_id.get(id))
                                    .find(|ing| {
                                        if ing.principal != viewer {
                                            return false;
                                        }
                                        let scope = scope_from_room_wire(&ing.room_id);
                                        can_view_scope(&reduced, &scope, Some(viewer.as_str()))
                                    })
                                    .map(|ing| ing.ts);
                                let since = since.or(since_default);
                                let cutoff = since.unwrap_or(0);
                                let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
                                let matching: Vec<&str> = reduced.ingests_ordered.iter().rev()
                                    .map(|id| id.as_str())
                                    .take_while(|id| reduced.ingests_by_id.get(*id).map_or(false, |ing| ing.ts > cutoff))
                                    .filter(|id| {
                                        reduced.ingests_by_id.get(*id).is_some_and(|ing| {
                                            let scope = scope_from_room_wire(&ing.room_id);
                                            can_view_scope(&reduced, &scope, Some(viewer.as_str()))
                                        })
                                    })
                                    .filter(|id| !reduced.redacted_posts.contains(*id))
                                    .collect();
                                let total = matching.len();
                                let posts: Vec<FeedPost> = matching.into_iter()
                                    .take(limit)
                                    .filter_map(|id| reduced.ingests_by_id.get(id))
                                    .map(|ing| {
                                        let scope = scope_from_room_wire(&ing.room_id);
                                        let thread_post_index = reduced
                                            .ingests_by_scope_thread
                                            .get(&(scope, ing.thread_tag.clone()))
                                            .and_then(|q| {
                                                q.iter().rev().position(|pid| pid == &ing.id).map(|i| i + 1)
                                            });
                                        FeedPost {
                                            ts: ing.ts,
                                            id: ing.id.clone(),
                                            thread: Some(ing.thread_tag.clone()),
                                            thread_post_index,
                                            body: ing.raw.clone(),
                                        }
                                    })
                                    .collect();
                                let line = line_ok(RpcResult::Feed(FeedResponse {
                                    delegate: None,
                                    since,
                                    posts,
                                    total,
                                }));
                                drop(reduced);
                                line
                            }
                        }
                    }
                }
            },
            RpcCommand::PostRedact { post_id } => {
                match rpc_post_redact(&state, &headers, post_id).await {
                    Ok(r) => line_ok(r),
                    Err((e, h)) => line_err(e, h),
                }
            }
        };
        results.push(line);
    }

    Json(RpcBatchResponse { results }).into_response()
}
