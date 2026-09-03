//! Browser-only UI commands: JSON in hidden `__rpc__` plus hole fill ([`crate::form_template`]).
//! Not part of [`slug_types::RpcCommand`] (CLI / JSON API).

use crate::form_template::fill_template_from_form;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

/// Form field name for the compact JSON template (possibly with `{"$form":"…"}` holes).
pub const UI_RPC_FIELD: &str = "__rpc__";

fn default_ui_form_action() -> String {
    "/ui".to_string()
}

/// HTML form / fetch `POST /ui` payload after template fill and deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HtmlUiAction {
    /// Forum ingest via `POST /ui`.
    PostIngest {
        room: String,
        thread_tag: String,
        text: String,
        #[serde(default)]
        error_target: Option<String>,
        #[serde(default)]
        form_id: Option<String>,
    },
    /// DSL check / validation via `POST /ui`.
    CheckIngest {
        room: String,
        thread_tag: String,
        text: String,
        #[serde(default)]
        error_target: Option<String>,
        #[serde(default)]
        form_id: Option<String>,
    },
    /// Post a pairwise vote from `/vote` (browser compose).
    VoteComparePost {
        room: String,
        thread_tag: String,
        left_item: String,
        right_item: String,
        /// From form fields (string); parsed server-side.
        ratio_left: String,
        ratio_right: String,
        explanation: String,
        /// Same-origin path after successful post (JS redirect).
        next: String,
        /// Pool parent item path, if the vote was initiated from a pool URL.
        #[serde(default)]
        pool: Option<String>,
        /// Aspect slug for an aspect-grouped vote (DSL `:slug` / API).
        /// Empty / omitted → canonical ranking (same as `/vote`).
        #[serde(default)]
        aspect: Option<String>,
        /// Distinguishes compare panels sharing one page (`vote-compare-form-{suffix}`).
        #[serde(default)]
        dom_suffix: Option<String>,
        #[serde(default = "default_ui_form_action")]
        form_action: String,
    },
    /// Set or clear the garden HUD pin cookie (`slug_garden_pin`). Response is **`303` + `Set-Cookie`** when submitted as a full-navigation form (`data-navigate="full"`), matching `POST /theme`.
    SetGardenPin {
        #[serde(default)]
        clear: bool,
        #[serde(default)]
        room_wire: String,
        #[serde(default)]
        item_storage: Option<String>,
        next: String,
        /// Must equal the form `action` (usually `/ui`). Used to reject forged requests that POST to another path.
        #[serde(default = "default_ui_form_action")]
        form_action: String,
    },
    /// Resolve children or siblings for an external garden item.
    ResolveExternal {
        room_wire: String,
        item_storage: String,
        mode: String,
        next: String,
        #[serde(default = "default_ui_form_action")]
        form_action: String,
    },
    /// Author redacts own post via `POST /ui`.
    RedactPost { post_id: String },
    /// Morph `#room-members-section` — members list open or collapsed (server-rendered).
    SetRoomMembersExpanded {
        room_wire: String,
        #[serde(default)]
        expanded: bool,
    },
    /// Delete the private room (Manage only); redirects to `/` on success.
    DeleteRoom { room: String },
    /// Morph `#new-thread-ui-slot` inner — compose open or collapsed (`room_wire: "public"` for `/t`).
    SetNewThreadComposeExpanded {
        room_wire: String,
        #[serde(default)]
        expanded: bool,
    },
    /// Replace a truncated post card with the full body (same thread index).
    ExpandPostFull {
        room: String,
        thread_tag: String,
        post_index: usize,
    },
    /// Expand a redacted/tombstone post to show stripped body (author view).
    ExpandRedactedPost {
        room: String,
        thread_tag: String,
        post_index: usize,
    },
    /// Collapse an expanded redacted post back to the tombstone card.
    CollapseRedactedPost {
        room: String,
        thread_tag: String,
        post_index: usize,
    },
    /// Copy full thread text (CLI `forum show` format) to the clipboard.
    CopyThread {
        room: String,
        thread_tag: String,
        copy_btn_id: String,
    },
    /// Copy garden child ranking as a concise markdown list to the clipboard.
    CopyGardenRank {
        room: String,
        /// Parent item path (storage or display). Ignored when `external_hosts` is true.
        parent_path: String,
        #[serde(default = "default_garden_rank_depth")]
        depth: usize,
        copy_btn_id: String,
        /// When true, copy rankings for external host roots (`/-/` index), not parent children.
        #[serde(default)]
        external_hosts: bool,
    },
    /// Publish a private-room thread to the public forum (Manage only).
    GraduateThread { room: String, thread_tag: String },
}

fn default_garden_rank_depth() -> usize {
    1
}

#[derive(Debug, Error)]
pub enum HtmlUiParseError {
    #[error("missing __rpc__ field")]
    MissingRpc,
    #[error("invalid template json: {0}")]
    Template(serde_json::Error),
    #[error("invalid ui action: {0}")]
    Action(serde_json::Error),
}

/// Parse `__rpc__` JSON, apply `$form` holes from the rest of the form map, deserialize.
pub fn parse_html_ui_from_form(
    form: &HashMap<String, String>,
) -> Result<HtmlUiAction, HtmlUiParseError> {
    let template = form.get(UI_RPC_FIELD).ok_or(HtmlUiParseError::MissingRpc)?;
    let mut hole_map = form.clone();
    hole_map.remove(UI_RPC_FIELD);
    let v: Value =
        fill_template_from_form(template, &hole_map).map_err(HtmlUiParseError::Template)?;
    serde_json::from_value(v).map_err(HtmlUiParseError::Action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_post_ingest_with_holes() {
        let template = serde_json::json!({
            "action": "post_ingest",
            "room": "public",
            "thread_tag": {"$form": "thread_tag"},
            "text": {"$form": "text"},
            "error_target": "e",
            "form_id": "f",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        form.insert("thread_tag".into(), "x".into());
        form.insert("text".into(), "body".into());

        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::PostIngest {
                room: "public".into(),
                thread_tag: "x".into(),
                text: "body".into(),
                error_target: Some("e".into()),
                form_id: Some("f".into()),
            }
        );
    }

    #[test]
    fn expand_post_full_round_trip() {
        let template = serde_json::json!({
            "action": "expand_post_full",
            "room": "public",
            "thread_tag": "demo",
            "post_index": 3,
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::ExpandPostFull {
                room: "public".into(),
                thread_tag: "demo".into(),
                post_index: 3,
            }
        );
    }

    #[test]
    fn set_room_members_expanded_defaults_false() {
        let template = serde_json::json!({
            "action": "set_room_members_expanded",
            "room_wire": "ab/cd",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::SetRoomMembersExpanded {
                room_wire: "ab/cd".into(),
                expanded: false,
            }
        );
    }

    #[test]
    fn copy_thread_round_trip() {
        let template = serde_json::json!({
            "action": "copy_thread",
            "room": "public",
            "thread_tag": "demo",
            "copy_btn_id": "thread-copy-top",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::CopyThread {
                room: "public".into(),
                thread_tag: "demo".into(),
                copy_btn_id: "thread-copy-top".into(),
            }
        );
    }

    #[test]
    fn set_new_thread_compose_expanded_true() {
        let template = serde_json::json!({
            "action": "set_new_thread_compose_expanded",
            "room_wire": "ab/cd",
            "expanded": true,
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::SetNewThreadComposeExpanded {
                room_wire: "ab/cd".into(),
                expanded: true,
            }
        );
    }

    #[test]
    fn copy_garden_rank_round_trip() {
        let template = serde_json::json!({
            "action": "copy_garden_rank",
            "room": "public",
            "parent_path": "~/topic",
            "depth": 2,
            "copy_btn_id": "garden-rank-copy",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::CopyGardenRank {
                room: "public".into(),
                parent_path: "~/topic".into(),
                depth: 2,
                copy_btn_id: "garden-rank-copy".into(),
                external_hosts: false,
            }
        );
    }

    #[test]
    fn vote_compare_post_round_trip_with_aspect_form_hole() {
        let template = serde_json::json!({
            "action": "vote_compare_post",
            "room": "public",
            "thread_tag": {"$form": "thread_tag"},
            "left_item": "~/a",
            "right_item": "~/b",
            "ratio_left": {"$form": "ratio_left"},
            "ratio_right": {"$form": "ratio_right"},
            "explanation": {"$form": "explanation"},
            "next": "/t/psalms",
            "aspect": {"$form": "aspect"},
            "form_action": "/ui",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        form.insert("thread_tag".into(), "psalms".into());
        form.insert("ratio_left".into(), "2".into());
        form.insert("ratio_right".into(), "1".into());
        form.insert("explanation".into(), "prefer a".into());
        form.insert("aspect".into(), "beauty".into());

        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::VoteComparePost {
                room: "public".into(),
                thread_tag: "psalms".into(),
                left_item: "~/a".into(),
                right_item: "~/b".into(),
                ratio_left: "2".into(),
                ratio_right: "1".into(),
                explanation: "prefer a".into(),
                next: "/t/psalms".into(),
                pool: None,
                aspect: Some("beauty".into()),
                dom_suffix: None,
                form_action: "/ui".into(),
            }
        );
    }

    #[test]
    fn copy_garden_rank_defaults_depth_and_external_hosts() {
        let template = serde_json::json!({
            "action": "copy_garden_rank",
            "room": "public",
            "parent_path": "~/",
            "copy_btn_id": "garden-rank-copy",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(
            a,
            HtmlUiAction::CopyGardenRank {
                room: "public".into(),
                parent_path: "~/".into(),
                depth: 1,
                copy_btn_id: "garden-rank-copy".into(),
                external_hosts: false,
            }
        );
    }
}
