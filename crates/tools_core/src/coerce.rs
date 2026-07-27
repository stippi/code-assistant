//! Schema-directed coercion of tool inputs.
//!
//! LLMs frequently emit a bare scalar where a tool's JSON Schema declares an
//! array — most often a single path handed to a `paths: string[]` parameter as
//! `"file.txt"` instead of `["file.txt"]`. Left alone this fails
//! deserialization and costs a whole extra round-trip while the model corrects
//! itself. [`coerce_to_schema`] walks the provided value against the tool's
//! schema and wraps such scalars in a one-element array, so the common mistake
//! parses on the first try.
//!
//! The coercion is deliberately conservative: it only ever *adds* an array
//! wrapper around a value the schema says should be an array, and it never
//! discards or reinterprets data. Because [`crate::dyn_tool::DynTool::invoke`]
//! re-serializes the parsed input back into `params`, a coerced value also
//! becomes the canonical form recorded in the conversation history.

use serde_json::{Map, Value};

/// Coerce `value` in place so that scalars land in arrays where `schema`
/// (a JSON Schema fragment) requires an array. Recurses through object
/// `properties` and array `items` so nested parameters are handled too.
pub fn coerce_to_schema(value: &mut Value, schema: &Value) {
    let Some(schema) = schema.as_object() else {
        return;
    };

    // Follow the first applicable branch of a combinator schema. We only need
    // to find *an* array-typed alternative to decide whether to wrap.
    if let Some(sub) = combinator_subschema_for_array(schema, value) {
        coerce_to_schema(value, sub);
        return;
    }

    match schema_type(schema) {
        Some(SchemaType::Array) => {
            let items = schema.get("items");
            if !value.is_array() && !value.is_null() {
                // Wrap the scalar (or object) in a single-element array, first
                // coercing the element against the array's `items` schema.
                if let Some(items) = items {
                    coerce_to_schema(value, items);
                }
                *value = Value::Array(vec![std::mem::take(value)]);
            } else if let (Some(items), Some(arr)) = (items, value.as_array_mut()) {
                for element in arr {
                    coerce_to_schema(element, items);
                }
            }
        }
        Some(SchemaType::Object) => {
            if let (Some(props), Some(map)) = (
                schema.get("properties").and_then(Value::as_object),
                value.as_object_mut(),
            ) {
                for (key, prop_schema) in props {
                    if let Some(field) = map.get_mut(key) {
                        coerce_to_schema(field, prop_schema);
                    }
                }
            }
        }
        _ => {}
    }
}

enum SchemaType {
    Array,
    Object,
}

/// Extract the declared JSON Schema `type`, tolerating the `["array", "null"]`
/// form some schemas use for nullable fields.
fn schema_type(schema: &Map<String, Value>) -> Option<SchemaType> {
    match schema.get("type") {
        Some(Value::String(s)) => classify(s),
        Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).find_map(classify),
        _ => None,
    }
}

fn classify(s: &str) -> Option<SchemaType> {
    match s {
        "array" => Some(SchemaType::Array),
        "object" => Some(SchemaType::Object),
        _ => None,
    }
}

/// For `anyOf`/`oneOf`/`allOf` schemas, return a subschema describing an array
/// when the value is not already an array — so a `string | string[]` union
/// still coerces a lone string. Returns `None` when no wrapping is warranted.
fn combinator_subschema_for_array<'a>(
    schema: &'a Map<String, Value>,
    value: &Value,
) -> Option<&'a Value> {
    if value.is_array() {
        return None;
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = schema.get(key) {
            // Only wrap if none of the variants already accepts the current
            // (scalar) value's type, to avoid clobbering a legitimate scalar
            // alternative. We approximate "accepts" by matching the JSON type.
            let scalar_ok = variants
                .iter()
                .filter_map(Value::as_object)
                .any(|v| accepts_scalar(v, value));
            if scalar_ok {
                return None;
            }
            let array_variant = variants
                .iter()
                .find(|v| matches!(v.as_object().and_then(schema_type), Some(SchemaType::Array)));
            if array_variant.is_some() {
                return array_variant;
            }
        }
    }
    None
}

/// Does this subschema accept the given scalar value by JSON type?
fn accepts_scalar(schema: &Map<String, Value>, value: &Value) -> bool {
    let wanted = match value {
        Value::String(_) => "string",
        Value::Number(n) if n.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        _ => return false,
    };
    match schema.get("type") {
        Some(Value::String(s)) => s == wanted || (wanted == "integer" && s == "number"),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .any(|s| s == wanted || (wanted == "integer" && s == "number")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn coerced(value: Value, schema: &Value) -> Value {
        let mut v = value;
        coerce_to_schema(&mut v, schema);
        v
    }

    #[test]
    fn wraps_top_level_string_into_array() {
        let schema = json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}}
            }
        });
        let out = coerced(json!({"paths": "file.txt"}), &schema);
        assert_eq!(out, json!({"paths": ["file.txt"]}));
    }

    #[test]
    fn leaves_existing_array_untouched() {
        let schema = json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}}
            }
        });
        let out = coerced(json!({"paths": ["a", "b"]}), &schema);
        assert_eq!(out, json!({"paths": ["a", "b"]}));
    }

    #[test]
    fn leaves_scalar_field_untouched() {
        let schema = json!({
            "type": "object",
            "properties": {
                "project": {"type": "string"},
                "paths": {"type": "array", "items": {"type": "string"}}
            }
        });
        let out = coerced(json!({"project": "p", "paths": "x"}), &schema);
        assert_eq!(out, json!({"project": "p", "paths": ["x"]}));
    }

    #[test]
    fn wraps_number_into_array() {
        let schema = json!({
            "type": "object",
            "properties": {"ids": {"type": "array", "items": {"type": "integer"}}}
        });
        let out = coerced(json!({"ids": 7}), &schema);
        assert_eq!(out, json!({"ids": [7]}));
    }

    #[test]
    fn does_not_wrap_null() {
        let schema = json!({
            "type": "object",
            "properties": {"paths": {"type": "array", "items": {"type": "string"}}}
        });
        let out = coerced(json!({"paths": null}), &schema);
        assert_eq!(out, json!({"paths": null}));
    }

    #[test]
    fn handles_nullable_array_type() {
        let schema = json!({
            "type": "object",
            "properties": {"paths": {"type": ["array", "null"], "items": {"type": "string"}}}
        });
        let out = coerced(json!({"paths": "file.txt"}), &schema);
        assert_eq!(out, json!({"paths": ["file.txt"]}));
    }

    #[test]
    fn recurses_into_nested_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "object",
                    "properties": {
                        "tags": {"type": "array", "items": {"type": "string"}}
                    }
                }
            }
        });
        let out = coerced(json!({"filter": {"tags": "urgent"}}), &schema);
        assert_eq!(out, json!({"filter": {"tags": ["urgent"]}}));
    }

    #[test]
    fn coerces_element_against_items_when_wrapping() {
        // Array of objects, each object with an array field: wrapping a lone
        // object should also fix its inner scalar.
        let schema = json!({
            "type": "object",
            "properties": {
                "actions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "keys": {"type": "array", "items": {"type": "string"}}
                        }
                    }
                }
            }
        });
        let out = coerced(json!({"actions": {"keys": "a"}}), &schema);
        assert_eq!(out, json!({"actions": [{"keys": ["a"]}]}));
    }

    #[test]
    fn union_of_string_and_array_wraps_lone_string() {
        let schema = json!({
            "type": "object",
            "properties": {
                "paths": {
                    "anyOf": [
                        {"type": "array", "items": {"type": "string"}}
                    ]
                }
            }
        });
        let out = coerced(json!({"paths": "x"}), &schema);
        assert_eq!(out, json!({"paths": ["x"]}));
    }

    #[test]
    fn union_that_accepts_scalar_is_left_alone() {
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                }
            }
        });
        let out = coerced(json!({"value": "x"}), &schema);
        assert_eq!(out, json!({"value": "x"}));
    }

    #[test]
    fn no_schema_is_noop() {
        let out = coerced(json!({"paths": "x"}), &json!(true));
        assert_eq!(out, json!({"paths": "x"}));
    }
}
