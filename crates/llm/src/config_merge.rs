//! Merging configuration overlays into API request payloads.
//!
//! Overlays (the request settings a model's reasoning effort implies, and the
//! user's per-model `config` block) are applied as a JSON Merge Patch
//! (RFC 7386): objects merge recursively, `null` removes a key, and any other
//! value replaces what was there. A user can therefore add a single nested
//! field (e.g. `generation_config.temperature`) without dropping its
//! siblings, and remove a field the client sends with `null`.
use serde_json::Value;

/// Apply `patch` to `target` as a JSON Merge Patch (RFC 7386).
///
/// ```
/// use serde_json::json;
/// use llm::config_merge::merge_patch;
///
/// let request = json!({
///     "generation_config": { "temperature": 1.0, "max_output_tokens": 65536 },
///     "text": { "verbosity": "medium" }
/// });
/// let patch = json!({
///     "generation_config": { "temperature": 0.7 },
///     "text": null
/// });
///
/// assert_eq!(
///     merge_patch(request, patch),
///     json!({ "generation_config": { "temperature": 0.7, "max_output_tokens": 65536 } })
/// );
/// ```
pub fn merge_patch(target: Value, patch: Value) -> Value {
    let Value::Object(patch_map) = patch else {
        return patch;
    };
    let mut target_map = match target {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for (key, patch_value) in patch_map {
        if patch_value.is_null() {
            target_map.remove(&key);
        } else {
            let current = target_map.remove(&key).unwrap_or(Value::Null);
            target_map.insert(key, merge_patch(current, patch_value));
        }
    }
    Value::Object(target_map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primitives_and_arrays_are_replaced() {
        assert_eq!(merge_patch(json!(5), json!(10)), json!(10));
        assert_eq!(merge_patch(json!([1, 2, 3]), json!([4, 5])), json!([4, 5]));
        assert_eq!(
            merge_patch(
                json!({"key": {"nested": "object"}}),
                json!({"key": "string"})
            ),
            json!({"key": "string"})
        );
    }

    #[test]
    fn nested_objects_merge_recursively() {
        let target = json!({
            "level1": { "level2": { "keep": "this", "override": "old" } },
            "other": "data"
        });
        let patch = json!({
            "level1": { "level2": { "override": "new", "add": "value" } }
        });
        assert_eq!(
            merge_patch(target, patch),
            json!({
                "level1": { "level2": { "keep": "this", "override": "new", "add": "value" } },
                "other": "data"
            })
        );
    }

    #[test]
    fn null_removes_a_key() {
        let target = json!({
            "thinking": { "type": "enabled", "budget_tokens": 8192 },
            "temperature": 0.7
        });
        let patch = json!({ "thinking": { "budget_tokens": null, "type": "disabled" } });
        assert_eq!(
            merge_patch(target, patch),
            json!({ "thinking": { "type": "disabled" }, "temperature": 0.7 })
        );
    }

    #[test]
    fn object_patch_replaces_a_non_object_target() {
        assert_eq!(
            merge_patch(json!({"a": 1}), json!({"a": {"b": 2, "c": null}})),
            json!({"a": {"b": 2}})
        );
    }

    #[test]
    fn empty_patch_keeps_target() {
        let target = json!({"a": 1, "b": 2});
        assert_eq!(merge_patch(target.clone(), json!({})), target);
    }
}
