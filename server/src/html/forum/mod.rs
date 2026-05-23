//! Forum / thread HTML: public home, room index, thread views, profile, and UI morph helpers.

mod access;
mod copy;
mod feed;
mod ingest;
mod nav;
mod new_thread;
mod page;
mod paginator;
mod post_single;
mod profile;
mod room_members;
mod thread_morph;
mod views;

pub use feed::{home, thread_feed_html, thread_feed_html_for_room, thread_feed_region_markup};
pub use nav::ThreadNav;
pub use post_single::{room_thread_post_view, thread_post_view};
pub use profile::user_profile_page;
pub use views::{room_page, room_thread_view, thread_view};

pub(crate) use access::{user_can_post_room, user_can_view_room};
pub(crate) use copy::thread_ui_copy_thread;
pub(crate) use new_thread::{fragment_new_thread_slot, login_to_post_hint_markup};
pub(crate) use room_members::room_members_section_markup;
pub(crate) use thread_morph::{
    thread_ui_collapse_redacted_post, thread_ui_expand_post_full, thread_ui_expand_redacted_post,
};
