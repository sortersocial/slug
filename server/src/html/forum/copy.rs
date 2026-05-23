use crate::canonical_path::canonicalize_tag;
use crate::form_template::template_json_compact;
use crate::reducer::{scope_from_room_wire, ReducerState, ScopeId};
use crate::state::AppState;
use maud::{html, Markup};
use slug_types::timeago::timeago_compact;

use super::access::user_can_view_room;
use super::ingest::thread_ui_fetch_onclick;
use super::nav::ThreadNav;
use crate::html::ui_action::HtmlUiAction;
use crate::html::{JsBuilder, now_ms};

/// CLI `forum show` text (`print_thread` in `cli/src/main.rs`).
pub(crate) fn format_thread_cli_text(
    reduced: &ReducerState,
    scope: &ScopeId,
    tag: &str,
    now_ms: i64,
) -> String {
    let tag = canonicalize_tag(tag);
    let key = (scope.clone(), tag);
    let all_ids: Vec<String> = reduced
        .ingests_by_scope_thread
        .get(&key)
        .map(|q| q.iter().rev().cloned().collect())
        .unwrap_or_default();

    let mut out = String::new();
    for (i, id) in all_ids.iter().enumerate() {
        let Some(ing) = reduced.ingests_by_id.get(id) else {
            continue;
        };
        if i > 0 {
            out.push_str("\n\n\n");
        }
        let index = i;
        let timeago = timeago_compact(now_ms, ing.ts);
        let redacted = reduced.redacted_posts.contains(&ing.id);
        let body = if redacted {
            String::new()
        } else {
            ing.raw.trim().to_string()
        };
        out.push_str(&format!("<post index=\"{index}\" timeago=\"{timeago}\">\n"));
        out.push_str(&body);
        out.push_str("\n</post>");
    }
    out
}

pub(crate) async fn thread_ui_copy_thread(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    copy_btn_id: &str,
    viewer: Option<&str>,
) -> axum::response::Response {
    let room = room.trim();
    let tag = canonicalize_tag(thread_tag);
    let scope = scope_from_room_wire(room);
    if let ScopeId::Room(ref rid) = scope {
        let reduced = state.reduced.read().await;
        if !user_can_view_room(&reduced, rid, viewer) {
            return crate::html::ui_js_warn("forbidden");
        }
    }

    let now = now_ms();
    let reduced = state.reduced.read().await;
    let text = format_thread_cli_text(&reduced, &scope, &tag, now);
    drop(reduced);

    JsBuilder::new()
        .clipboard_write_text_and_label_btn(&text, copy_btn_id, "copied")
        .into_response()
}

pub(super) fn thread_copy_button_markup(nav: &ThreadNav, tag: &str, top: bool) -> Markup {
    let copy_btn_id = if top {
        "thread-copy-top"
    } else {
        "thread-copy-bottom"
    };
    let rpc = template_json_compact(&HtmlUiAction::CopyThread {
        room: nav.room_wire.clone(),
        thread_tag: tag.to_string(),
        copy_btn_id: copy_btn_id.to_string(),
    })
    .expect("CopyThread serializes");
    html! {
        button type="button" id=(copy_btn_id) class="post-nav-btn thread-copy-btn" title="Copy full thread"
            onclick=(thread_ui_fetch_onclick(&rpc)) {
            "copy"
        }
    }
}
