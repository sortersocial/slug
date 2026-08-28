use crate::form_template::template_json_compact;
use crate::reducer::{scope_from_room_wire, ReducerState};
use maud::{html, Markup};
use serde_json::json;
use slug_types::MAX_FORUM_POST_PREVIEW_CHARS;

use crate::html::js_string_literal;
use crate::html::ui_action::{HtmlUiAction, UI_RPC_FIELD};
use crate::html::{
    authorship_attr, profile_href, recency_color_style, render_linkified_with_embeds_in_scope,
};
use crate::timeago;

use super::nav::{post_fragment_id, ThreadNav};

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

pub(super) fn thread_post_index_in_scope(
    reduced: &ReducerState,
    ing: &crate::events::Ingest,
) -> Option<usize> {
    let scope = scope_from_room_wire(&ing.room_id);
    reduced.try_thread_post_index_chronological(&scope, &ing.thread_tag, &ing.id)
}

fn post_header_meta(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    principal: &str,
    delegate: &Option<String>,
    ts: i64,
    now: i64,
    delete_post_id: Option<&str>,
) -> Markup {
    let post_href = nav.post_url(tag, post_idx);
    let profile = profile_href(principal);
    let ts_hover = timeago::rfc3339_utc(ts);
    let ago = timeago::timeago(now, ts);
    let ts_style = recency_color_style(now, ts);
    let attr = authorship_attr(principal, delegate);
    let author_style = format!("color:{}", attr.color);
    html! {
        div class="ingest-meta muted" {
            span class="ingest-meta-primary" {
                a href=(post_href) class="post-num" { "#" (post_idx) }
                " "
                @if let Some(ref title) = attr.author_title {
                    a href=(profile) class="post-author" style=(author_style.as_str()) title=(title) { (attr.label.as_str()) }
                } @else {
                    a href=(profile) class="post-author" style=(author_style.as_str()) { (attr.label.as_str()) }
                }
                " · "
                span class="ts-recency" style=(ts_style.as_str()) title=(ts_hover) { (ago) }
            }
            @if let Some(pid) = delete_post_id {
                form class="post-delete-form" method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(template_json_compact(&HtmlUiAction::RedactPost { post_id: pid.to_string() }).unwrap());
                    button type="submit" class="post-delete-btn" { "delete" }
                }
            }
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
    let delete_post_id = if show_delete {
        Some(ing.id.as_str())
    } else {
        None
    };
    post_header_meta(
        nav,
        tag,
        post_idx,
        &ing.principal,
        &ing.delegate,
        ing.ts,
        now,
        delete_post_id,
    )
}

pub(super) fn redacted_header_row(
    nav: &ThreadNav,
    tag: &str,
    post_idx: usize,
    ing: &crate::events::Ingest,
    now: i64,
    expanded: bool,
) -> Markup {
    let meta = post_header_meta(
        nav,
        tag,
        post_idx,
        &ing.principal,
        &ing.delegate,
        ing.ts,
        now,
        None,
    );
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

pub(crate) fn ingest_entry_markup(
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
    let frag = post_fragment_id(post_idx);
    if redacted {
        html! {
            div class="ingest-entry ingest-redacted" id=(frag.as_str()) data-ingest-id=(ing.id) {
                (redacted_header_row(nav, tag, post_idx, ing, now, false))
            }
        }
    } else {
        let char_len = ing.raw.chars().count();
        let truncated = char_len > MAX_FORUM_POST_PREVIEW_CHARS;
        let display_body = if truncated {
            let byte_end = ing
                .raw
                .char_indices()
                .nth(MAX_FORUM_POST_PREVIEW_CHARS)
                .map(|(i, _)| i)
                .unwrap_or(ing.raw.len());
            &ing.raw[..byte_end]
        } else {
            &ing.raw[..]
        };
        let entry_class = if truncated {
            "ingest-entry ingest-entry-truncated"
        } else {
            "ingest-entry"
        };
        let item_bodies = reduced
            .content_for_scope(&nav.scope())
            .map(|c| &c.item_bodies);
        html! {
            div class=(entry_class) id=(frag.as_str()) data-ingest-id=(ing.id) {
                (post_header_row(nav, tag, post_idx, ing, viewer, now, show_delete))
                (render_linkified_with_embeds_in_scope(display_body, nav.garden_root_url(), item_bodies))
                @if truncated {
                    div class="post-truncation-banner" role="note" {
                        p.post-truncation-title { "Long post — preview ends here" }
                        p class="post-truncation-meta muted" {
                            "Showing first "
                            (MAX_FORUM_POST_PREVIEW_CHARS)
                            " of "
                            (char_len)
                            " characters (body continues below the fold)."
                        }
                        @let rpc_full = template_json_compact(&json!({
                            "action": "expand_post_full",
                            "room": nav.room_wire,
                            "thread_tag": tag,
                            "post_index": post_idx,
                        })).unwrap();
                        @let onclick_full = thread_ui_fetch_onclick(&rpc_full);
                        p.post-truncation-action {
                            a href="#" class="show-full-link"
                              onclick=(onclick_full) {
                                "[show full post]"
                            }
                        }
                    }
                }
            }
        }
    }
}
