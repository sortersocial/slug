use crate::form_template::template_json_compact;
use crate::html::ui_action::{HtmlUiAction, UI_RPC_FIELD};
use maud::{html, Markup};
use serde_json::json;

use super::nav::ThreadNav;

/// Stable ids shared by public home and private room “new thread” UI (`#new-thread-ui-slot`).
const COMPOSE_SECTION_ID: &str = "new-thread-compose";
const ERRORS_ID: &str = "new-thread-errors";
const FORM_ID: &str = "new-thread-form";
const TAG_INPUT_ID: &str = "new-thread-tag";

/// Shared `<section class="compose">`: check + post with `error_target` / `form_id` (same as thread reply compose).
fn new_thread_compose_section(room_wire: &str) -> Markup {
    html! {
        section class="compose" id=(COMPOSE_SECTION_ID) {
            div id=(ERRORS_ID) {}
            form id=(FORM_ID) method="POST" action="/ui" data-check-action="/ui" data-check-rpc=(template_json_compact(&json!({
                "action": "check_ingest",
                "room": room_wire,
                "thread_tag": {"$form": "thread_tag"},
                "text": {"$form": "text"},
                "error_target": ERRORS_ID,
                "form_id": FORM_ID,
            })).unwrap()) {
                input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&json!({
                    "action": "post_ingest",
                    "room": room_wire,
                    "thread_tag": {"$form": "thread_tag"},
                    "text": {"$form": "text"},
                    "error_target": ERRORS_ID,
                    "form_id": FORM_ID,
                })).unwrap());
                input type="text" id=(TAG_INPUT_ID) name="thread_tag" pattern="[a-z0-9_\\-]{1,64}" required placeholder="thread-topic-slug-here";
                textarea name="text" rows="4" placeholder="First post body…" required {}
                p { button type="submit" { "create thread / post" } }
            }
        }
    }
}

/// Inner fragment morphed into `#new-thread-ui-slot` (Idiomorph replaces children; outer `id` stays).
fn new_thread_slot_inner(nav: &ThreadNav, compose_expanded: bool) -> Markup {
    html! {
        div class="new-thread-slot-inner" {
            @if compose_expanded {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::SetNewThreadComposeExpanded {
                        room_wire: nav.room_wire.clone(),
                        expanded: false,
                    }).expect("static json"));
                    button type="submit" class="form-toggle" aria-expanded="true" {
                        "-"
                    }
                }
                (new_thread_compose_section(&nav.room_wire))
            } @else {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::SetNewThreadComposeExpanded {
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

pub(crate) fn new_thread_slot_markup(nav: &ThreadNav, show: bool, compose_expanded: bool) -> Markup {
    if !show {
        return html! {};
    }
    new_thread_slot_inner(nav, compose_expanded)
}

pub(crate) fn fragment_new_thread_slot(nav: &ThreadNav, show: bool, compose_expanded: bool) -> Markup {
    new_thread_slot_markup(nav, show, compose_expanded)
}

pub(crate) fn login_to_post_hint_markup() -> Markup {
    html! {
        p class="muted" { "log in to post" }
    }
}
