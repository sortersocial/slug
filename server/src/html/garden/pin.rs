use axum_extra::extract::cookie::CookieJar;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64_ENGINE, Engine as _};
use maud::html;
use serde_json::json;

use crate::{
    form_template::template_json_compact,
    html::{forum::ThreadNav, ui_action::UI_RPC_FIELD},
    path_types::ItemId,
    reducer::ContentState,
};

use super::vote::{edge_vote_count_for_pair, vote_compare_href};

pub(crate) const GARDEN_PIN_COOKIE: &str = "slug_garden_pin";
const PIN_COOKIE_SEP: char = '\x1f';

/// Parse `slug_garden_pin`: base64(`room` + US + item storage), or legacy unencoded form.
pub(super) fn pinned_item_from_jar(jar: &CookieJar) -> Option<(String, ItemId)> {
    let c = jar.get(GARDEN_PIN_COOKIE)?;
    let v = c.value();
    let raw: String = if let Ok(bytes) = B64_ENGINE.decode(v) {
        String::from_utf8(bytes).ok()?
    } else {
        v.to_string()
    };
    let (room, rest) = raw.split_once(PIN_COOKIE_SEP)?;
    let item = ItemId::parse(rest.trim())?;
    Some((room.trim().to_string(), item.normalized_storage()))
}

pub(crate) fn encode_pin_cookie_value(room: &str, item_storage: &str) -> String {
    let raw = format!("{room}{PIN_COOKIE_SEP}{item_storage}");
    B64_ENGINE.encode(raw.as_bytes())
}

pub(super) fn ont_pin_vote_controls(
    nav: &ThreadNav,
    current_storage: &str,
    pinned_room_and_item: Option<&(String, ItemId)>,
    next_path: &str,
) -> maud::Markup {
    let room_wire = nav.room_wire.clone();
    let current = ItemId::parse(current_storage)
        .unwrap_or_else(|| ItemId::opaque(current_storage.to_string()));
    let pin_matches_scope = pinned_room_and_item
        .map(|(r, _)| r == nav.room_wire.as_str())
        .unwrap_or(false);
    let pinned_item = pinned_room_and_item
        .filter(|_| pin_matches_scope)
        .map(|(_, i)| i);

    let pin_rpc = template_json_compact(&json!({
        "action": "set_garden_pin",
        "clear": false,
        "room_wire": room_wire,
        "item_storage": current.as_str(),
        "next": next_path,
        "form_action": "/ui",
    }))
    .expect("pin rpc json");
    let unpin_rpc = template_json_compact(&json!({
        "action": "set_garden_pin",
        "clear": true,
        "room_wire": "",
        "next": next_path,
        "form_action": "/ui",
    }))
    .expect("unpin rpc json");

    html! {
        div class="ont-item-pin-zone" data-garden-room=(room_wire.as_str()) {
            @if let Some(pi) = pinned_item {
                @if pi == &current {
                    form method="POST" action="/ui" data-navigate="full" class="ont-pin-form" {
                        input type="hidden" name=(UI_RPC_FIELD) value=(unpin_rpc);
                        button type="submit" class="ont-pin-btn ont-pin-btn-active" title="Unpin" aria-label="Unpin from HUD" {
                            span class="ont-pin-glyph" aria-hidden="true" { "📌" }
                        }
                    }
                } @else {
                    a class="ont-vote-compare-btn" href=(vote_compare_href(nav, pi, &current, None, None)) title="Compare and vote" {
                        span class="ont-vote-glyph" aria-hidden="true" { "⚖" }
                        span { "vote" }
                    }
                }
            } @else {
                form method="POST" action="/ui" data-navigate="full" class="ont-pin-form" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(pin_rpc);
                    button type="submit" class="ont-pin-btn" title="Pin to HUD" aria-label="Pin to corner" {
                        span class="ont-pin-glyph" aria-hidden="true" { "📌" }
                    }
                }
            }
        }
    }
}

pub(super) fn child_row_pin_or_vote(
    nav: &ThreadNav,
    row_item: &ItemId,
    pinned_room_and_item: Option<&(String, ItemId)>,
    scope_content: &ContentState,
) -> maud::Markup {
    let pin_matches_scope = pinned_room_and_item
        .map(|(r, _)| r == nav.room_wire.as_str())
        .unwrap_or(false);
    let pinned_item = pinned_room_and_item
        .filter(|_| pin_matches_scope)
        .map(|(_, i)| i);

    html! {
        @if let Some(pi) = pinned_item {
            span class="ont-garden-child-actions" data-garden-room=(nav.room_wire.as_str()) {
                @if pi == row_item {
                    span class="ont-garden-pinned-here" title="Pinned" aria-label="Pinned" { "📌" }
                } @else {
                    @let nv = edge_vote_count_for_pair(scope_content, pi, row_item);
                    @let tip = format!(
                        "Compare and vote — {nv} pairwise vote{} in this scope for pinned vs this row",
                        if nv == 1 { "" } else { "s" },
                    );
                    @let aria = format!("Vote; {} pairwise {}", nv, if nv == 1 { "vote" } else { "votes" });
                    a class="ont-garden-vote-ico" href=(vote_compare_href(nav, pi, row_item, None, None)) title=(tip) aria-label=(aria) {
                        span class="ont-garden-vote-glyph" aria-hidden="true" { "⚖" }
                        span class="ont-garden-vote-count" { (format!("{}", nv)) }
                    }
                }
            }
        }
    }
}
