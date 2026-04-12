//! Browser-only UI commands: JSON in hidden `__rpc__` plus hole fill ([`crate::form_template`]).
//! Not part of [`slug_types::RpcCommand`] (CLI / JSON API).

use crate::form_template::fill_template_from_form;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

/// Form field name for the compact JSON template (possibly with `{"$form":"…"}` holes).
pub const UI_RPC_FIELD: &str = "__rpc__";

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
    /// Author redacts own post via `POST /ui`.
    RedactPost {
        post_id: String,
    },
    /// Morph `#public-new-thread-ui-slot` to the new-thread form (or login hint).
    ExpandPublicNewThreadForm,
    /// Morph `#room-new-thread-ui-slot` for the given room wire id.
    ExpandRoomNewThreadForm {
        room_wire: String,
    },
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
pub fn parse_html_ui_from_form(form: &HashMap<String, String>) -> Result<HtmlUiAction, HtmlUiParseError> {
    let template = form
        .get(UI_RPC_FIELD)
        .ok_or(HtmlUiParseError::MissingRpc)?;
    let mut hole_map = form.clone();
    hole_map.remove(UI_RPC_FIELD);
    let v: Value = fill_template_from_form(template, &hole_map).map_err(HtmlUiParseError::Template)?;
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
    fn expand_public_unit_variant() {
        let template = serde_json::json!({ "action": "expand_public_new_thread_form" });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(a, HtmlUiAction::ExpandPublicNewThreadForm);
    }
}
