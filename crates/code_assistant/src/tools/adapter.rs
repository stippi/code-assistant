use crate::config::ProjectManager;
use crate::tools::core::{ResourcesTracker, ToolContext, ToolRegistry};
use crate::types::{Tool, ToolResult};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// Convert XML parameter HashMap to JSON Value based on tool schema
pub fn convert_xml_params_to_json(
    tool_name: &str,
    params: &HashMap<String, Vec<String>>,
    registry: &ToolRegistry,
) -> Result<Value> {
    // Get tool schema from registry
    let tool = registry
        .get(tool_name)
        .ok_or_else(|| anyhow!("Tool {} not found in registry", tool_name))?;

    let tool_spec = tool.spec();
    let schema = &tool_spec.parameters_schema;

    // Create base object
    let mut result = json!({});

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
                    // First try exact property name
                    let values_exact = params.get(prop_name).filter(|v| !v.is_empty());

                    // If not found, try alternative singular/plural form
                    let values = if values_exact.is_none() {
                        // Get alternative name (singular/plural form)
                        let alt_name = if prop_name.ends_with('s') {
                            // Remove the 's' at the end for singular form
                            prop_name[0..prop_name.len() - 1].to_string()
                        } else {
                            // Add 's' for plural form
                            format!("{}s", prop_name)
                        };

                        params.get(&alt_name).filter(|v| !v.is_empty())
                    } else {
                        values_exact
                    };

                    // If we have values, add them as an array
                    if let Some(array_values) = values {
                        result[prop_name] = json!(array_values);
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
                }
            }
        }
    }

    Ok(result)
}

/// Converts a tool result from the new system to the legacy ToolResult enum
fn convert_to_legacy_result(
    tool: &Tool,
    result: Box<dyn crate::tools::core::AnyOutput>,
) -> Result<ToolResult> {
    let mut tracker = ResourcesTracker::new();
    let output = result.as_render().render(&mut tracker);

    match tool {
        Tool::ListProjects => {
            use crate::config;
            // For ListProjects, we need to parse the output or directly access the projects
            let projects = config::load_projects()?;
            Ok(ToolResult::ListProjects { projects })
        }
        Tool::ReadFiles { project, paths } => {
            // For ReadFiles, extract the output from the rendered result
            // This is a simplification - in a full implementation we would
            // need more direct access to the output data

            // Since we can't directly access the ReadFilesOutput struct fields,
            // we'll extract the projects and paths from the original tool invocation
            let project = project.clone();
            let mut loaded_files = HashMap::new();
            let mut failed_files = Vec::new();

            // Parse the output to populate loaded_files and failed_files
            // This is not ideal but serves as a bridge during transition
            for line in output.lines() {
                if line.starts_with(">>>>> FILE: ") {
                    let file_path = line.trim_start_matches(">>>>> FILE: ");
                    if !file_path.contains("(content shown in another tool invocation)") {
                        // Simplified approach - would need more robust parsing in practice
                        let path = PathBuf::from(file_path);
                        // Get content from subsequent lines up to <<<<< END FILE
                        // This is a simplification
                        loaded_files.insert(path, "Content extracted from output".to_string());
                    }
                } else if line.starts_with("Failed to load '") {
                    // Extract failed file path and error message
                    // Simplified implementation
                    let parts: Vec<&str> = line.splitn(2, "': ").collect();
                    if parts.len() == 2 {
                        let path_str = parts[0].trim_start_matches("Failed to load '");
                        let path = PathBuf::from(path_str);
                        let error = parts[1].to_string();
                        failed_files.push((path, error));
                    }
                }
            }

            Ok(ToolResult::ReadFiles {
                project,
                loaded_files,
                failed_files,
            })
        }
        // Other conversion cases will be added as we implement more tools
        _ => Err(anyhow!("Unsupported tool type for adapter: {:?}", tool)),
    }
}

/// Execute a legacy Tool using the new system
pub async fn execute_with_new_system<'a>(
    tool: &Tool,
    project_manager: Box<dyn ProjectManager>,
    working_memory: Option<&'a mut crate::types::WorkingMemory>,
) -> Result<ToolResult> {
    // Create tool context
    let mut context = ToolContext {
        project_manager,
        working_memory,
    };

    // Get the tool registry
    let registry = ToolRegistry::global();

    match tool {
        Tool::ListProjects => {
            if let Some(list_projects_tool) = registry.get("list_projects") {
                // Empty parameters for list_projects
                let params = json!({});

                // Execute the tool
                let result = list_projects_tool.invoke(&mut context, params).await?;

                // Convert the result back to the old format
                convert_to_legacy_result(tool, result)
            } else {
                Err(anyhow!("list_projects tool not found in registry"))
            }
        }
        Tool::ReadFiles { project, paths } => {
            if let Some(read_files_tool) = registry.get("read_files") {
                // Convert paths to strings for the new tool format
                let path_strings: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();

                // Prepare parameters
                let params = json!({
                    "project": project,
                    "paths": path_strings
                });

                // Execute the tool
                let result = read_files_tool.invoke(&mut context, params).await?;

                // Convert the result back to the old format
                convert_to_legacy_result(tool, result)
            } else {
                Err(anyhow!("read_files tool not found in registry"))
            }
        }
        // Other tool mappings will be added as we implement more tools
        _ => Err(anyhow!(
            "Tool not yet implemented in new system: {:?}",
            tool
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::{Tool, ToolContext, ToolMode, ToolSpec};
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
                name: "test_tool",
                description: "Test tool for parameter conversion",
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
                supported_modes: &[ToolMode::McpServer],
            }
        }

        async fn execute<'a>(
            &self,
            _context: &mut ToolContext<'a>,
            _input: Self::Input,
        ) -> Result<Self::Output> {
            unimplemented!()
        }
    }

    #[derive(serde::Deserialize)]
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

    struct TestOutput;

    impl crate::tools::core::Render for TestOutput {
        fn status(&self) -> String {
            "Test output".to_string()
        }

        fn render(&self, _tracker: &mut ResourcesTracker) -> String {
            "Test output rendered".to_string()
        }
    }

    #[tokio::test]
    async fn test_convert_xml_params_to_json() {
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
        let json_params = convert_xml_params_to_json("test_tool", &params, &registry).unwrap();

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
        let result = convert_xml_params_to_json("test_tool", &params, &registry);
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
        let result = convert_xml_params_to_json("test_tool", &params, &registry);
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
            convert_xml_params_to_json("list_projects", &empty_params, &registry).unwrap();
        assert_eq!(json_params, json!({}));

        // Test read_files with singular "path" in params (XML style)
        let mut params = HashMap::new();
        params.insert("project".to_string(), vec!["test-project".to_string()]);
        params.insert(
            "path".to_string(),
            vec!["file1.txt".to_string(), "file2.txt".to_string()],
        );

        let json_params = convert_xml_params_to_json("read_files", &params, &registry).unwrap();
        assert_eq!(json_params["project"], "test-project");
        assert!(json_params["paths"].is_array()); // Should match "paths" (plural) from schema
        assert_eq!(json_params["paths"].as_array().unwrap().len(), 2);
    }
}
