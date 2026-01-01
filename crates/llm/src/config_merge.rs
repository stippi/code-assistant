/// Utilities for merging JSON configurations
///
/// This module provides functionality to merge custom model configurations
/// with base API request payloads using shallow (top-level only) merging.
use serde_json::Value;

/// Shallow merge two JSON values at the top level only.
///
/// The merge behavior is:
/// - For objects: merge only at top level - custom values completely replace base values
/// - For arrays and primitives: `custom` replaces `base`
/// - Keys that only exist in `base` are preserved
///
/// This approach requires custom configs to specify complete sub-objects when overriding
/// nested structures, which is more explicit and avoids unexpected partial merges.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use llm::config_merge::merge_json;
///
/// let base = json!({
///     "temperature": 0.7,
///     "thinking": {
///         "type": "enabled",
///         "budget_tokens": 8192
///     }
/// });
///
/// // To disable thinking, provide the complete thinking object
/// let custom = json!({
///     "temperature": 0.9,
///     "thinking": {
///         "type": "disabled"
///     }
/// });
///
/// let result = merge_json(base, custom);
///
/// assert_eq!(result, json!({
///     "temperature": 0.9,  // overridden
///     "thinking": {
///         "type": "disabled"  // completely replaced (no budget_tokens)
///     }
/// }));
/// ```
pub fn merge_json(mut base: Value, custom: Value) -> Value {
    match (&mut base, custom) {
        // Both are objects: shallow merge at top level only
        (Value::Object(base_map), Value::Object(custom_map)) => {
            for (key, custom_value) in custom_map {
                // Always replace - no recursive merging
                base_map.insert(key, custom_value);
            }
            base
        }
        // For any other combination, custom replaces base
        (_, custom) => custom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_merge_primitives() {
        assert_eq!(merge_json(json!(5), json!(10)), json!(10));
        assert_eq!(merge_json(json!("hello"), json!("world")), json!("world"));
        assert_eq!(merge_json(json!(true), json!(false)), json!(false));
    }

    #[test]
    fn test_merge_arrays() {
        // Arrays are replaced, not merged
        assert_eq!(merge_json(json!([1, 2, 3]), json!([4, 5])), json!([4, 5]));
    }

    #[test]
    fn test_merge_simple_objects() {
        let base = json!({
            "a": 1,
            "b": 2
        });
        let custom = json!({
            "b": 20,
            "c": 3
        });
        let expected = json!({
            "a": 1,   // preserved from base
            "b": 20,  // overridden
            "c": 3    // added from custom
        });
        assert_eq!(merge_json(base, custom), expected);
    }

    #[test]
    fn test_shallow_merge_nested_objects() {
        // Shallow merge: nested objects are completely replaced, not recursively merged
        let base = json!({
            "outer": {
                "inner1": "value1",
                "inner2": "value2"
            },
            "other": "data"
        });
        let custom = json!({
            "outer": {
                "inner2": "overridden",
                "inner3": "new"
            }
        });
        let expected = json!({
            "outer": {
                // inner1 is NOT preserved - entire "outer" object was replaced
                "inner2": "overridden",
                "inner3": "new"
            },
            "other": "data"  // preserved (not in custom)
        });
        assert_eq!(merge_json(base, custom), expected);
    }

    #[test]
    fn test_anthropic_thinking_disabled() {
        // Key use case: disabling thinking without leftover budget_tokens
        let base = json!({
            "model": "claude-sonnet-4",
            "temperature": 0.7,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 8192
            }
        });
        let custom = json!({
            "thinking": {
                "type": "disabled"
            }
        });
        let expected = json!({
            "model": "claude-sonnet-4",
            "temperature": 0.7,
            "thinking": {
                "type": "disabled"
                // No budget_tokens - entire thinking object was replaced
            }
        });
        assert_eq!(merge_json(base, custom), expected);
    }

    #[test]
    fn test_anthropic_thinking_custom_budget() {
        // To customize budget, provide the complete thinking object
        let base = json!({
            "model": "claude-sonnet-4",
            "temperature": 0.7,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 8192
            }
        });
        let custom = json!({
            "thinking": {
                "type": "enabled",
                "budget_tokens": 16384
            }
        });
        let expected = json!({
            "model": "claude-sonnet-4",
            "temperature": 0.7,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 16384
            }
        });
        assert_eq!(merge_json(base, custom), expected);
    }

    #[test]
    fn test_empty_custom_config() {
        let base = json!({"a": 1, "b": 2});
        let custom = json!({});
        assert_eq!(merge_json(base.clone(), custom), base);
    }

    #[test]
    fn test_empty_base_config() {
        let base = json!({});
        let custom = json!({"a": 1, "b": 2});
        assert_eq!(merge_json(base, custom.clone()), custom);
    }

    #[test]
    fn test_shallow_replaces_deeply_nested() {
        // Shallow merge: entire top-level key is replaced
        let base = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "keep": "this",
                        "override": "old"
                    }
                }
            }
        });
        let custom = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "override": "new",
                        "add": "value"
                    }
                }
            }
        });
        // The entire "level1" object is replaced
        let expected = json!({
            "level1": {
                "level2": {
                    "level3": {
                        // "keep" is NOT preserved
                        "override": "new",
                        "add": "value"
                    }
                }
            }
        });
        assert_eq!(merge_json(base, custom), expected);
    }

    #[test]
    fn test_type_replacement() {
        // When types differ, custom replaces base
        let base = json!({"key": {"nested": "object"}});
        let custom = json!({"key": "string"});
        assert_eq!(merge_json(base, custom.clone()), custom);
    }
}
