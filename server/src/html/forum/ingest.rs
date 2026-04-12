use crate::canonical_path::canonicalize_tag;
use crate::form_template::template_json_compact;
use crate::reducer::{scope_from_room_wire, ReducerState};
use maud::{html, Markup};
use serde_json::json;

use crate::html::js_string_literal;
use crate::html::ui_action::{HtmlUiAction, UI_RPC_FIELD};
use crate::html::{profile_href, render_linkified_with_embeds_in_scope, timeago};

use super::nav::ThreadNav;

/// `POST /ui` + `__rpc__` from an inline link (`onclick`); same-origin credentials as other morph actions.
pub(super) fn thread_ui_fetch_onclick(rpc_compact_json: &str) -> String {
    format!(
        "fetch('/ui',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:new URLSearchParams({{__rpc__:{}}}).toString(),credentials:'same-origin'}}).then(r=>r.text()).then(eval);return false",
        js_string_literal(rpc_compact_json)
    )
}

pub(super) fn thread_nav_for_ingest(ing: &crate::events::Ingest) -> Option<ThreadNav> {
    let room = ing.room_id.trim();
    if room.is_empty() || room == "public" {
        Some(ThreadNav::public())
    } else {
        ThreadNav::from_room_id(room)
    }
}

pub(super) fn thread_post_index_in_scope(reduced: &ReducerState, ing: &crate::events::Ingest) -> Option<usize> {
    let scope = scope_from_room_wire(&ing.room_id);
    let tag = canonicalize_tag(&ing.thread_tag);
    reduced
        .ingests_by_scope_thread
        .get(&(scope, tag))
        .and_then(|q| q.iter().rev().position(|id| id == &ing.id))
}

fn post_header_meta(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    principal: &str,
    ts: i64,
    now: i64,
) -> Markup {
    let post_href = nav.post_url(tag, post_idx);
    let profile = profile_href(principal);
    let hover = timeago::rfc3339_utc(ts);
    let ago = timeago::timeago(now, ts);
    html! {
        div class="ingest-meta muted" title=(hover) {
            a href=(post_href) class="post-num" { "#" (post_idx) }
            " "
            a href=(profile) class="post-author" { "@" (principal) }
            " · "
            (ago)
        }
    }
}

pub(super) fn post_header_row(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    _viewer: Option<&str>,
    now: i64,
    show_delete: bool,
) -> Markup {
    let meta = post_header_meta(nav, tag, post_idx, &ing.principal, ing.ts, now);
    html! {
        div class="ingest-header-row" {
            (meta)
            @if show_delete {
                form class="post-delete-form" method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::RedactPost { post_id: ing.id.clone() }).unwrap());
                    button type="submit" class="post-delete-btn" { "delete" }
                }
            }
        }
    }
}

pub(super) fn redacted_header_row(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    now: i64,
    expanded: bool,
) -> Markup {
    let meta = post_header_meta(nav, tag, post_idx, &ing.principal, ing.ts, now);
    let rpc_expand = template_json_compact(&json!({
        "action": "expand_redacted_post",
        "room": nav.room_wire,
        "thread_tag": tag,
        "post_index": post_idx,
    }))
    .unwrap();
    let rpc_collapse = template_json_compact(&json!({
        "action": "collapse_redacted_post",
        "room": nav.room_wire,
        "thread_tag": tag,
        "post_index": post_idx,
    }))
    .unwrap();
    let onclick_expand = thread_ui_fetch_onclick(&rpc_expand);
    let onclick_collapse = thread_ui_fetch_onclick(&rpc_collapse);
    html! {
        div class="ingest-header-row ingest-tombstone-row" {
            (meta)
            span class="post-tombstone-inline muted" {
                "deleted · "
                @if expanded {
                    a href="#" class="hide-deleted-link"
                      onclick=(onclick_collapse) {
                        "[hide deleted content]"
                    }
                } @else {
                    a href="#" class="show-deleted-link"
                      onclick=(onclick_expand) {
                        "[show deleted content]"
                    }
                }
            }
        }
    }
}

pub(super) fn ingest_entry_markup(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    viewer: Option<&str>,
    now: i64,
    reduced: &ReducerState,
) -> Markup {
    let redacted = reduced.redacted_posts.contains(&ing.id);
    let show_delete = viewer == Some(ing.principal.as_str()) && !redacted;
    if redacted {
        html! {
            div class="ingest-entry ingest-redacted" data-ingest-id=(ing.id) {
                (redacted_header_row(nav, tag, post_idx, ing, now, false))
            }
        }
    } else {
        let truncated = ing.raw.len() > 2000;
        let display_body = if truncated { &ing.raw[..2000] } else { &ing.raw[..] };
        html! {
            div class="ingest-entry" data-ingest-id=(ing.id) {
                (post_header_row(nav, tag, post_idx, ing, viewer, now, show_delete))
                (render_linkified_with_embeds_in_scope(display_body, nav.garden_root_url()))
                @if truncated {
                    @let rpc_full = template_json_compact(&json!({
                        "action": "expand_post_full",
                        "room": nav.room_wire,
                        "thread_tag": tag,
                        "post_index": post_idx,
                    })).unwrap();
                    @let onclick_full = thread_ui_fetch_onclick(&rpc_full);
                    a href="#" class="show-full-link"
                      onclick=(onclick_full) {
                        "[show full post]"
                    }
                }
            }
        }
    }
}
