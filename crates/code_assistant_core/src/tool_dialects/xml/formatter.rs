//! Formatting a `ToolRequest` back into XML syntax (format-on-save).

use crate::tools::core::ToolRegistry;
use crate::tools::ToolRequest;
use anyhow::Result;
use serde_json::Value;

/// Format a tool request in XML syntax.
pub(crate) fn format_tool_request(request: &ToolRequest, registry: &ToolRegistry) -> Result<String> {
    let mut formatted = format!("<tool:{}>\n", request.name);

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

            // Check if this is an array parameter and handle it specially
            if let Some(param_schema) = properties.get(key) {
                if let Some(param_type) = param_schema.get("type").and_then(|t| t.as_str()) {
                    if param_type == "array" {
                        // For arrays in XML, we repeat the parameter tag for each item
                        if let Value::Array(items) = value {
                            for item in items {
                                let item_str = match item {
                                    Value::String(s) => s.clone(),
                                    _ => serde_json::to_string(item)?,
                                };

                                // Use singular form of the parameter name for XML tags
                                let singular_name = if key.ends_with('s') && key.len() > 1 {
                                    &key[..key.len() - 1]
                                } else {
                                    key
                                };

                                if tool_spec.is_multiline_param(key) {
                                    formatted.push_str(&format!(
                                        "<param:{singular_name}>\n{item_str}\n</param:{singular_name}>\n"
                                    ));
                                } else {
                                    formatted.push_str(&format!(
                                        "<param:{singular_name}>{item_str}</param:{singular_name}>\n"
                                    ));
                                }
                            }
                        }
                        continue; // Skip the normal parameter processing for arrays
                    }
                }
            }

            // For non-array parameters, use normal formatting
            let param_value = match value {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string(value)?,
            };

            if tool_spec.is_multiline_param(key) {
                formatted.push_str(&format!("<param:{key}>\n{param_value}\n</param:{key}>\n"));
            } else {
                formatted.push_str(&format!("<param:{key}>{param_value}</param:{key}>\n"));
            }
        }
    }

    formatted.push_str(&format!("</tool:{}>\n", request.name));
    Ok(formatted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRequest;
    use serde_json::json;

    #[test]
    fn test_xml_formatter_with_array_parameters() {
        let request = ToolRequest {
            id: "test-1".to_string(),
            name: "read_files".to_string(),
            input: json!({
                "project": "test-project",
                "paths": ["file1.txt", "file2.txt", "file3.txt"]
            }),
            start_offset: None,
            end_offset: None,
        };

        let expected = concat!(
            "<tool:read_files>\n",
            "<param:project>test-project</param:project>\n",
            "<param:path>file1.txt</param:path>\n",
            "<param:path>file2.txt</param:path>\n",
            "<param:path>file3.txt</param:path>\n",
            "</tool:read_files>\n"
        );

        let result = format_tool_request(&request, &crate::tools::test_registry()).unwrap();

        assert_eq!(expected, result);
    }

    #[test]
    fn test_xml_formatter_omits_default_values() {
        let request = ToolRequest {
            id: "test-2".to_string(),
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

        // Should not contain the append parameter since it matches the default
        let expected = concat!(
            "<tool:write_file>\n",
            "<param:project>test-project</param:project>\n",
            "<param:path>test.txt</param:path>\n",
            "<param:content>\n",
            "Hello world\n",
            "</param:content>\n",
            "</tool:write_file>\n",
        );

        let result = format_tool_request(&request, &crate::tools::test_registry()).unwrap();

        assert_eq!(expected, result);
    }

    #[test]
    fn test_xml_formatter_includes_non_default_values() {
        let request = ToolRequest {
            id: "test-3".to_string(),
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

        // Should contain the append parameter since it's not the default
        let expected = concat!(
            "<tool:write_file>\n",
            "<param:project>test-project</param:project>\n",
            "<param:path>test.txt</param:path>\n",
            "<param:content>\n",
            "Hello world\n",
            "</param:content>\n",
            "<param:append>true</param:append>\n",
            "</tool:write_file>\n",
        );

        let result = format_tool_request(&request, &crate::tools::test_registry()).unwrap();

        assert_eq!(expected, result);
    }
}
