use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use rand::seq::SliceRandom;
use slug_types::*;

use crate::{
    canonical_path::{canonicalize_item, canonicalize_tag},
    dsl,
    events::{AgentBound, Event, GrantAdded, Ingest, RoomCreated, ThreadCapability, ThreadVisibility},
    identity::{parse_agent, parse_username},
    path_types::CanonicalItemUrl,
    ranking::{connected_components_from_voted_pairs, ranked_items_subset},
    reducer::{scope_from_room_wire, ReducerState, ScopeId},
    state::AppState,
};

use super::auth::verify_bearer_principal;
use super::helpers::{
    compute_connectivity_stats, is_pair_voted, item_path_for_api, now_ms, paginate_rankings,
    parse_parent_specs, pick_random_distinct, resolve_item, vote_touches_path,
};
use super::validate::{normalize_room_and_thread, validate_ingest_document};

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

fn gen_short_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..7).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

fn build_rank_response_for_content(
    content: &crate::reducer::ContentState,
    parent: Option<&str>,
    depth: usize,
    offset: usize,
    limit: Option<usize>,
    want_percent: bool,
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
                        item: item_path_for_api(r.item.as_str()),
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

    let prefixed_unranked: Vec<String> = rankings
        .unranked_items
        .into_iter()
        .map(|s| item_path_for_api(s.as_str()))
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

async fn rpc_post(
    state: &AppState,
    headers: &HeaderMap,
    room: String,
    thread_tag: String,
    delegate_opt: Option<String>,
    text: String,
    return_rank_diff: bool,
) -> Result<RpcResult, RpcErr> {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;

    let principal = verify_bearer_principal(headers, &reduced).map_err(|(_, m)| (m, None))?;

    let delegate: Option<String> = match delegate_opt {
        None => None,
        Some(ref s) if s.trim().is_empty() => None,
        Some(s) => Some(parse_agent(&s).map_err(|msg| (format!("invalid delegate format"), Some(msg)))?),
    };

    let (room_key, thread_id) = normalize_room_and_thread(&room, &thread_tag);
    let scope = scope_from_room_wire(&room_key);

    let is_private = !matches!(scope, ScopeId::Public);
    if is_private && !reduced.rooms.contains_key(&room_key) {
        drop(reduced);
        return Err(("unknown room".into(), Some(format!("room `{}` does not exist", room_key))));
    }

    let v = validate_ingest_document(&reduced, &text, &scope).map_err(|(st, m, h)| {
        let _ = st;
        (m, h)
    })?;

    if is_private {
        let mut required: HashSet<ThreadCapability> = HashSet::new();
        required.insert(ThreadCapability::View);
        for stmt in &v.doc.statements {
            match stmt {
                dsl::Stmt::Vote { .. } => { required.insert(ThreadCapability::Vote); }
                dsl::Stmt::Item { .. } => { required.insert(ThreadCapability::AddItem); }
                dsl::Stmt::Prose { .. } => { required.insert(ThreadCapability::Post); }
            }
        }
        let missing: Vec<_> = required.iter()
            .filter(|cap| !reduced.user_has_cap(&room_key, &principal, **cap))
            .collect();
        if !missing.is_empty() {
            drop(reduced);
            return Err(("insufficient capabilities for this room".into(), None));
        }
    }

    if let Some(ref d) = delegate {
        match reduced.agent_bindings.get(d) {
            Some(u) if u != &principal => {
                drop(reduced);
                return Err(("delegate already bound to another user".into(), None));
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
            if let Err(err) = state.event_log.append(&ab).await {
                return Err((format!("{err}"), None));
            }
            events_appended += 1;
            let mut reduced = reduced_arc.write().await;
            reduced.apply_event(ab);
        }
    }

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

    let pre_rankings: HashMap<CanonicalItemUrl, crate::scope_rank::ChildrenRankings> =
        if !voted_parent_scopes.is_empty() {
            let reduced = reduced_arc.read().await;
            let content = content_for_room(&reduced, &room_key);
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
        room_id: room_key.clone(),
        thread_tag: thread_id.clone(),
    });

    if let Err(err) = state.event_log.append(&ingest_event).await {
        return Err((format!("{err}"), None));
    }
    events_appended += 1;

    {
        let mut reduced = reduced_arc.write().await;
        reduced.apply_event(ingest_event);
    }

    let ranking_changes: Option<Vec<ScopeRankChanges>> = if return_rank_diff && !voted_parent_scopes.is_empty() {
        let reduced = reduced_arc.read().await;
        let content = content_for_room(&reduced, &room_key);
        let v: Vec<ScopeRankChanges> = voted_parent_scopes
            .iter()
            .filter_map(|p| {
                let before = pre_rankings.get(p)?;
                let after = crate::scope_rank::build_children_rankings(content, p);
                compute_scope_rank_changes(p.as_str(), before, &after)
            })
            .collect();
        if v.is_empty() { None } else { Some(v) }
    } else {
        None
    };

    Ok(RpcResult::PostOk {
        events_appended,
        ranking_changes,
        threads: vec![format!("#{}", thread_id)],
        next: NextMoves {
            pair: "npx slugsocial public garden pair".to_string(),
            rank: "npx slugsocial public garden rank".to_string(),
            web: format!("https://slug.social/t/{}", thread_id),
        },
    })
}

async fn rpc_check(
    state: &AppState,
    _room: String,
    text: String,
) -> Result<RpcResult, RpcErr> {
    let reduced_arc = state.reduced.clone();
    let reduced = reduced_arc.read().await;
    let thread_id = "check".to_string();
    let v = validate_ingest_document(&reduced, &text, &ScopeId::Public).map_err(|(_, m, h)| (m, h))?;
    drop(reduced);

    let delegate: Option<String> = None;
    let principal = "placeholder".to_string();
    let event = Event::Ingest(Ingest {
        ts: v.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: v.raw_text.clone(),
        principal,
        delegate,
        room_id: "public".into(),
        thread_tag: thread_id.clone(),
    });

    let mut simulated = { reduced_arc.read().await.clone() };
    simulated.apply_event(event);

    let voted_parents: Vec<CanonicalItemUrl> = {
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

    let rankings: Vec<CheckScopeRanking> = voted_parents
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
            CheckScopeRanking {
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

    Ok(RpcResult::CheckOk {
        rankings,
        threads: vec![format!("#{}", thread_id)],
        next: vec![
            "npx slugsocial public forum post <TAG> --delegate <uuid:rig:model>".to_string(),
            "npx slugsocial public forum list".to_string(),
            format!("https://slug.social/t/{}", thread_id),
        ],
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
            web: format!("https://slug.social/t/{}", tag),
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
            Some((idx, ing)) => Ok(ThreadDetailResponse {
                thread: format!("#{}", tag),
                posts: vec![PostRow {
                    id: ing.id.clone(),
                    index: idx,
                    ts: ing.ts,
                    actor: ing.principal.clone(),
                    body: ing.raw.clone(),
                    truncated: false,
                }],
                total: 1,
                offset: idx,
            }),
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
    let posts: Vec<PostRow> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(idx, ing)| {
            let (body, truncated) = if ing.raw.len() > MAX_BODY {
                (ing.raw[..MAX_BODY].to_string(), true)
            } else {
                (ing.raw.clone(), false)
            };
            PostRow {
                id: ing.id.clone(),
                index: idx,
                ts: ing.ts,
                actor: ing.principal.clone(),
                body,
                truncated,
            }
        })
        .collect();

    Ok(ThreadDetailResponse {
        thread: format!("#{}", tag),
        posts,
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

fn rpc_search(reduced: &ReducerState, q: &str, limit: usize) -> SearchResponse {
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
                path: item_path_for_api(item.as_str()),
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
            let thread = reduced
                .ingests_by_scope_thread
                .iter()
                .find(|(_, ids)| ids.contains(id))
                .map(|((scope, tag), _)| match scope {
                    ScopeId::Public => format!("#{tag}"),
                    ScopeId::Room(rid) => format!("{rid}/#{tag}"),
                })
                .unwrap_or_else(|| "#unknown".to_string());
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
    let pool: Vec<String> = {
        let reduced = reduced_arc.read().await;
        let content = content_for_room(&reduced, &room);
        let tmp = if parent_path.trim().is_empty() {
            None
        } else {
            Some(parent_path.clone())
        };
        let specs = parse_parent_specs(tmp.as_ref());
        let raw_pool: Vec<CanonicalItemUrl> = if specs.is_empty() {
            content.ranking_group.idx_to_item.clone()
        } else {
            crate::scope_rank::resolve_scope(content, &specs)
        };
        raw_pool.into_iter().map(|it| it.0).collect()
    };
    if pool.len() < 2 {
        return Err((
            format!("need at least 2 items under parent /{}", parent_path.trim()),
            Some("add items via ingest".into()),
        ));
    }
    let selected: Option<(String, String)> = {
        let mut reduced = reduced_arc.write().await;
        let content = reduced.content.entry(scope.clone()).or_default();
        let group = &mut content.ranking_group;
        if group.idx_to_item.is_empty() {
            pick_random_distinct(&pool)
        } else {
            let mut rng = rand::thread_rng();
            let idxs: Vec<usize> = pool.iter()
                .filter_map(|it| {
                    let key = CanonicalItemUrl(it.clone());
                    group.item_to_idx.get(&key).copied()
                })
                .collect();
            let ranked = ranked_items_subset(group, &idxs, 10000, 1e-8);
            let ranked_set: HashSet<String> = ranked.iter().map(|r| r.item.as_str().to_string()).collect();
            let unsorted: Vec<String> = pool.iter()
                .filter(|it| !ranked_set.contains(*it))
                .cloned()
                .collect();
            let mut pick: Option<(String, String)> = None;
            if !unsorted.is_empty() {
                if let Some(left) = unsorted.choose(&mut rng).cloned() {
                    let mut candidates: Vec<String> = if !ranked.is_empty() {
                        ranked.iter().map(|r| r.item.as_str().to_string()).collect()
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
                        pick = Some((a.to_string(), b.to_string()));
                        break;
                    }
                }
                if pick.is_none() {
                    for _ in 0..64 {
                        let (Some(a), Some(b)) = (pool.choose(&mut rng).cloned(), pool.choose(&mut rng).cloned()) else { break; };
                        if a != b && !is_pair_voted(group, &a, &b) {
                            pick = Some((a, b));
                            break;
                        }
                    }
                }
            }
            pick.or_else(|| pick_random_distinct(&pool))
        }
    };
    let Some((left, right)) = selected else {
        return Err(("need at least 2 items".into(), None));
    };
    let reduced = reduced_arc.read().await;
    let content = content_for_room(&reduced, &room);
    let left_key = CanonicalItemUrl(left.clone());
    let right_key = CanonicalItemUrl(right.clone());
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
        left: item_path_for_api(&left),
        right: item_path_for_api(&right),
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
            RpcCommand::Check { room, text } => match rpc_check(&state, room, text).await {
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
                ) {
                    Ok(r) => line_ok(RpcResult::GardenRank(r)),
                    Err((e, h)) => line_err(e, h),
                }
            }
            RpcCommand::GetGardenItem {
                room,
                item_path,
                full,
            } => {
                let reduced = state.reduced.read().await;
                let content = content_for_room(&reduced, &room);
                let item_str = canonicalize_item(&item_path);
                let item = CanonicalItemUrl(item_str.clone());
                if !content.items.contains(&item) {
                    line_err(
                        "item not found",
                        Some(format!("{} does not exist", item_path_for_api(&item_str))),
                    )
                } else {
                    const MAX_ITEM_BODY: usize = 10_000;
                    let want_full = full.unwrap_or(false);
                    let (body, truncated, body_len) = match content.item_bodies.get(&item) {
                        None => (None, false, 0),
                        Some(raw) => {
                            let char_len = raw.chars().count();
                            if !want_full && char_len > MAX_ITEM_BODY {
                                let byte_end = raw.char_indices().nth(MAX_ITEM_BODY).map(|(i, _)| i).unwrap_or(raw.len());
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
                        item: item_str,
                        body,
                        truncated,
                        body_len,
                        threads,
                    }))
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
            RpcCommand::ListForumThreads { room } => {
                let reduced = state.reduced.read().await;
                line_ok(RpcResult::ForumThreads(rpc_list_forum_threads(&reduced, &room)))
            }
            RpcCommand::RoomCreate { slug, visibility } => {
                // Scope the first read so its guard drops before any nested `read().await` / `write().await`.
                // A guard from `match verify(..., &*state.reduced.read().await)` would otherwise live for the
                // whole `match` and deadlock here (tokio::sync::RwLock is not reentrant).
                let principal = {
                    let reduced = state.reduced.read().await;
                    verify_bearer_principal(&headers, &*reduced)
                };
                match principal {
                    Err((_, m)) => line_err(m, None),
                    Ok(principal) => {
                        let slug = slug.trim().to_lowercase();
                        if slug.is_empty() || slug.len() > 64 {
                            line_err("slug must be 1-64 characters", None)
                        } else if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                            line_err("slug must be lowercase alphanumeric with hyphens", None)
                        } else {
                            match visibility.as_deref().unwrap_or("private") {
                                "private" | "public" => {
                                    let vis = if visibility.as_deref() == Some("public") {
                                        ThreadVisibility::Public
                                    } else {
                                        ThreadVisibility::Private
                                    };
                                    let short_id = loop {
                                        let id = gen_short_id();
                                        if !state.reduced.read().await.rooms.contains_key(&format!("{id}/{slug}")) {
                                            break id;
                                        }
                                    };
                                    let room_id = format!("{short_id}/{slug}");
                                    let ts = now_ms();
                                    let tc_ev = Event::RoomCreated(RoomCreated {
                                        ts,
                                        room_id: room_id.clone(),
                                        slug: slug.clone(),
                                        owner: principal.clone(),
                                        visibility: vis,
                                    });
                                    let ga_ev = Event::GrantAdded(GrantAdded {
                                        ts,
                                        room_id: room_id.clone(),
                                        username: principal.clone(),
                                        capabilities: vec![
                                            ThreadCapability::View,
                                            ThreadCapability::Post,
                                            ThreadCapability::Vote,
                                            ThreadCapability::AddItem,
                                            ThreadCapability::Manage,
                                        ],
                                        granted_by: principal.clone(),
                                    });
                                    if let Err(e) = state.event_log.append(&tc_ev).await {
                                        line_err(format!("{e}"), None)
                                    } else if let Err(e) = state.event_log.append(&ga_ev).await {
                                        line_err(format!("{e}"), None)
                                    } else {
                                        let mut r = state.reduced.write().await;
                                        r.apply_event(tc_ev);
                                        r.apply_event(ga_ev);
                                        line_ok(RpcResult::RoomCreated { room_id })
                                    }
                                }
                                other => line_err(format!("unknown visibility: {other}"), None),
                            }
                        }
                    }
                }
            }
            RpcCommand::RoomGrant {
                room,
                username,
                capability,
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
                        } else {
                            match parse_username(&username) {
                                Err(msg) => line_err("invalid username", Some(msg)),
                                Ok(target) => {
                                    let user_exists = {
                                        let reduced = state.reduced.read().await;
                                        reduced.users_by_provider.values().any(|u| u == &target)
                                    };
                                    if !user_exists {
                                        line_err(format!("user @{target} not found"), None)
                                    } else {
                                        match parse_capability(&capability) {
                                            Err(msg) => line_err(msg, None),
                                            Ok(cap) => {
                                                let ga_ev = Event::GrantAdded(GrantAdded {
                                                    ts: now_ms(),
                                                    room_id: room,
                                                    username: target,
                                                    capabilities: vec![cap],
                                                    granted_by: principal,
                                                });
                                                if let Err(e) = state.event_log.append(&ga_ev).await {
                                                    line_err(format!("{e}"), None)
                                                } else {
                                                    let mut r = state.reduced.write().await;
                                                    r.apply_event(ga_ev);
                                                    line_ok(RpcResult::GrantOk {})
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            RpcCommand::GetGlobalRank {
                room,
                limit,
                offset,
                percent,
            } => {
                const DEFAULT_LIMIT: usize = 50;
                const MAX_LIMIT: usize = 500;
                let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
                let offset = offset.unwrap_or(0);
                let want_percent = percent.unwrap_or(false);
                let mut reduced = state.reduced.write().await;
                let content = reduced.content.entry(scope_from_room_wire(&room)).or_default();
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
                        ranked.push(RankRow { item: item_path_for_api(r.item.as_str()), score: r.score, percent: pct });
                    }
                }

                let ranked_total = ranked.len();
                let mut unranked: Vec<String> = content
                    .items
                    .iter()
                    .filter(|it| !group.item_to_idx.contains_key(*it))
                    .map(|it| it.as_str().to_string())
                    .collect();
                unranked.sort();
                let unranked_total = unranked.len();

                let page: Vec<RankRow> = ranked
                    .into_iter()
                    .chain(unranked.into_iter().map(|it| RankRow {
                        item: item_path_for_api(&it),
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
            RpcCommand::GetPair { room, parent_path } => match rpc_get_pair(&state, room, parent_path).await {
                Ok(r) => line_ok(r),
                Err((e, h)) => line_err(e, h),
            },
            RpcCommand::GetMatchup {
                room,
                item_path,
                limit,
            } => {
                let reduced = state.reduced.read().await;
                let content = content_for_room(&reduced, &room);
                let item_str = canonicalize_item(&item_path);
                let item = CanonicalItemUrl(item_str.clone());
                let limit = limit.unwrap_or(50).clamp(1, 200);
                if !content.items.contains(&item) {
                    line_err(
                        "item not found",
                        Some(format!("{} does not exist", item_path_for_api(&item_str))),
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
                                    a: v.a.as_str().to_string(),
                                    b: v.b.as_str().to_string(),
                                    ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                                    actor: Some(v.principal.clone()),
                                    body: v.body.clone(),
                                    thread: Some(v.thread_tag.clone()),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    line_ok(RpcResult::Matchup(MatchupResponse {
                        item: item_path_for_api(&item_str),
                        votes,
                    }))
                }
            },
            RpcCommand::GetRankHistory { room, item_path } => {
                let reduced = state.reduced.read().await;
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
                                            a: item_path_for_api(&a),
                                            b: item_path_for_api(&b),
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
                    item: item_path_for_api(&item_str),
                    history,
                }))
            },
            RpcCommand::GetLeaves { room } => {
                let reduced = state.reduced.read().await;
                let content = content_for_room(&reduced, &room);
                let parents: HashSet<&str> = content.item_children.keys().map(|s| s.as_str()).collect();
                let mut paths: Vec<String> = content
                    .items
                    .iter()
                    .filter(|p| !parents.contains(p.as_str()))
                    .map(|p| p.as_str().to_string())
                    .collect();
                paths.sort();
                line_ok(RpcResult::Leaves(LeavesResponse { paths }))
            },
            RpcCommand::GetPaths { room } => {
                let reduced = state.reduced.read().await;
                let content = content_for_room(&reduced, &room);
                let out: Vec<PathSummary> = content
                    .item_children
                    .get(&CanonicalItemUrl::ontology_root())
                    .map(|roots| {
                        let mut v: Vec<PathSummary> = roots.iter()
                            .map(|path| {
                                let children = content.item_children.get(path.as_str()).map(|s| s.len()).unwrap_or(0);
                                PathSummary {
                                    path: format!("~/{}", path),
                                    children,
                                    web: format!("https://slug.social/~/{}", path),
                                }
                            }).collect();
                        v.sort_by(|a, b| a.path.cmp(&b.path));
                        v
                    })
                    .unwrap_or_default();
                line_ok(RpcResult::Paths(PathsResponse { paths: out }))
            },
            RpcCommand::GetRecentVotes {
                room,
                parent,
                limit,
            } => {
                let reduced = state.reduced.read().await;
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
                        a: v.a.as_str().to_string(),
                        b: v.b.as_str().to_string(),
                        ratio: format!("{}:{}", v.ratio_left, v.ratio_right),
                        actor: Some(v.principal.clone()),
                        body: v.body.clone(),
                        thread: Some(v.thread_tag.clone()),
                    }).collect();
                line_ok(RpcResult::RecentVotes(RecentVotesResponse { votes: out }))
            },
            RpcCommand::Search { query } => {
                let reduced = state.reduced.read().await;
                let limit = 50usize.min(200);
                line_ok(RpcResult::Search(rpc_search(&reduced, &query, limit)))
            },
            RpcCommand::GetFeed {
                actor,
                since,
                limit,
            } => {
                const DEFAULT_LIMIT: usize = 50;
                const MAX_LIMIT: usize = 200;
                match parse_username(&actor) {
                    Err(msg) => line_err("invalid actor", Some(msg)),
                    Ok(actor_stored) => {
                        let reduced = state.reduced.read().await;
                        let since = since.or_else(|| reduced.actor_last_post_ts.get(&actor_stored).copied());
                        let cutoff = since.unwrap_or(0);
                        let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
                        let matching: Vec<&str> = reduced.ingests_ordered.iter().rev()
                            .map(|id| id.as_str())
                            .take_while(|id| reduced.ingests_by_id.get(*id).map_or(false, |ing| ing.ts > cutoff))
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
                            actor: actor_stored,
                            since,
                            posts,
                            total,
                        }))
                    }
                }
            },
        };
        results.push(line);
    }

    Json(RpcBatchResponse { results }).into_response()
}
