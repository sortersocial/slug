use maud::html;

use crate::canonical_path::canonicalize_tag;
use crate::reducer::{scope_from_room_wire, ScopeId};
use crate::state::AppState;

use super::access::user_can_view_room;
use super::ingest::{ingest_entry_markup, post_header_row, redacted_header_row};
use super::nav::ThreadNav;
use crate::html::{render_linkified_with_embeds_in_scope, JsBuilder, now_ms};

pub(crate) async fn thread_ui_expand_post_full(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    post_index: usize,
    viewer: Option<&str>,
) -> axum::response::Response {
    let room = room.trim();
    let tag = canonicalize_tag(thread_tag);
    let scope = scope_from_room_wire(room);
    let nav = if room.is_empty() || room == "public" {
        ThreadNav::public()
    } else {
        let Some(n) = ThreadNav::from_room_id(room) else {
            return crate::html::ui_js_warn("bad room");
        };
        n
    };
    if let ScopeId::Room(ref rid) = scope {
        let reduced = state.reduced.read().await;
        if !user_can_view_room(&reduced, rid, viewer) {
            return crate::html::ui_js_warn("forbidden");
        }
    }

    let now = now_ms();
    let reduced = state.reduced.read().await;
    let ing = reduced
        .ingests_by_scope_thread
        .get(&(scope.clone(), tag.clone()))
        .and_then(|q| q.iter().rev().nth(post_index))
        .and_then(|id| reduced.ingests_by_id.get(id).cloned());
    let Some(ing) = ing else {
        return crate::html::ui_js_warn("post not found");
    };
    if reduced.redacted_posts.contains(&ing.id) {
        return crate::html::ui_js_warn("post not found");
    }
    let ing_id = ing.id.clone();
    drop(reduced);

    let full_html = html! {
        div class="ingest-entry" data-ingest-id=(ing_id) {
            (post_header_row(
                &nav,
                &tag,
                post_index,
                &ing,
                viewer,
                now,
                viewer == Some(ing.principal.as_str()),
            ))
            (render_linkified_with_embeds_in_scope(&ing.raw, nav.garden_root_url()))
        }
    };

    JsBuilder::new()
        .morph_selector(&format!("[data-ingest-id=\"{}\"]", ing_id), full_html)
        .into_response()
}

pub(crate) async fn thread_ui_expand_redacted_post(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    post_index: usize,
    viewer: Option<&str>,
) -> axum::response::Response {
    let room = room.trim();
    let tag = canonicalize_tag(thread_tag);
    let scope = scope_from_room_wire(room);
    let nav = if room.is_empty() || room == "public" {
        ThreadNav::public()
    } else {
        let Some(n) = ThreadNav::from_room_id(room) else {
            return crate::html::ui_js_warn("bad room");
        };
        n
    };
    if let ScopeId::Room(ref rid) = scope {
        let reduced = state.reduced.read().await;
        if !user_can_view_room(&reduced, rid, viewer) {
            return crate::html::ui_js_warn("forbidden");
        }
    }

    let now = now_ms();
    let reduced = state.reduced.read().await;
    let ing = reduced
        .ingests_by_scope_thread
        .get(&(scope.clone(), tag.clone()))
        .and_then(|q| q.iter().rev().nth(post_index))
        .and_then(|id| reduced.ingests_by_id.get(id).cloned());
    let Some(ing) = ing else {
        return crate::html::ui_js_warn("post not found");
    };
    if !reduced.redacted_posts.contains(&ing.id) {
        return crate::html::ui_js_warn("post not found");
    }
    let ing_id = ing.id.clone();
    drop(reduced);

    let full_html = html! {
        div class="ingest-entry ingest-redacted-expanded" data-ingest-id=(ing_id) {
            (redacted_header_row(&nav, &tag, post_index, &ing, now, true))
            (render_linkified_with_embeds_in_scope(&ing.raw, nav.garden_root_url()))
        }
    };

    JsBuilder::new()
        .morph_selector(&format!("[data-ingest-id=\"{}\"]", ing_id), full_html)
        .into_response()
}

pub(crate) async fn thread_ui_collapse_redacted_post(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    post_index: usize,
    viewer: Option<&str>,
) -> axum::response::Response {
    let room = room.trim();
    let tag = canonicalize_tag(thread_tag);
    let scope = scope_from_room_wire(room);
    let nav = if room.is_empty() || room == "public" {
        ThreadNav::public()
    } else {
        let Some(n) = ThreadNav::from_room_id(room) else {
            return crate::html::ui_js_warn("bad room");
        };
        n
    };
    if let ScopeId::Room(ref rid) = scope {
        let reduced = state.reduced.read().await;
        if !user_can_view_room(&reduced, rid, viewer) {
            return crate::html::ui_js_warn("forbidden");
        }
    }

    let now = now_ms();
    let reduced = state.reduced.read().await;
    let ing = reduced
        .ingests_by_scope_thread
        .get(&(scope.clone(), tag.clone()))
        .and_then(|q| q.iter().rev().nth(post_index))
        .and_then(|id| reduced.ingests_by_id.get(id).cloned());
    let Some(ing) = ing else {
        return crate::html::ui_js_warn("post not found");
    };
    if !reduced.redacted_posts.contains(&ing.id) {
        return crate::html::ui_js_warn("post not found");
    }
    let ing_id = ing.id.clone();
    let collapsed = ingest_entry_markup(&nav, &tag, post_index, &ing, viewer, now, &reduced);
    drop(reduced);

    JsBuilder::new()
        .morph_selector(&format!("[data-ingest-id=\"{}\"]", ing_id), collapsed)
        .into_response()
}
