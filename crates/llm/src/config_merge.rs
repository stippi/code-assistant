/// Utilities for merging JSON configurations
///
/// This module provides functionality to recursively merge custom model configurations
/// with base API request payloads in a non-destructive way.
use serde_json::Value;

/// Recursively merge two JSON values.
///
/// The merge behavior is:
/// - For objects: recursively merge keys, with `custom` values overriding `base` values
/// - For arrays and primitives: `custom` replaces `base`
/// - Keys that only exist in `base` are preserved
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
/// let custom = json!({
///     "temperature": 0.9,
///     "thinking": {
///         "budget_tokens": 16384
///     },
///     "max_tokens": 4096
/// });
///
/// let result = merge_json(base, custom);
///
/// assert_eq!(result, json!({
///     "temperature": 0.9,  // overridden
///     "thinking": {
///         "type": "enabled",        // preserved from base
///         "budget_tokens": 16384    // overridden
///     },
///     "max_tokens": 4096   // added from custom
/// }));
/// ```
pub fn merge_json(mut base: Value, custom: Value) -> Value {
    match (&mut base, custom) {
        // Both are objects: recursively merge keys
        (Value::Object(base_map), Value::Object(custom_map)) => {
            for (key, custom_value) in custom_map {
                base_map
                    .entry(key)
                    .and_modify(|base_value| {
                        *base_value = merge_json(base_value.clone(), custom_value.clone());
                    })
                    .or_insert(custom_value);
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
    fn test_merge_nested_objects() {
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
                "inner1": "value1",      // preserved
                "inner2": "overridden",  // overridden
                "inner3": "new"          // added
            },
            "other": "data"  // preserved
        });
        assert_eq!(merge_json(base, custom), expected);
    }

    #[test]
    fn test_anthropic_thinking_example() {
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
    fn test_deeply_nested_merge() {
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
        let expected = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "keep": "this",
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
