//! Live SSE: topic subscription lists derived from page URLs (same semantics as HTML routes).

use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use futures_util::StreamExt;
use slug_types::room_id_from_route_segment;
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    api::optional_principal,
    html::user_can_view_room,
    state::{AppState, JsSnippet, JsSnippetAudience, LiveTopic},
};

pub fn subscriber_topics_public_thread(tag: &str) -> Vec<LiveTopic> {
    vec![
        LiveTopic::ForumHome,
        LiveTopic::PublicThread {
            tag: tag.to_string(),
        },
    ]
}

pub fn subscriber_topics_public_home() -> Vec<LiveTopic> {
    vec![LiveTopic::ForumHome]
}

pub fn subscriber_topics_room_thread(room_id: &str, tag: &str) -> Vec<LiveTopic> {
    vec![
        LiveTopic::RoomForum {
            room_id: room_id.to_string(),
        },
        LiveTopic::RoomThread {
            room_id: room_id.to_string(),
            tag: tag.to_string(),
        },
    ]
}

pub fn subscriber_topics_room_forum(room_id: &str) -> Vec<LiveTopic> {
    vec![LiveTopic::RoomForum {
        room_id: room_id.to_string(),
    }]
}

pub fn subscriber_topics_public_garden_root() -> Vec<LiveTopic> {
    vec![LiveTopic::PublicGardenRoot]
}

pub fn subscriber_topics_public_garden_path(tail: &str) -> Vec<LiveTopic> {
    vec![
        LiveTopic::PublicGardenRoot,
        LiveTopic::PublicGardenPath {
            tail: tail.to_string(),
        },
    ]
}

pub fn subscriber_topics_public_external_path(tail: &str) -> Vec<LiveTopic> {
    vec![LiveTopic::PublicGardenExternal {
        tail: tail.to_string(),
    }]
}

pub fn subscriber_topics_room_garden_root(room_id: &str) -> Vec<LiveTopic> {
    vec![LiveTopic::RoomGardenRoot {
        room_id: room_id.to_string(),
    }]
}

pub fn subscriber_topics_room_garden_path(room_id: &str, tail: &str) -> Vec<LiveTopic> {
    vec![
        LiveTopic::RoomGardenRoot {
            room_id: room_id.to_string(),
        },
        LiveTopic::RoomGardenPath {
            room_id: room_id.to_string(),
            tail: tail.to_string(),
        },
    ]
}

pub fn subscriber_topics_room_external_path(room_id: &str, tail: &str) -> Vec<LiveTopic> {
    vec![LiveTopic::RoomGardenExternal {
        room_id: room_id.to_string(),
        tail: tail.to_string(),
    }]
}

pub fn subscriber_topics_garden_item(
    room_wire: Option<&str>,
    item_storage: &str,
) -> Vec<LiveTopic> {
    vec![LiveTopic::GardenItem {
        room_wire: room_wire.map(|s| s.to_string()),
        item_storage: item_storage.to_string(),
    }]
}

/// Derive subscription topics from `window.location` style path + query (legacy `/sse?path=`).
pub fn topics_from_legacy_sse_path(full_path: &str) -> Option<(Vec<LiveTopic>, Option<String>)> {
    let path_only = full_path.split('?').next().unwrap_or(full_path);
    let segs: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();

    if path_only == "/" || path_only.is_empty() {
        return Some((subscriber_topics_public_home(), None));
    }

    // /t/:tag
    if segs.len() == 2 && segs[0] == "t" {
        let tag = crate::canonical_path::canonicalize_tag(segs[1]);
        return Some((subscriber_topics_public_thread(&tag), None));
    }

    // /t/:tag/:index (single post view — same thread topics)
    if segs.len() == 3 && segs[0] == "t" {
        let tag = crate::canonical_path::canonicalize_tag(segs[1]);
        return Some((subscriber_topics_public_thread(&tag), None));
    }

    // /~ or /~/...
    if segs[0] == "~" {
        let tail = segs[1..].join("/");
        return Some((subscriber_topics_public_garden_path(&tail), None));
    }

    // /- or /-/...
    if segs[0] == "-" {
        let tail = segs[1..].join("/");
        return Some((subscriber_topics_public_external_path(&tail), None));
    }

    // /r/:seg/...
    if segs.len() >= 2 && segs[0] == "r" {
        let room_id = room_id_from_route_segment(segs[1])?;
        // /r/:seg/t/:tag[/idx]
        if segs.len() >= 4 && segs[2] == "t" {
            let tag = crate::canonical_path::canonicalize_tag(segs[3]);
            return Some((
                subscriber_topics_room_thread(&room_id, &tag),
                Some(room_id.clone()),
            ));
        }
        // /r/:seg only (room forum index)
        if segs.len() == 2 {
            return Some((
                subscriber_topics_room_forum(&room_id),
                Some(room_id.clone()),
            ));
        }
        // /r/:seg/~[/tail]
        if segs.len() >= 3 && segs[2] == "~" {
            let tail = segs[3..].join("/");
            return Some((
                subscriber_topics_room_garden_path(&room_id, &tail),
                Some(room_id.clone()),
            ));
        }
        // /r/:seg/-/...
        if segs.len() >= 3 && segs[2] == "-" {
            let tail = segs[3..].join("/");
            return Some((
                subscriber_topics_room_external_path(&room_id, &tail),
                Some(room_id.clone()),
            ));
        }
    }

    // Fallback: thread index bumps still relevant on ancillary pages.
    Some((subscriber_topics_public_home(), None))
}

fn snippet_allowed(
    snippet: &JsSnippet,
    subscriptions: &[LiveTopic],
    username: Option<&str>,
    reduced: &crate::reducer::ReducerState,
) -> bool {
    let topic_ok = snippet
        .topics
        .iter()
        .any(|t| subscriptions.iter().any(|s| s == t));
    if !topic_ok {
        return false;
    }
    match &snippet.audience {
        JsSnippetAudience::Public => true,
        JsSnippetAudience::RoomViewers(room_id) => user_can_view_room(reduced, room_id, username),
    }
}

pub fn wants_event_stream(headers: &HeaderMap) -> bool {
    let Some(accept) = headers.get(header::ACCEPT) else {
        return false;
    };
    let Ok(s) = accept.to_str() else {
        return false;
    };
    s.split(',').any(|part| {
        part.trim()
            .split(';')
            .next()
            .map(|m| m.trim().eq_ignore_ascii_case("text/event-stream"))
            .unwrap_or(false)
    })
}

fn sse_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        [(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")],
        ": forbidden\n\n",
    )
        .into_response()
}

pub async fn live_sse_multi_topic(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    subscriptions: Vec<LiveTopic>,
    gate_room_view: Option<String>,
) -> Response {
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    if let Some(ref rid) = gate_room_view {
        if !user_can_view_room(&reduced, rid, user.as_deref()) {
            drop(reduced);
            return sse_forbidden();
        }
    }
    drop(reduced);

    let user_for_filter = user;
    let state_clone = state.clone();
    let subs_owned = subscriptions;
    let js_rx = state.js_tx.subscribe();

    let js_updates = BroadcastStream::new(js_rx).filter_map(move |msg| {
        let state = state_clone.clone();
        let subscriptions = subs_owned.clone();
        let user_for_filter = user_for_filter.clone();
        async move {
            match msg {
                Ok(snippet) => {
                    let reduced = state.reduced.read().await;
                    let allowed = snippet_allowed(
                        &snippet,
                        &subscriptions,
                        user_for_filter.as_deref(),
                        &reduced,
                    );
                    drop(reduced);
                    if allowed {
                        Some(Ok::<_, std::convert::Infallible>(
                            SseEvent::default().data(snippet.code),
                        ))
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        }
    });

    Sse::new(js_updates)
        .keep_alive(KeepAlive::default())
        .into_response()
}
