//! Serialized writer: all event-log appends and reducer mutations go through one task.

use std::collections::{HashMap, HashSet};

use slug_types::paths::ForumThreadUrl;
use tokio::sync::mpsc;

use crate::{
    dsl,
    events::{AgentBound, Event, GrantAdded, Ingest, PostRedacted, RoomDeleted, ThreadGraduated, UserRegistered},
    html::JsBuilder,
    identity::parse_agent,
    path_types::ItemId,
    reducer::{scope_from_room_wire, ReducerState, ScopeId},
    state::AppState,
    write_cmd::WriteCmd,
};

use super::auth::{issue_token_for_user, verify_token};
use super::helpers::{now_ms, resolve_item};
use super::validate::{normalize_room_and_thread, validate_ingest_document};
use slug_types::{room_route_segment, RpcResult, ROOM_SHORT_ID_LEN};

fn gen_short_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..ROOM_SHORT_ID_LEN)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn parse_capability(s: &str) -> Result<crate::events::ThreadCapability, String> {
    use crate::events::ThreadCapability;
    match s {
        "view" => Ok(ThreadCapability::View),
        "post" => Ok(ThreadCapability::Post),
        "vote" => Ok(ThreadCapability::Vote),
        "add_item" => Ok(ThreadCapability::AddItem),
        "manage" => Ok(ThreadCapability::Manage),
        other => Err(format!("unknown capability: {other}")),
    }
}

fn empty_content() -> &'static crate::reducer::ContentState {
    use std::sync::OnceLock;
    static E: OnceLock<crate::reducer::ContentState> = OnceLock::new();
    E.get_or_init(Default::default)
}

fn content_for_room<'a>(reduced: &'a ReducerState, room: &str) -> &'a crate::reducer::ContentState {
    let scope = scope_from_room_wire(room);
    reduced.content.get(&scope).unwrap_or(empty_content())
}

/// Push live updates to web subscribers after a thread changed (new post, redaction,
/// graduation). The `#thread-feed-region` morphs are **page-scoped**: each one is
/// guarded by the viewer's current `?offset` page, so viewers reading an older page
/// are never yanked to the latest posts. `changed_post_index` is the chronological
/// index of a changed existing post (e.g. a redaction) so its page refreshes too.
async fn broadcast_web_refresh(
    state: &AppState,
    room_key: &str,
    thread_id: &str,
    changed_post_index: Option<usize>,
) {
    let feed_id = if room_key == "public" {
        "thread-feed"
    } else {
        "room-thread-feed"
    };
    let thread_url = if room_key == "public" {
        format!("/t/{thread_id}")
    } else if let Some(seg) = room_route_segment(room_key) {
        format!("/r/{seg}/t/{thread_id}")
    } else {
        format!("/t/{thread_id}")
    };

    let feed_markup = if room_key == "public" {
        crate::html::thread_feed_html(state).await
    } else {
        crate::html::thread_feed_html_for_room(state, room_key).await
    };

    let morphs: crate::html::ThreadRegionPageMorphs =
        crate::html::thread_region_page_morphs(state, Some(room_key), thread_id, None, changed_post_index)
            .await;

    // Two SSE payloads: the bump-list morph must not ship private HTML to subscribers who only
    // matched `/` (public) or lack room access — see [`crate::api::stream::get_html_stream`].
    let feed_builder = JsBuilder::new().morph_selector(&format!("#{feed_id}"), feed_markup);
    let feed_builder = feed_builder.qs("#new-thread-compose form").reset();
    let feed_js = feed_builder.build();

    let latest_offset = morphs.latest_offset;
    let thread_builder = JsBuilder::new().if_current_path_matches(&thread_url, |mut builder| {
        for page in morphs.pages {
            let morph = |b: JsBuilder| b.morph_selector("#thread-feed-region", page.markup);
            builder = if page.offset == latest_offset {
                builder.if_page_offset_at_least(page.offset, morph)
            } else {
                builder.if_page_offset_equals(page.offset, morph)
            };
        }
        builder
    });
    let thread_js = thread_builder.build();

    let audience = if room_key == "public" {
        crate::state::JsSnippetAudience::Public
    } else {
        crate::state::JsSnippetAudience::RoomViewers(room_key.to_string())
    };

    let feed_prefixes = if room_key == "public" {
        vec!["/".to_string(), thread_url.clone()]
    } else if let Some(seg) = room_route_segment(room_key) {
        vec![format!("/r/{seg}"), thread_url.clone()]
    } else {
        vec!["/".to_string(), thread_url.clone()]
    };

    let _ = state.js_tx.send(crate::state::JsSnippet {
        code: feed_js,
        path_prefixes: feed_prefixes,
        audience: audience.clone(),
    });
    let _ = state.js_tx.send(crate::state::JsSnippet {
        code: thread_js,
        path_prefixes: vec![thread_url.clone()],
        audience,
    });
}

fn compute_scope_rank_changes(
    parent: &ItemId,
    before: &crate::scope_rank::ChildrenRankings,
    after: &crate::scope_rank::ChildrenRankings,
    room_wire: &str,
) -> Option<slug_types::ScopeRankChanges> {
    use slug_types::{RankChange, RankPosition, ScopeRankChanges};
    use std::collections::BTreeSet;
    fn build_positions(
        rankings: &crate::scope_rank::ChildrenRankings,
    ) -> HashMap<ItemId, Option<RankPosition>> {
        let mut map = HashMap::new();
        for comp in &rankings.component_rankings {
            let total = comp.ranked.len();
            for (i, item) in comp.ranked.iter().enumerate() {
                map.insert(
                    item.item.clone(),
                    Some(RankPosition {
                        rank: i + 1,
                        of: total,
                    }),
                );
            }
        }
        for item in &rankings.unranked_items {
            map.insert(item.clone(), None);
        }
        map
    }

    let before_pos = build_positions(before);
    let after_pos = build_positions(after);

    let all_items: BTreeSet<ItemId> = before_pos
        .keys()
        .cloned()
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
                item: slug_types::paths::GardenItemUrl::from_stored(&item, room_wire),
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
        parent: slug_types::paths::GardenItemUrl::from_stored(parent, room_wire).into_inner(),
        changes,
    })
}

async fn redeem_invite_grant(
    state: &AppState,
    invite_token: &str,
    grantee_username: &str,
) -> Result<(), String> {
    let now = now_ms();
    let ga = {
        let mut invites = state.invites.write().await;
        let Some(inv) = invites.get_mut(invite_token) else {
            return Err("invite not found".into());
        };
        if now > inv.expires_at_ms {
            invites.remove(invite_token);
            return Err("invite expired".into());
        }
        if inv.current_uses >= inv.max_uses {
            return Err("invite exhausted".into());
        }
        inv.current_uses += 1;
        Event::GrantAdded(GrantAdded {
            ts: now,
            room_id: inv.room_id.clone(),
            username: grantee_username.to_string(),
            capabilities: inv.capabilities.clone(),
            granted_by: inv.inviter.clone(),
        })
    };

    match state.event_log.append(&ga).await {
        Ok(()) => {
            let mut reduced = state.reduced.write().await;
            reduced.apply_event(ga);
            drop(reduced);
            let mut invites = state.invites.write().await;
            if let Some(inv) = invites.get(invite_token) {
                if inv.current_uses >= inv.max_uses {
                    invites.remove(invite_token);
                }
            }
            Ok(())
        }
        Err(e) => {
            let mut invites = state.invites.write().await;
            if let Some(inv) = invites.get_mut(invite_token) {
                inv.current_uses = inv.current_uses.saturating_sub(1);
            }
            Err(format!("{e}"))
        }
    }
}

pub async fn writer_actor(mut rx: mpsc::Receiver<WriteCmd>, state: AppState) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            WriteCmd::Post {
                room,
                thread_tag,
                delegate_opt,
                text,
                return_rank_diff,
                bearer,
                reply,
            } => {
                let out = async {
                    let mut reduced = state.reduced.write().await;

                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;

                    let delegate: Option<String> = match delegate_opt {
                        None => None,
                        Some(ref s) if s.trim().is_empty() => None,
                        Some(s) => Some(
                            parse_agent(&s)
                                .map_err(|msg| ("invalid delegate format".to_string(), Some(msg)))?,
                        ),
                    };

                    let (room_key, thread_id) = normalize_room_and_thread(&room, &thread_tag)
                        .map_err(|msg| {
                            (msg, Some("thread tags are one /t/:tag segment (no '/')".into()))
                        })?;
                    let scope = scope_from_room_wire(&room_key);
                    let is_private = !matches!(scope, ScopeId::Public);
                    if is_private && !reduced.rooms.contains(&room_key) {
                        return Err((
                            "unknown room".into(),
                            Some(format!("room `{room_key}` does not exist")),
                        ));
                    }

                    if is_private && reduced.is_thread_graduated(&room_key, &thread_id) {
                        return Err((
                            "thread graduated to public".into(),
                            Some(format!(
                                "this private thread was published to public #{}; post there instead",
                                crate::canonical_path::canonicalize_tag(&thread_id)
                            )),
                        ));
                    }

                    let v = validate_ingest_document(&reduced, &text, &scope).map_err(
                        |(st, m, h)| {
                            let _ = st;
                            (m, h)
                        },
                    )?;

                    if is_private {
                        use crate::events::ThreadCapability;
                        let mut required: HashSet<ThreadCapability> = HashSet::new();
                        required.insert(ThreadCapability::View);
                        for stmt in &v.doc.statements {
                            match stmt {
                                dsl::Stmt::Vote { .. } | dsl::Stmt::Containment { sugar: false, .. } => {
                                    required.insert(ThreadCapability::Vote);
                                }
                                dsl::Stmt::Item { .. } => {
                                    required.insert(ThreadCapability::AddItem);
                                }
                                dsl::Stmt::Prose { .. }
                                | dsl::Stmt::Aspect { .. }
                                | dsl::Stmt::Containment { sugar: true, .. } => {
                                    required.insert(ThreadCapability::Post);
                                }
                            }
                        }
                        let missing: Vec<_> = required
                            .iter()
                            .filter(|cap| !reduced.user_has_cap(&room_key, &principal, **cap))
                            .collect();
                        if !missing.is_empty() {
                            return Err(("insufficient capabilities for this room".into(), None));
                        }
                    }

                    if let Some(ref d) = delegate {
                        match reduced.agent_bindings.get(d) {
                            Some(u) if u != &principal => {
                                return Err((
                                    "delegate already bound to another user".into(),
                                    None,
                                ));
                            }
                            _ => {}
                        }
                    }

                    let need_agent_bind = delegate
                        .as_ref()
                        .map(|d| !reduced.agent_bindings.contains_key(d))
                        .unwrap_or(false);

                    let voted_parent_scopes: Vec<ItemId> = {
                        let mut parents: HashSet<ItemId> = HashSet::new();
                        for s in &v.doc.statements {
                            if let dsl::Stmt::Vote {
                                item1,
                                item2,
                                aspect: None,
                                ..
                            } = s
                            {
                                if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                                    let a = a.ontology_leaf();
                                    let b = b.ontology_leaf();
                                    let content = content_for_room(&reduced, &room_key);
                                    for p in content.shared_scopes(&a, &b) {
                                        parents.insert(p);
                                    }
                                }
                            }
                        }
                        for s in &v.doc.statements {
                            if let dsl::Stmt::Containment {
                                parent,
                                sugar: true,
                                border: false,
                                ..
                            } = s
                            {
                                if let Ok(p) = resolve_item(parent) {
                                    parents.insert(p.ontology_leaf());
                                }
                            }
                        }
                        let mut out: Vec<ItemId> = parents.into_iter().collect();
                        out.sort();
                        out
                    };

                    let pre_rankings: HashMap<ItemId, crate::scope_rank::ChildrenRankings> =
                        if !voted_parent_scopes.is_empty() {
                            let content = content_for_room(&reduced, &room_key);
                            voted_parent_scopes
                                .iter()
                                .map(|p| {
                                    (
                                        p.clone(),
                                        crate::scope_rank::build_children_rankings(content, p),
                                    )
                                })
                                .collect()
                        } else {
                            HashMap::new()
                        };

                    let mut events_appended: usize = 0;

                    if need_agent_bind {
                        if let Some(agent_id) = delegate.clone() {
                            let ab = Event::AgentBound(AgentBound {
                                ts: now_ms(),
                                agent: agent_id,
                                username: principal.clone(),
                            });
                            state
                                .event_log
                                .append(&ab)
                                .await
                                .map_err(|e| (format!("{e}"), None))?;
                            events_appended += 1;
                            reduced.apply_event(ab);
                        }
                    }

                    let new_post_id = uuid::Uuid::new_v4().to_string();
                    let ingest_event = Event::Ingest(Ingest {
                        ts: v.ts,
                        id: new_post_id.clone(),
                        raw: v.raw_text.clone(),
                        principal: principal.clone(),
                        delegate: delegate.clone(),
                        room_id: room_key.clone(),
                        thread_tag: thread_id.clone(),
                    });

                    state
                        .event_log
                        .append(&ingest_event)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    events_appended += 1;
                    reduced.apply_event(ingest_event);
                    drop(reduced);

                    use crate::canonical_path::canonicalize_tag;
                    let thread_tag_canon = canonicalize_tag(&thread_id);
                    broadcast_web_refresh(&state, &room_key, &thread_tag_canon, None).await;

                    let ranking_changes: Option<Vec<slug_types::ScopeRankChanges>> =
                        if return_rank_diff && !voted_parent_scopes.is_empty() {
                            let reduced = state.reduced.read().await;
                            let content = content_for_room(&reduced, &room_key);
                            let v: Vec<slug_types::ScopeRankChanges> = voted_parent_scopes
                                .iter()
                                .filter_map(|p| {
                                    let before = pre_rankings.get(p)?;
                                    let after =
                                        crate::scope_rank::build_children_rankings(content, p);
                                    compute_scope_rank_changes(p, before, &after, &room_key)
                                })
                                .collect();
                            if v.is_empty() {
                                None
                            } else {
                                Some(v)
                            }
                        } else {
                            None
                        };

                    let (pair_hint, rank_hint, web_url) = if room_key == "public" {
                        (
                            "npx slugsocial public garden pair".to_string(),
                            "npx slugsocial public garden rank".to_string(),
                            ForumThreadUrl::from_room_tag("public", &thread_id),
                        )
                    } else {
                        (
                            format!("npx slugsocial private {room_key} garden pair"),
                            format!("npx slugsocial private {room_key} garden rank"),
                            ForumThreadUrl::from_room_tag(&room_key, &thread_id),
                        )
                    };

                    let post_index = {
                        let reduced = state.reduced.read().await;
                        reduced.try_thread_post_index_chronological(
                            &scope_from_room_wire(&room_key),
                            &thread_tag_canon,
                            &new_post_id,
                        )
                    };

                    Ok(RpcResult::PostOk {
                        events_appended,
                        post_id: Some(new_post_id),
                        post_index,
                        ranking_changes,
                        threads: vec![format!("#{}", thread_id)],
                        next: slug_types::NextMoves {
                            pair: pair_hint,
                            rank: rank_hint,
                            web: web_url,
                        },
                    })
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::SystemIngest {
                room,
                thread_tag,
                text,
                principal,
                reply,
            } => {
                let out = async {
                    let (room_key, thread_id) = normalize_room_and_thread(&room, &thread_tag)
                        .map_err(|msg| (msg, Some("system thread tags must not contain '/'".into())))?;
                    let scope = scope_from_room_wire(&room_key);
                    let is_private = !matches!(scope, ScopeId::Public);
                    let mut reduced = state.reduced.write().await;
                    if is_private && !reduced.rooms.contains(&room_key) {
                        return Err((
                            "unknown room".into(),
                            Some(format!("room `{room_key}` does not exist")),
                        ));
                    }

                    let v = validate_ingest_document(&reduced, &text, &scope).map_err(
                        |(st, m, h)| {
                            let _ = st;
                            (m, h)
                        },
                    )?;

                    let new_post_id = uuid::Uuid::new_v4().to_string();
                    let ingest_event = Event::Ingest(Ingest {
                        ts: v.ts,
                        id: new_post_id.clone(),
                        raw: v.raw_text.clone(),
                        principal: principal.clone(),
                        delegate: None,
                        room_id: room_key.clone(),
                        thread_tag: thread_id.clone(),
                    });

                    state
                        .event_log
                        .append(&ingest_event)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(ingest_event);
                    drop(reduced);

                    use crate::canonical_path::canonicalize_tag;
                    let thread_tag_canon = canonicalize_tag(&thread_id);
                    broadcast_web_refresh(&state, &room_key, &thread_tag_canon, None).await;
                    let post_index = {
                        let reduced = state.reduced.read().await;
                        reduced.try_thread_post_index_chronological(
                            &scope_from_room_wire(&room_key),
                            &thread_tag_canon,
                            &new_post_id,
                        )
                    };

                    Ok(RpcResult::PostOk {
                        events_appended: 1,
                        post_id: Some(new_post_id),
                        post_index,
                        ranking_changes: None,
                        threads: vec![format!("#{}", thread_id)],
                        next: slug_types::NextMoves {
                            pair: "npx slugsocial public garden pair".to_string(),
                            rank: "npx slugsocial public garden rank".to_string(),
                            web: ForumThreadUrl::from_room_tag(&room_key, &thread_id),
                        },
                    })
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::SystemRedact {
                post_id,
                principal,
                reply,
            } => {
                let out = async {
                    let mut reduced = state.reduced.write().await;
                    let post_id = post_id.trim().to_string();
                    let principal = principal.trim().to_string();
                    if principal.is_empty() {
                        return Err(("missing principal".into(), None));
                    }
                    let Some(ing) = reduced.ingests_by_id.get(&post_id).cloned() else {
                        return Err(("post not found".into(), None));
                    };
                    if ing.principal != principal {
                        return Err(("not your post".into(), None));
                    }
                    if reduced.redacted_posts.contains(&post_id) {
                        return Err(("already redacted".into(), None));
                    }
                    let room_key = ing.room_id.trim().to_string();
                    let ev = Event::PostRedacted(PostRedacted {
                        ts: now_ms(),
                        post_id: post_id.clone(),
                        principal: principal.clone(),
                    });
                    state
                        .event_log
                        .append(&ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(ev);
                    let changed_idx = reduced.try_thread_post_index_chronological(
                        &scope_from_room_wire(&room_key),
                        &ing.thread_tag,
                        &post_id,
                    );
                    drop(reduced);

                    use crate::canonical_path::canonicalize_tag;
                    let thread_tag = canonicalize_tag(&ing.thread_tag);
                    broadcast_web_refresh(&state, &room_key, &thread_tag, changed_idx).await;
                    Ok(RpcResult::RedactPostOk {})
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::Redact {
                post_id,
                bearer,
                reply,
            } => {
                let out = async {
                    let mut reduced = state.reduced.write().await;
                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;
                    let post_id = post_id.trim().to_string();
                    let Some(ing) = reduced.ingests_by_id.get(&post_id).cloned() else {
                        return Err(("post not found".into(), None));
                    };
                    if ing.principal != principal {
                        return Err(("not your post".into(), None));
                    }
                    if reduced.redacted_posts.contains(&post_id) {
                        return Err(("already redacted".into(), None));
                    }
                    let room_key = ing.room_id.trim().to_string();
                    let scope = scope_from_room_wire(&room_key);
                    if matches!(scope, ScopeId::Room(_))
                        && (!reduced.rooms.contains(&room_key)
                            || !reduced.user_has_cap(
                                &room_key,
                                &principal,
                                crate::events::ThreadCapability::View,
                            ))
                    {
                        return Err(("room not found".into(), None));
                    }

                    let ev = Event::PostRedacted(PostRedacted {
                        ts: now_ms(),
                        post_id: post_id.clone(),
                        principal: principal.clone(),
                    });
                    state
                        .event_log
                        .append(&ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(ev);
                    let changed_idx = reduced.try_thread_post_index_chronological(
                        &scope,
                        &ing.thread_tag,
                        &post_id,
                    );
                    drop(reduced);

                    use crate::canonical_path::canonicalize_tag;
                    let thread_tag = canonicalize_tag(&ing.thread_tag);
                    broadcast_web_refresh(&state, &room_key, &thread_tag, changed_idx).await;
                    Ok(RpcResult::RedactPostOk {})
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::RoomCreate {
                slug,
                bearer,
                reply,
            } => {
                let out = async {
                    let mut reduced = state.reduced.write().await;
                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;
                    let slug = slug.trim().to_lowercase();
                    if slug.is_empty() || slug.len() > 64 {
                        return Err(("slug must be 1-64 characters".into(), None));
                    }
                    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                        return Err((
                            "slug must be lowercase alphanumeric with hyphens".into(),
                            None,
                        ));
                    }
                    let short_id = loop {
                        let id = gen_short_id();
                        if !reduced.rooms.contains(&format!("{id}/{slug}")) {
                            break id;
                        }
                    };
                    let room_id = format!("{short_id}/{slug}");
                    let ts = now_ms();
                    use crate::events::{GrantAdded, RoomCreated, ThreadCapability};
                    let tc_ev = Event::RoomCreated(RoomCreated {
                        ts,
                        room_id: room_id.clone(),
                        slug: slug.clone(),
                        owner: principal.clone(),
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
                    state
                        .event_log
                        .append(&tc_ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    state
                        .event_log
                        .append(&ga_ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(tc_ev);
                    reduced.apply_event(ga_ev);
                    Ok(RpcResult::RoomCreated { room_id })
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::RoomDelete {
                room,
                bearer,
                reply,
            } => {
                let out = async {
                    use crate::events::ThreadCapability;

                    let mut reduced = state.reduced.write().await;
                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;
                    let room = room.trim().to_string();
                    if !reduced.rooms.contains(&room) {
                        return Err(("unknown room".into(), None));
                    }
                    if !reduced.user_has_cap(&room, &principal, ThreadCapability::Manage) {
                        return Err(("requires Manage capability".into(), None));
                    }
                    let ev = Event::RoomDeleted(RoomDeleted {
                        ts: now_ms(),
                        room_id: room.clone(),
                        deleted_by: principal,
                    });
                    state
                        .event_log
                        .append(&ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(ev);
                    drop(reduced);

                    {
                        let mut inv = state.invites.write().await;
                        inv.retain(|_, v| v.room_id != room);
                    }

                    Ok(RpcResult::RoomDeletedOk {})
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::ThreadGraduate {
                room,
                thread_tag,
                bearer,
                reply,
            } => {
                let out = async {
                    use crate::canonical_path::canonicalize_tag;
                    use crate::events::ThreadCapability;

                    let mut reduced = state.reduced.write().await;
                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;
                    let room = room.trim().to_string();
                    let thread_tag = canonicalize_tag(&thread_tag);
                    if !reduced.rooms.contains(&room) {
                        return Err(("unknown room".into(), None));
                    }
                    if !reduced.user_has_cap(&room, &principal, ThreadCapability::Manage) {
                        return Err(("requires Manage capability".into(), None));
                    }
                    if reduced.is_thread_graduated(&room, &thread_tag) {
                        return Err(("thread already graduated".into(), None));
                    }

                    let scope = ScopeId::Room(room.clone());
                    let source_ids: Vec<String> = reduced
                        .ingests_by_scope_thread
                        .get(&(scope.clone(), thread_tag.clone()))
                        .map(|q| q.iter().rev().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|id| !reduced.redacted_posts.contains(id))
                        .collect();

                    if source_ids.is_empty() {
                        return Err((
                            "thread has no posts to graduate".into(),
                            Some("post at least once in this private thread first".into()),
                        ));
                    }

                    let mut posts_copied: u32 = 0;
                    for source_id in &source_ids {
                        let Some(source) = reduced.ingests_by_id.get(source_id).cloned() else {
                            continue;
                        };
                        let v = validate_ingest_document(
                            &reduced,
                            &source.raw,
                            &ScopeId::Public,
                        )
                        .map_err(|(_, m, h)| (m, h))?;

                        let new_post_id = uuid::Uuid::new_v4().to_string();
                        let ingest_event = Event::Ingest(Ingest {
                            ts: v.ts,
                            id: new_post_id,
                            raw: v.raw_text,
                            principal: source.principal,
                            delegate: source.delegate,
                            room_id: "public".to_string(),
                            thread_tag: thread_tag.clone(),
                        });
                        state
                            .event_log
                            .append(&ingest_event)
                            .await
                            .map_err(|e| (format!("{e}"), None))?;
                        reduced.apply_event(ingest_event);
                        posts_copied += 1;
                    }

                    if posts_copied == 0 {
                        return Err(("thread has no posts to graduate".into(), None));
                    }

                    let grad_ev = Event::ThreadGraduated(ThreadGraduated {
                        ts: now_ms(),
                        source_room_id: room.clone(),
                        thread_tag: thread_tag.clone(),
                        graduated_by: principal,
                        posts_copied,
                    });
                    state
                        .event_log
                        .append(&grad_ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(grad_ev);
                    drop(reduced);

                    broadcast_web_refresh(&state, "public", &thread_tag, None).await;
                    broadcast_web_refresh(&state, &room, &thread_tag, None).await;

                    Ok(RpcResult::ThreadGraduatedOk {
                        thread_tag: thread_tag.clone(),
                        posts_copied,
                        web: ForumThreadUrl::from_room_tag("public", &thread_tag).into_inner(),
                    })
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::Grant {
                room,
                username,
                capabilities,
                bearer,
                reply,
            } => {
                let out = async {
                    use crate::events::{GrantAdded, ThreadCapability};
                    use crate::identity::parse_username;

                    let mut reduced = state.reduced.write().await;
                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;
                    if !reduced.user_has_cap(&room, &principal, ThreadCapability::Manage) {
                        return Err(("requires Manage capability".into(), None));
                    }
                    if capabilities.is_empty() {
                        return Err(("capabilities must not be empty".into(), None));
                    }
                    let target = parse_username(&username)
                        .map_err(|msg| ("invalid username".into(), Some(msg)))?;
                    if !reduced.users_by_provider.values().any(|u| u == &target) {
                        return Err((format!("user @{target} not found"), None));
                    }
                    let caps: Vec<crate::events::ThreadCapability> = capabilities
                        .iter()
                        .map(|c| parse_capability(c.trim()))
                        .collect::<Result<_, _>>()
                        .map_err(|msg| (msg, None))?;
                    let ga_ev = Event::GrantAdded(GrantAdded {
                        ts: now_ms(),
                        room_id: room,
                        username: target,
                        capabilities: caps,
                        granted_by: principal,
                    });
                    state
                        .event_log
                        .append(&ga_ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(ga_ev);
                    Ok(RpcResult::GrantOk {})
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::Revoke {
                room,
                username,
                capability,
                bearer,
                reply,
            } => {
                let out = async {
                    use crate::events::{GrantRevoked, ThreadCapability};
                    use crate::identity::parse_username;

                    let mut reduced = state.reduced.write().await;
                    let principal = verify_token(&reduced, &bearer).map_err(|(_, m)| (m, None))?;
                    if !reduced.user_has_cap(&room, &principal, ThreadCapability::Manage) {
                        return Err(("requires Manage capability".into(), None));
                    }
                    let target = parse_username(&username)
                        .map_err(|msg| ("invalid username".into(), Some(msg)))?;
                    if !reduced.users_by_provider.values().any(|u| u == &target) {
                        return Err((format!("user @{target} not found"), None));
                    }
                    let cap = parse_capability(capability.trim()).map_err(|msg| (msg, None))?;
                    let gr_ev = Event::GrantRevoked(GrantRevoked {
                        ts: now_ms(),
                        room_id: room,
                        username: target,
                        capabilities: vec![cap],
                        revoked_by: principal,
                    });
                    state
                        .event_log
                        .append(&gr_ev)
                        .await
                        .map_err(|e| (format!("{e}"), None))?;
                    reduced.apply_event(gr_ev);
                    Ok(RpcResult::GrantOk {})
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::Register {
                username,
                provider,
                provider_id,
                redeem_invite,
                reply,
            } => {
                let out = async {
                    let mut reduced = state.reduced.write().await;
                    let provider_key = (provider.to_lowercase(), provider_id.clone());
                    if reduced.users_by_provider.contains_key(&provider_key) {
                        return Err("provider already registered".into());
                    }
                    if reduced.users_by_provider.values().any(|u| u == &username) {
                        return Err("that username is taken — try another".into());
                    }

                    let ur = Event::UserRegistered(UserRegistered {
                        ts: now_ms(),
                        username: username.clone(),
                        provider: provider.to_lowercase(),
                        provider_id: provider_id.clone(),
                    });
                    let (bearer, ti) = issue_token_for_user(&username);
                    let ti_ev = Event::TokenIssued(ti);

                    state
                        .event_log
                        .append(&ur)
                        .await
                        .map_err(|e| format!("{e}"))?;
                    state
                        .event_log
                        .append(&ti_ev)
                        .await
                        .map_err(|e| format!("{e}"))?;
                    reduced.apply_event(ur);
                    reduced.apply_event(ti_ev.clone());
                    drop(reduced);

                    if let Some(tok) = redeem_invite {
                        if let Err(e) = redeem_invite_grant(&state, &tok, &username).await {
                            tracing::warn!(error = %e, "invite redemption skipped after registration");
                        }
                    }

                    Ok(bearer)
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::OAuthTokenIssue {
                token_event,
                redeem_invite,
                reply,
            } => {
                let grantee = match &token_event {
                    Event::TokenIssued(t) => t.username.clone(),
                    _ => {
                        let _ = reply.send(Err("internal: expected TokenIssued".into()));
                        continue;
                    }
                };
                let out = async {
                    let mut reduced = state.reduced.write().await;
                    state
                        .event_log
                        .append(&token_event)
                        .await
                        .map_err(|e| format!("{e}"))?;
                    reduced.apply_event(token_event);
                    drop(reduced);

                    if let Some(tok) = redeem_invite {
                        if let Err(e) = redeem_invite_grant(&state, &tok, &grantee).await {
                            tracing::warn!(error = %e, "invite redemption skipped after oauth");
                        }
                    }
                    Ok(())
                }
                .await;
                let _ = reply.send(out);
            }

            WriteCmd::RedeemInvite {
                token,
                grantee_username,
                reply,
            } => {
                let out = redeem_invite_grant(&state, &token, &grantee_username).await;
                let _ = reply.send(out);
            }
        }
    }
}
