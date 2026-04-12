use crate::form_template::template_json_compact;
use crate::html::ui_action::{HtmlUiAction, UI_RPC_FIELD};
use maud::{html, Markup};
use serde_json::json;

use super::nav::ThreadNav;

struct NewThreadIds {
    compose_section_id: &'static str,
    errors_id: &'static str,
    form_id: &'static str,
    tag_input_id: &'static str,
    text_input_id: Option<&'static str>,
}

const PUBLIC_IDS: NewThreadIds = NewThreadIds {
    compose_section_id: "public-new-thread-compose",
    errors_id: "public-new-thread-errors",
    form_id: "public-new-thread-form",
    tag_input_id: "new-thread-tag",
    text_input_id: Some("new-thread-text"),
};

const ROOM_IDS: NewThreadIds = NewThreadIds {
    compose_section_id: "room-new-thread-compose",
    errors_id: "room-new-thread-errors",
    form_id: "room-new-thread-form",
    tag_input_id: "room-new-tag",
    text_input_id: None,
};

#[derive(Clone, Copy)]
enum NewThreadComposeKind {
    /// Home page: no client-side check RPC; post template omits `error_target` / `form_id`.
    Public,
    /// Room page: `check_ingest` + error targets on post (matches thread compose).
    Room,
}

/// Shared `<section class="compose">` for creating a thread + first post.
fn new_thread_compose_section(room_wire: &str, ids: &NewThreadIds, kind: NewThreadComposeKind) -> Markup {
    let client_check = matches!(kind, NewThreadComposeKind::Room);
    let (tag_placeholder, text_placeholder, submit_label) = match kind {
        NewThreadComposeKind::Public => (
            "thread-title-slug-here",
            "Hello threadgoers!! Behold my new thread!",
            "create thread / make first post",
        ),
        NewThreadComposeKind::Room => (
            "thread-topic-slug-here",
            "First post body…",
            "post",
        ),
    };

    html! {
        section class="compose" id=(ids.compose_section_id) {
            div id=(ids.errors_id) {}
            @if client_check {
                form id=(ids.form_id) method="POST" action="/ui" data-check-action="/ui" data-check-rpc=(template_json_compact(&json!({
                    "action": "check_ingest",
                    "room": room_wire,
                    "thread_tag": {"$form": "thread_tag"},
                    "text": {"$form": "text"},
                    "error_target": ids.errors_id,
                    "form_id": ids.form_id,
                })).unwrap()) {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&json!({
                        "action": "post_ingest",
                        "room": room_wire,
                        "thread_tag": {"$form": "thread_tag"},
                        "text": {"$form": "text"},
                        "error_target": ids.errors_id,
                        "form_id": ids.form_id,
                    })).unwrap());
                    input type="text" id=(ids.tag_input_id) name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" required placeholder=(tag_placeholder);
                    @if let Some(tid) = ids.text_input_id {
                        textarea id=(tid) name="text" rows="4" placeholder=(text_placeholder) required {}
                    } @else {
                        textarea name="text" rows="4" placeholder=(text_placeholder) required {}
                    }
                    p { button type="submit" { (submit_label) } }
                }
            } @else {
                form id=(ids.form_id) method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&json!({
                        "action": "post_ingest",
                        "room": room_wire,
                        "thread_tag": {"$form": "thread_tag"},
                        "text": {"$form": "text"},
                    })).unwrap());
                    input type="text" id=(ids.tag_input_id) name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" placeholder=(tag_placeholder);
                    @if let Some(tid) = ids.text_input_id {
                        textarea id=(tid) name="text" rows="4" placeholder=(text_placeholder) {}
                    } @else {
                        textarea name="text" rows="4" placeholder=(text_placeholder) {}
                    }
                    p { button type="submit" { (submit_label) } }
                }
            }
        }
    }
}

fn new_thread_form_public(show: bool) -> Markup {
    if !show {
        return html! {};
    }
    new_thread_compose_section("public", &PUBLIC_IDS, NewThreadComposeKind::Public)
}

fn new_thread_form_for_room(nav: &ThreadNav, show: bool, compose_expanded: bool) -> Markup {
    if !show {
        return html! {};
    }
    // Single root for Idiomorph when morphing `#room-new-thread-ui-slot` (expanded has form + section).
    html! {
        div class="room-new-thread-slot-inner" {
            @if compose_expanded {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::SetRoomNewThreadComposeExpanded {
                        room_wire: nav.room_wire.clone(),
                        expanded: false,
                    }).expect("static json"));
                    button type="submit" class="form-toggle" aria-expanded="true" {
                        "-"
                    }
                }
                (new_thread_compose_section(&nav.room_wire, &ROOM_IDS, NewThreadComposeKind::Room))
            } @else {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::SetRoomNewThreadComposeExpanded {
                        room_wire: nav.room_wire.clone(),
                        expanded: true,
                    }).expect("static json"));
                    button type="submit" class="form-toggle" aria-expanded="false" {
                        "+"
                    }
                }
            }
        }
    }
}

pub(crate) fn login_to_post_hint_markup() -> Markup {
    html! {
        p class="muted" { "log in to post" }
    }
}

pub(crate) fn fragment_public_new_thread_form(show: bool) -> Markup {
    new_thread_form_public(show)
}

pub(crate) fn fragment_room_new_thread_form(nav: &ThreadNav, show: bool, compose_expanded: bool) -> Markup {
    new_thread_form_for_room(nav, show, compose_expanded)
}
