//! Tool invocation dialects — vertical slices per syntax: everything one
//! dialect needs (parser, formatter, stream processor, prompt docs, tests)
//! lives in its directory. The native default (plain LLM tool calling) ships
//! with the agent core.

pub mod caret;
pub mod system_message;
pub mod xml;

#[cfg(test)]
mod tests;

pub use caret::CaretDialect;
pub use xml::XmlDialect;

use crate::tools::core::ToolRegistry;
use crate::types::ToolSyntax;
use agent_core::ToolDialect;
use anyhow::{anyhow, Result};
use llm::{ContentBlock, Message, MessageContent};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Select the dialect implementation for a session's tool syntax.
pub fn dialect_for(syntax: ToolSyntax) -> Arc<dyn ToolDialect> {
    match syntax {
        ToolSyntax::Native => Arc::new(agent_core::native::NativeDialect),
        ToolSyntax::Xml => Arc::new(XmlDialect),
        ToolSyntax::Caret => Arc::new(CaretDialect),
    }
}

/// The text segments of a message, for invocation sniffing.
pub(crate) fn message_text_segments(message: &Message) -> Vec<&str> {
    match &message.content {
        MessageContent::Text(text) => vec![text.as_str()],
        MessageContent::Structured(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text, .. } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect(),
    }
}

/// Whether the named parameter of the given tool typically spans multiple
/// lines (block syntax in the text dialects).
pub(crate) fn is_multiline_param(
    tool_name: &str,
    param_name: &str,
    registry: &ToolRegistry,
) -> bool {
    registry
        .get(tool_name)
        .map(|tool| tool.spec().is_multiline_param(param_name))
        .unwrap_or(false)
}

/// Example value for a parameter in the prompt docs, taken from the JSON
/// schema's `examples` annotation when present.
pub(crate) fn example_placeholder(name: &str, prop: &serde_json::Value) -> String {
    prop.get("examples")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| format!("{name} here"))
}

/// Convert a raw string-parameter map to a JSON value based on the tool's
/// schema. Shared by the text dialects.
pub fn convert_params_to_json(
    tool_name: &str,
    params: &HashMap<String, Vec<String>>,
    registry: &ToolRegistry,
) -> Result<Value> {
    // Get tool schema from registry
    let tool = registry
        .get(tool_name)
        .ok_or_else(|| anyhow!("Tool {tool_name} not found in registry"))?;

    let tool_spec = tool.spec();
    let schema = &tool_spec.parameters_schema;

    // Create base object
    let mut result = json!({});
    let mut processed_params = std::collections::HashSet::new();

    // Access properties from schema
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (prop_name, prop_schema) in properties {
            // Determine type from schema
            let prop_type = prop_schema
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string");

            match prop_type {
                // For arrays - handle both singular and plural forms
                "array" => {
                    // First try exact property name (including empty arrays)
                    let values_exact = params.get(prop_name);

                    // If not found, try alternative singular/plural form
                    let (values, used_param_name) = if values_exact.is_none() {
                        // Get alternative name (singular/plural form)
                        let alt_name = if prop_name.ends_with('s') {
                            // Remove the 's' at the end for singular form
                            prop_name[0..prop_name.len() - 1].to_string()
                        } else {
                            // Add 's' for plural form
                            format!("{prop_name}s")
                        };

                        if let Some(alt_values) = params.get(&alt_name) {
                            (Some(alt_values), alt_name)
                        } else {
                            (None, prop_name.clone())
                        }
                    } else {
                        (values_exact, prop_name.clone())
                    };

                    // If we have values (including empty arrays), add them as an array
                    if let Some(array_values) = values {
                        result[prop_name] = json!(array_values);
                        processed_params.insert(used_param_name);
                    }
                }
                // For all other types, use normal parameter handling
                _ => {
                    // Skip if parameter not provided
                    if !params.contains_key(prop_name) {
                        continue;
                    }

                    // Get the parameter values
                    let param_values = &params[prop_name];
                    if param_values.is_empty() {
                        continue;
                    }

                    match prop_type {
                        // For boolean convert from string
                        "boolean" => {
                            let bool_value = param_values[0].to_lowercase() == "true";
                            result[prop_name] = json!(bool_value);
                        }
                        // For numbers convert from string
                        "number" => {
                            if let Ok(num) = param_values[0].parse::<f64>() {
                                result[prop_name] = json!(num);
                            } else {
                                return Err(anyhow!(
                                    "Failed to parse '{}' as number for parameter '{}'",
                                    param_values[0],
                                    prop_name
                                ));
                            }
                        }
                        // For integers convert from string
                        "integer" => {
                            if let Ok(num) = param_values[0].parse::<i64>() {
                                result[prop_name] = json!(num);
                            } else {
                                return Err(anyhow!(
                                    "Failed to parse '{}' as integer for parameter '{}'",
                                    param_values[0],
                                    prop_name
                                ));
                            }
                        }
                        // Default to string (first value only)
                        _ => {
                            result[prop_name] = json!(param_values[0]);
                        }
                    }
                    processed_params.insert(prop_name.clone());
                }
            }
        }
    }

    // Add any unprocessed parameters as-is (for backward compatibility with tests)
    for (param_name, param_values) in params {
        if !processed_params.contains(param_name) && !param_values.is_empty() {
            if param_values.len() == 1 {
                result[param_name] = json!(param_values[0]);
            } else {
                result[param_name] = json!(param_values);
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use crate::tools::core::capabilities;
    use crate::tools::core::ResourcesTracker;
    use crate::tools::core::{Tool, ToolContext, ToolSpec};
    use crate::tools::impls::{ListProjectsTool, ReadFilesTool};
    use std::collections::HashMap;

    // Test tool to use for schema parsing tests
    struct TestTool;

    #[async_trait::async_trait]
    impl Tool for TestTool {
        type Input = TestInput;
        type Output = TestOutput;

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "test_tool".into(),
                description: "Test tool for parameter conversion".into(),
                parameters_schema: json!({
                    "type": "object",
                    "properties": {
                        "string_param": {
                            "type": "string",
                            "description": "A string parameter"
                        },
                        "number_param": {
                            "type": "number",
                            "description": "A number parameter"
                        },
                        "integer_param": {
                            "type": "integer",
                            "description": "An integer parameter"
                        },
                        "boolean_param": {
                            "type": "boolean",
                            "description": "A boolean parameter"
                        },
                        "array_param": {
                            "type": "array",
                            "description": "An array parameter",
                            "items": {
                                "type": "string"
                            }
                        }
                    },
                    "required": ["string_param"]
                }),
                annotations: None,
                capabilities: ToolSpec::capabilities(&[capabilities::SCOPE_MCP]),
                multiline_params: &[],
                hidden: false,
                title_template: None,
            }
        }

        async fn execute<'a>(
            &self,
            _context: &mut ToolContext<'a>,
            _input: &mut Self::Input,
        ) -> Result<Self::Output> {
            unimplemented!()
        }
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    struct TestInput {
        string_param: String,
        #[serde(default)]
        number_param: Option<f64>,
        #[serde(default)]
        integer_param: Option<i64>,
        #[serde(default)]
        boolean_param: Option<bool>,
        #[serde(default)]
        array_param: Option<Vec<String>>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct TestOutput;

    impl crate::tools::core::Render for TestOutput {
        fn status(&self) -> String {
            "Test output".to_string()
        }

        fn render(&self, _tracker: &mut ResourcesTracker) -> String {
            "Test output rendered".to_string()
        }
    }

    impl crate::tools::core::ToolResult for TestOutput {
        fn is_success(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_convert_params_to_json() {
        // Create a registry with our test tool
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(TestTool));

        // Create XML-style params
        let mut params = HashMap::new();
        params.insert("string_param".to_string(), vec!["test string".to_string()]);
        params.insert("number_param".to_string(), vec!["42.5".to_string()]);
        params.insert("integer_param".to_string(), vec!["42".to_string()]);
        params.insert("boolean_param".to_string(), vec!["true".to_string()]);
        params.insert(
            "array_param".to_string(),
            vec!["item1".to_string(), "item2".to_string()],
        );

        // Convert to JSON
        let json_params = convert_params_to_json("test_tool", &params, &registry).unwrap();

        // Verify conversion results
        assert_eq!(json_params["string_param"], "test string");
        assert_eq!(json_params["number_param"], 42.5);
        assert_eq!(json_params["integer_param"], 42);
        assert_eq!(json_params["boolean_param"], true);
        assert!(json_params["array_param"].is_array());
        assert_eq!(json_params["array_param"][0], "item1");
        assert_eq!(json_params["array_param"][1], "item2");
    }

    #[tokio::test]
    async fn test_convert_xml_params_error_handling() {
        // Create a registry with our test tool
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(TestTool));

        // Create XML-style params with invalid number
        let mut params = HashMap::new();
        params.insert("string_param".to_string(), vec!["test string".to_string()]);
        params.insert("number_param".to_string(), vec!["not a number".to_string()]);

        // Conversion should fail with a descriptive error
        let result = convert_params_to_json("test_tool", &params, &registry);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse 'not a number' as number"));

        // Test with invalid integer
        let mut params = HashMap::new();
        params.insert("string_param".to_string(), vec!["test string".to_string()]);
        params.insert("integer_param".to_string(), vec!["42.5".to_string()]);

        // Conversion should fail with a descriptive error
        let result = convert_params_to_json("test_tool", &params, &registry);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse '42.5' as integer"));
    }

    #[tokio::test]
    async fn test_convert_xml_params_real_tools() {
        // Create registry with real tools
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ListProjectsTool));
        registry.register(Box::new(ReadFilesTool));

        // Test list_projects (simple case)
        let empty_params = HashMap::new();
        let json_params =
            convert_params_to_json("list_projects", &empty_params, &registry).unwrap();
        assert_eq!(json_params, json!({}));

        // Test read_files with singular "path" in params (XML style)
        let mut params = HashMap::new();
        params.insert("project".to_string(), vec!["test-project".to_string()]);
        params.insert(
            "path".to_string(),
            vec!["file1.txt".to_string(), "file2.txt".to_string()],
        );

        let json_params = convert_params_to_json("read_files", &params, &registry).unwrap();
        assert_eq!(json_params["project"], "test-project");
        assert!(json_params["paths"].is_array()); // Should match "paths" (plural) from schema
        assert_eq!(json_params["paths"].as_array().unwrap().len(), 2);
    }
}
