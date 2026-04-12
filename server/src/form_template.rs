//! Plan2-style JSON templates with `{"$form": "field_name"}` holes, filled from
//! `application/x-www-form-urlencoded` (or any `String` → `String` map) **before**
//! deserializing into a typed struct.
//!
//! # Wire format
//!
//! Templates are **compact JSON** (`serde_json::to_string`): one line, no pretty
//! printing, strings escaped per JSON rules (`\"`, `\n`, etc.). Embed that string
//! in HTML attributes or text nodes with normal HTML escaping (e.g. maud), not
//! bespoke encodings.
//!
//! # Power vs flat hidden fields
//!
//! A form is always a string→string map. You can fake depth with dotted keys (`a.b.c`),
//! but one structured blob (`__rpc__` = compact JSON) gives you nested objects,
//! arrays, and optional fields without inventing a new naming scheme each time.
//!
//! # Security
//!
//! Substitution runs **before** `serde` into your command type. It does not fix
//! authorization: if the client can replace the hidden `__rpc__` value, they can
//! change the command shape unless you validate (signed blob, server-side session
//! context, or treat the blob as hints only). Same threat model as any hidden field.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// Serialize a value to compact JSON for a hidden `__rpc__` (or similar) field.
pub fn template_json_compact<T: Serialize>(v: &T) -> serde_json::Result<String> {
    serde_json::to_string(v)
}

/// Recursively walk the JSON AST and replace `{"$form": "key"}` with the submitted
/// string for `key` (empty if missing). Other keys are unchanged.
pub fn substitute_form_vars(val: &mut Value, form_data: &HashMap<String, String>) {
    match val {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(Value::String(field_name)) = map.get("$form") {
                    let submitted = form_data
                        .get(field_name.as_str())
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    *val = Value::String(submitted.to_string());
                    return;
                }
            }
            for v in map.values_mut() {
                substitute_form_vars(v, form_data);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                substitute_form_vars(v, form_data);
            }
        }
        _ => {}
    }
}

/// Parse JSON, apply [`substitute_form_vars`], return the mutated value.
pub fn fill_template_from_form(
    template_json: &str,
    form_data: &HashMap<String, String>,
) -> Result<Value, serde_json::Error> {
    let mut v: Value = serde_json::from_str(template_json)?;
    substitute_form_vars(&mut v, form_data);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Demo {
        room: String,
        thread_tag: String,
        nested: Nested,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Nested {
        text: String,
    }

    #[test]
    fn holes_become_strings() {
        let json = r#"{
            "room": "public",
            "thread_tag": {"$form": "tag"},
            "nested": {"text": {"$form": "body"}}
        }"#;
        let mut form = HashMap::new();
        form.insert("tag".into(), "foo".into());
        form.insert("body".into(), "hello\nworld".into());

        let v = fill_template_from_form(json, &form).unwrap();
        let d: Demo = serde_json::from_value(v).unwrap();
        assert_eq!(
            d,
            Demo {
                room: "public".into(),
                thread_tag: "foo".into(),
                nested: Nested {
                    text: "hello\nworld".into(),
                },
            }
        );
    }

    #[test]
    fn missing_form_key_is_empty_string() {
        let json = r#"{"x": {"$form": "nope"}}"#;
        let mut form = HashMap::new();
        form.insert("other".into(), "y".into());
        let v = fill_template_from_form(json, &form).unwrap();
        assert_eq!(v["x"], "");
    }

    #[test]
    fn array_of_holes() {
        let json = r#"{"items": [{"$form": "a"}, {"$form": "b"}]}"#;
        let mut form = HashMap::new();
        form.insert("a".into(), "1".into());
        form.insert("b".into(), "2".into());
        let v = fill_template_from_form(json, &form).unwrap();
        assert_eq!(v["items"], serde_json::json!(["1", "2"]));
    }

    #[test]
    fn template_json_compact_escapes_and_single_line() {
        let s = template_json_compact(&serde_json::json!({
            "x": "quote\"and\nnewline"
        }))
        .unwrap();
        assert!(!s.contains('\n'));
        assert!(s.contains("\\\"") || s.contains("\\n"));
    }
}
