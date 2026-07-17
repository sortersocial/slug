//! Garden / ontology HTML: tilde paths, external URLs, vote compare, pins.

mod access;
mod browse;
mod copy;
mod external;
mod item;
mod item_page;
mod pin;
mod render;
mod routes;
mod vote;

#[cfg(test)]
mod tests;

pub(crate) use copy::garden_ui_copy_rank;
pub(crate) use external::external_resolver_status_markup;
pub(crate) use pin::{encode_pin_cookie_value, GARDEN_PIN_COOKIE};
pub(crate) use vote::vote_compare_post_success_js;

pub use routes::{
    external_garden_index, external_ontology_path, garden_index, ontology_path,
    room_external_garden_index, room_external_ontology_path, room_garden_index, room_ontology_path,
};
pub use vote::{room_vote_compare_page, vote_compare_page};
