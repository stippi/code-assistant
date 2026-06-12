//! Formatting a `ToolRequest` back into Caret syntax (format-on-save).

use crate::tools::core::ToolRegistry;
use crate::tools::ToolRequest;
use anyhow::Result;
use serde_json::Value;

/// Format a tool request in Caret syntax.
pub(crate) fn format_tool_request(request: &ToolRequest, registry: &ToolRegistry) -> Result<String> {
    let mut formatted = format!("^^^{}\n", request.name);

    // Get tool spec to understand parameter types and defaults
    let tool_spec = registry
        .get(&request.name)
        .map(|tool| tool.spec())
        .ok_or_else(|| anyhow::anyhow!("Tool '{}' not found in registry", request.name))?;

    if let Value::Object(map) = &request.input {
        // Get required parameters from schema
        let required_params = tool_spec
            .parameters_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();

        // Get parameter properties from schema
        let empty_map = serde_json::Map::new();
        let properties = tool_spec
            .parameters_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap_or(&empty_map);

        for (key, value) in map {
            // Check if this parameter should be omitted (has default value and matches it)
            if let Some(param_schema) = properties.get(key) {
                if let Some(default_value) = param_schema.get("default") {
                    // Skip if the value matches the default and parameter is not required
                    if !required_params.contains(key.as_str()) && value == default_value {
                        continue;
                    }
                }
            }

            // Format the parameter value based on its type
            let param_value = format_parameter_value_for_caret(key, value, properties)?;

            if tool_spec.is_multiline_param(key) {
                formatted.push_str(&format!("{key} ---\n{param_value}\n--- {key}\n"));
            } else {
                formatted.push_str(&format!("{key}: {param_value}\n"));
            }
        }
    }

    formatted.push_str("^^^\n");
    Ok(formatted)
}

/// Format a parameter value for Caret syntax based on its schema type
fn format_parameter_value_for_caret(
    key: &str,
    value: &Value,
    properties: &serde_json::Map<String, Value>,
) -> Result<String> {
    // Check if this is an array parameter
    if let Some(param_schema) = properties.get(key) {
        if let Some(param_type) = param_schema.get("type").and_then(|t| t.as_str()) {
            if param_type == "array" {
                // For arrays in Caret, we use array syntax: [item1, item2, ...]
                if let Value::Array(items) = value {
                    let mut result = String::from("[\n");
                    for item in items {
                        let item_str = match item {
                            Value::String(s) => s.clone(),
                            _ => serde_json::to_string(item)?,
                        };
                        result.push_str(&format!("{item_str}\n"));
                    }
                    result.push(']');
                    return Ok(result);
                }
            }
        }
    }

    // For non-array parameters, use the value as-is
    match value {
        Value::String(s) => Ok(s.clone()),
        _ => Ok(serde_json::to_string(value)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRequest;
    use serde_json::json;

    #[test]
    fn test_caret_formatter_with_array_parameters() {
        let request = ToolRequest {
            id: "test-4".to_string(),
            name: "read_files".to_string(),
            input: json!({
                "project": "test-project",
                "paths": ["file1.txt", "file2.txt"]
            }),
            start_offset: None,
            end_offset: None,
        };

        let expected = concat!(
            "^^^read_files\n",
            "project: test-project\n",
            "paths: [\n",
            "file1.txt\n",
            "file2.txt\n",
            "]\n",
            "^^^\n",
        );

        let result = format_tool_request(&request, &crate::tools::test_registry()).unwrap();

        assert_eq!(expected, result);
    }

    #[test]
    fn test_caret_formatter_omits_default_values() {
        let request = ToolRequest {
            id: "test-5".to_string(),
            name: "write_file".to_string(),
            input: json!({
                "project": "test-project",
                "path": "test.txt",
                "content": "Hello world",
                "append": false // This is the default value, should be omitted
            }),
            start_offset: None,
            end_offset: None,
        };

        let result = format_tool_request(&request, &crate::tools::test_registry()).unwrap();

        // Should not contain the append parameter since it matches the default
        let expected = concat!(
            "^^^write_file\n",
            "project: test-project\n",
            "path: test.txt\n",
            "content ---\n",
            "Hello world\n",
            "--- content\n",
            "^^^\n",
        );

        assert_eq!(expected, result);
    }

    #[test]
    fn test_caret_formatter_includes_non_default_values() {
        let request = ToolRequest {
            id: "test-5".to_string(),
            name: "write_file".to_string(),
            input: json!({
                "project": "test-project",
                "path": "test.txt",
                "content": "Hello world",
                "append": true // This is NOT the default value, should be included
            }),
            start_offset: None,
            end_offset: None,
        };

        let result = format_tool_request(&request, &crate::tools::test_registry()).unwrap();

        // Should not contain the append parameter since it matches the default
        let expected = concat!(
            "^^^write_file\n",
            "project: test-project\n",
            "path: test.txt\n",
            "content ---\n",
            "Hello world\n",
            "--- content\n",
            "append: true\n",
            "^^^\n",
        );

        assert_eq!(expected, result);
    }
}
