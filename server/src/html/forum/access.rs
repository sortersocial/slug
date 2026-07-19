use crate::events::ThreadCapability;
use crate::reducer::ReducerState;

pub fn user_can_view_room(reduced: &ReducerState, room_id: &str, username: Option<&str>) -> bool {
    if !reduced.rooms.contains(room_id) {
        return false;
    }
    let Some(u) = username else {
        return false;
    };
    reduced.user_has_cap(room_id, u, ThreadCapability::View)
}

pub(crate) fn user_can_post_room(reduced: &ReducerState, room_id: &str, username: &str) -> bool {
    reduced.user_has_cap(room_id, username, ThreadCapability::Post)
}

pub(crate) fn user_can_manage_room(reduced: &ReducerState, room_id: &str, username: &str) -> bool {
    reduced.user_has_cap(room_id, username, ThreadCapability::Manage)
}
