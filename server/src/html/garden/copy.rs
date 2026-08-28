//! Copy garden rankings to the clipboard as concise markdown (POST /ui + JsBuilder eval).

use crate::form_template::template_json_compact;
use crate::html::forum::ThreadNav;
use crate::html::js_string_literal;
use crate::html::ui_action::HtmlUiAction;
use crate::html::{JsBuilder, ui_js_warn};
use crate::path_types::ItemId;
use crate::reducer::scope_from_room_wire;
use crate::scope_rank::{
    build_children_rankings, build_rankings_for_item_set, external_root_host_items,
    resolve_scope_recursive, ChildrenRankings,
};
use crate::state::AppState;
use maud::{html, Markup};

use super::access::user_can_view_room;
use super::item::item_display_path;
use crate::reducer::{ContentState, ScopeId};

const COPY_BTN_ID: &str = "garden-rank-copy";

/// `POST /ui` + `__rpc__` from an inline button; response body is `eval`'d (same as forum copy).
fn garden_ui_fetch_onclick(rpc_compact_json: &str) -> String {
    format!(
        "fetch('/ui',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:new URLSearchParams({{__rpc__:{}}}).toString(),credentials:'same-origin'}}).then(r=>r.text()).then(eval);return false",
        js_string_literal(rpc_compact_json)
    )
}

/// Concise markdown for ranked child groups (numbered lists + unranked bullets).
pub(crate) fn format_garden_rank_markdown(rankings: &ChildrenRankings) -> String {
    let mut out = String::new();
    let multi = rankings.component_rankings.len() > 1;
    for (ci, comp) in rankings.component_rankings.iter().enumerate() {
        if ci > 0 {
            out.push('\n');
        }
        if multi {
            out.push_str(&format!("### ordering {}\n\n", ci + 1));
        }
        for (i, r) in comp.ranked.iter().enumerate() {
            out.push_str(&format!(
                "{}. {}\n",
                i + 1,
                item_display_path(r.item.as_str())
            ));
        }
    }
    if !rankings.unranked_items.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        for name in &rankings.unranked_items {
            out.push_str(&format!("- {}\n", item_display_path(name.as_str())));
        }
    }
    out
}

fn rankings_for_copy(
    state_content: &crate::reducer::ContentState,
    parent_path: &str,
    depth: usize,
    external_hosts: bool,
) -> ChildrenRankings {
    if external_hosts {
        let hosts = external_root_host_items(state_content);
        return build_rankings_for_item_set(state_content, &hosts);
    }
    let parent = ItemId::parse(parent_path.trim()).map(|id| id.ontology_leaf())
        .unwrap_or_else(|| ItemId::ontology_root())
        .normalized_storage();
    let depth = depth.max(1);
    if depth > 1 {
        let items = resolve_scope_recursive(state_content, &[parent.as_str().to_string()], depth);
        build_rankings_for_item_set(state_content, &items)
    } else {
        build_children_rankings(state_content, &parent)
    }
}

pub(crate) async fn garden_ui_copy_rank(
    state: &AppState,
    room: &str,
    parent_path: &str,
    depth: usize,
    copy_btn_id: &str,
    external_hosts: bool,
    viewer: Option<&str>,
) -> axum::response::Response {
    let room = room.trim();
    let scope = scope_from_room_wire(room);
    if let ScopeId::Room(ref rid) = scope {
        let reduced = state.reduced.read().await;
        if !user_can_view_room(&reduced, rid, viewer) {
            return ui_js_warn("forbidden");
        }
    }

    let reduced = state.reduced.read().await;
    let empty = ContentState::default();
    let content = match &scope {
        ScopeId::Public => reduced.public(),
        ScopeId::Room(_) => reduced.content_for_scope(&scope).unwrap_or(&empty),
    };
    let rankings = rankings_for_copy(content, parent_path, depth, external_hosts);
    let text = format_garden_rank_markdown(&rankings);
    drop(reduced);

    if text.is_empty() {
        return ui_js_warn("nothing to copy");
    }

    JsBuilder::new()
        .clipboard_write_text_and_label_btn(&text, copy_btn_id, "copied")
        .into_response()
}

pub(super) fn garden_rank_copy_button_markup(
    nav: &ThreadNav,
    parent_path: &str,
    depth: usize,
    external_hosts: bool,
) -> Markup {
    let rpc = template_json_compact(&HtmlUiAction::CopyGardenRank {
        room: nav.room_wire.clone(),
        parent_path: parent_path.to_string(),
        depth,
        copy_btn_id: COPY_BTN_ID.to_string(),
        external_hosts,
    })
    .expect("CopyGardenRank serializes");
    html! {
        button type="button" id=(COPY_BTN_ID) class="post-nav-btn ont-rank-copy-btn" title="Copy ranking as markdown"
            onclick=(garden_ui_fetch_onclick(&rpc)) {
            "copy"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_types::ItemId;
    use crate::ranking::RankedItem;
    use crate::scope_rank::ScopedComponent;

    #[test]
    fn markdown_single_component_and_unranked() {
        let rankings = ChildrenRankings {
            component_rankings: vec![ScopedComponent {
                pairs: 1,
                ranked: vec![
                    RankedItem {
                        item: ItemId::parse("~/a").unwrap(),
                        score: 0.9,
                    },
                    RankedItem {
                        item: ItemId::parse("~/b").unwrap(),
                        score: 0.1,
                    },
                ],
            }],
            unranked_items: vec![ItemId::parse("~/c").unwrap()],
        };
        assert_eq!(
            format_garden_rank_markdown(&rankings),
            "1. ~/a\n2. ~/b\n\n- ~/c\n"
        );
    }

    #[test]
    fn markdown_multi_component_headers() {
        let rankings = ChildrenRankings {
            component_rankings: vec![
                ScopedComponent {
                    pairs: 1,
                    ranked: vec![RankedItem {
                        item: ItemId::parse("~/a").unwrap(),
                        score: 1.0,
                    }],
                },
                ScopedComponent {
                    pairs: 1,
                    ranked: vec![RankedItem {
                        item: ItemId::parse("~/b").unwrap(),
                        score: 1.0,
                    }],
                },
            ],
            unranked_items: vec![],
        };
        assert_eq!(
            format_garden_rank_markdown(&rankings),
            "### ordering 1\n\n1. ~/a\n\n### ordering 2\n\n1. ~/b\n"
        );
    }
}
