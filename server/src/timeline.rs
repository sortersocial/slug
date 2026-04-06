//! Room admin lines merged into forum thread views.

use crate::{
    canonical_path::canonicalize_tag,
    reducer::{ReducerState, RoomTimelineEntry, RoomTimelineKind},
};

fn cap_label(c: crate::events::ThreadCapability) -> &'static str {
    use crate::events::ThreadCapability::*;
    match c {
        View => "view",
        Post => "post",
        Vote => "vote",
        AddItem => "add_item",
        Manage => "manage",
    }
}

fn caps_list(caps: &[crate::events::ThreadCapability]) -> String {
    let mut v: Vec<_> = caps.iter().map(|c| cap_label(*c)).collect();
    v.sort();
    v.join(", ")
}

/// Human-readable system line for the thread feed.
pub fn format_room_timeline_entry(e: &RoomTimelineEntry) -> String {
    match &e.kind {
        RoomTimelineKind::RoomCreated { owner, slug } => {
            format!("@{owner} created room #{slug}")
        }
        RoomTimelineKind::GrantAdded {
            username,
            granted_by,
            capabilities,
        } => {
            format!(
                "@{granted_by} granted @{} {}",
                username,
                caps_list(capabilities)
            )
        }
        RoomTimelineKind::GrantRevoked {
            username,
            revoked_by,
            capabilities,
        } => {
            format!(
                "@{revoked_by} revoked @{} {}",
                username,
                caps_list(capabilities)
            )
        }
    }
}

#[derive(Clone, Debug)]
pub enum MergedThreadRow {
    System { ts: i64, text: String },
    Post {
        index: usize,
        id: String,
        ts: i64,
        principal: String,
        raw: String,
    },
}

/// Merge room admin lines with thread ingests for one room + tag. Oldest first.
/// `actor_prefix` filters posts only (system lines always included).
pub fn merge_thread_rows(
    reduced: &ReducerState,
    room_wire: &str,
    thread_tag: &str,
    since: Option<i64>,
    before: Option<i64>,
    actor_prefix: &str,
) -> Vec<MergedThreadRow> {
    let scope = crate::reducer::scope_from_room_wire(room_wire);
    let tag = canonicalize_tag(thread_tag);
    let key = (scope.clone(), tag.clone());

    let mut rows: Vec<MergedThreadRow> = Vec::new();

    if let Some(entries) = reduced.room_timeline.get(room_wire.trim()) {
        for e in entries {
            if since.map_or(true, |s| e.ts >= s) && before.map_or(true, |b| e.ts < b) {
                rows.push(MergedThreadRow::System {
                    ts: e.ts,
                    text: format_room_timeline_entry(e),
                });
            }
        }
    }

    let all_ids: Vec<String> = reduced
        .ingests_by_scope_thread
        .get(&key)
        .map(|q| q.iter().rev().cloned().collect())
        .unwrap_or_default();

    for (idx, id) in all_ids.into_iter().enumerate() {
        let Some(ing) = reduced.ingests_by_id.get(&id) else {
            continue;
        };
        if since.map_or(true, |s| ing.ts >= s) && before.map_or(true, |b| ing.ts < b) {
            if !actor_prefix.is_empty()
                && !ing
                    .principal
                    .to_lowercase()
                    .starts_with(actor_prefix)
            {
                continue;
            }
            rows.push(MergedThreadRow::Post {
                index: idx,
                id: ing.id.clone(),
                ts: ing.ts,
                principal: ing.principal.clone(),
                raw: ing.raw.clone(),
            });
        }
    }

    rows.sort_by(|a, b| {
        let ta = match a {
            MergedThreadRow::System { ts, .. } | MergedThreadRow::Post { ts, .. } => *ts,
        };
        let tb = match b {
            MergedThreadRow::System { ts, .. } | MergedThreadRow::Post { ts, .. } => *ts,
        };
        ta.cmp(&tb)
    });
    rows
}

/// Shared-site thread view (`room_wire == "public"` on the wire). Same merge as private rooms; private-room timeline is unused here.
pub fn merge_public_thread_rows(
    reduced: &ReducerState,
    thread_tag: &str,
    since: Option<i64>,
    before: Option<i64>,
    actor_prefix: &str,
) -> Vec<MergedThreadRow> {
    merge_thread_rows(reduced, "public", thread_tag, since, before, actor_prefix)
}
