use crate::events::ThreadCapability;
use crate::form_template::template_json_compact;
use crate::reducer::ReducerState;
use maud::{html, Markup};

use crate::html::ui_action::{HtmlUiAction, UI_RPC_FIELD};

#[derive(Clone)]
pub(super) struct RoomMemberRow {
    pub(super) username: String,
    pub(super) capabilities: Vec<&'static str>,
}

fn capability_label(cap: ThreadCapability) -> &'static str {
    match cap {
        ThreadCapability::View => "view",
        ThreadCapability::Post => "post",
        ThreadCapability::Vote => "vote",
        ThreadCapability::AddItem => "add_item",
        ThreadCapability::Manage => "manage",
    }
}

fn room_members_for_room(reduced: &ReducerState, room_id: &str) -> Vec<RoomMemberRow> {
    let mut rows: Vec<RoomMemberRow> = reduced
        .grants
        .get(room_id)
        .into_iter()
        .flat_map(|members| members.iter())
        .map(|(username, caps)| {
            let mut ordered = Vec::new();
            for cap in [
                ThreadCapability::View,
                ThreadCapability::Post,
                ThreadCapability::Vote,
                ThreadCapability::AddItem,
                ThreadCapability::Manage,
            ] {
                if caps.contains(&cap) {
                    ordered.push(capability_label(cap));
                }
            }
            RoomMemberRow {
                username: username.clone(),
                capabilities: ordered,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.username.cmp(&b.username));
    rows
}

fn room_members_inner(members: &[RoomMemberRow]) -> Markup {
    html! {
        h3 { "members" }
        ul class="room-members" {
            @for member in members {
                li {
                    span class="room-member-name" { "@" (member.username) }
                    span class="muted" {
                        " · "
                        (member.capabilities.join(", "))
                    }
                }
            }
        }
    }
}

/// Fragment for `#room-members-section` — expand/collapse is server-driven via `POST /ui`.
pub(crate) fn room_members_section_markup(
    reduced: &ReducerState,
    room_id: &str,
    members_expanded: bool,
) -> Markup {
    let members = room_members_for_room(reduced, room_id);
    if members.is_empty() {
        return html! {};
    }
    html! {
        div id="room-members-section" {
            @if members_expanded {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::SetRoomMembersExpanded {
                        room_wire: room_id.to_string(),
                        expanded: false,
                    }).expect("static json"));
                    button type="submit" class="form-toggle" aria-expanded="true" {
                        "hide members & permissions"
                    }
                }
                section class="room-members-panel" {
                    (room_members_inner(&members))
                }
            } @else {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::SetRoomMembersExpanded {
                        room_wire: room_id.to_string(),
                        expanded: true,
                    }).expect("static json"));
                    button type="submit" class="form-toggle" aria-expanded="false" {
                        "members & permissions"
                    }
                }
            }
        }
    }
}
