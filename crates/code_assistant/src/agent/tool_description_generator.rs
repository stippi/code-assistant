use crate::tools::core::{ToolRegistry, ToolScope};
use serde_json::Value;

/// Tool syntax for documentation generation
#[derive(Clone, Copy)]
pub enum DocumentationSyntax {
    Xml,
    Caret,
}

/// Convert a parameter value to documentation text based on its JSON Schema
fn format_parameter_doc(name: &str, param: &Value, is_in_required_list: bool) -> String {
    let mut doc = format!("- {}", name);

    // Check if we should mark the parameter as required
    let needs_required_flag = is_in_required_list
        || match param.get("description") {
            Some(description) => description.as_str().unwrap_or("").contains("(required)"),
            None => false,
        };

    // Add required flag if needed
    if needs_required_flag {
        doc.push_str(" (required)");
    }

    // Add description if available
    if let Some(description) = param.get("description") {
        if let Some(desc_str) = description.as_str() {
            // Remove any (required) markers from the description since we handle it separately
            let desc_str = desc_str.replace("(required)", "").trim().to_string();
            doc.push_str(&format!(": {}", desc_str));
        }
    }

    doc
}

/// Generate parameter documentation for a tool
fn generate_parameters_doc(parameters: &Value) -> String {
    let mut docs = Vec::new();

    // Get the properties object
    if let Some(properties) = parameters.get("properties").and_then(|p| p.as_object()) {
        // Get required fields array
        let required_fields = parameters
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
            .unwrap_or_default();

        // Process each parameter
        for (name, param) in properties {
            let is_required = required_fields.contains(&name.as_str());
            docs.push(format_parameter_doc(name, param, is_required));
        }
    }

    docs.join("\n")
}

/// Generate a parameter example based on its type and name
fn generate_parameter_example(example: &mut String, name: &str, prop: &Value) {
    // Determine if this parameter is an array
    let is_array = prop.get("type").and_then(|t| t.as_str()) == Some("array");

    // For array parameters, we always use the singular form in the XML tags
    let param_name = if is_array && name.ends_with('s') {
        // Simple singular conversion by removing trailing 's'
        // A more sophisticated implementation could use a proper pluralization library
        &name[..name.len() - 1]
    } else {
        name
    };

    // Check if this is a multiline content parameter
    let is_multiline =
        name == "content" || name == "command_line" || name == "diff" || name == "message";

    // Generate appropriate placeholder text
    let placeholder = if is_multiline {
        format!("\nYour {} here\n", name)
    } else if name == "project" {
        "project-name".to_string()
    } else if name == "path" || name == "paths" {
        "File path here".to_string()
    } else if name == "regex" {
        "Your regex pattern here".to_string()
    } else if name == "command_line" {
        "Your command here".to_string()
    } else if name == "working_dir" {
        "Working directory here (optional)".to_string()
    } else if name == "url" {
        "https://example.com/docs".to_string()
    } else if name == "query" {
        "Your search query here".to_string()
    } else if name == "hits_page_number" {
        "1".to_string()
    } else if name == "max_depth" {
        "level (optional)".to_string()
    } else {
        format!("{} here", name)
    };

    // Add the parameter to the example
    example.push_str(&format!(
        "<param:{}>{}</param:{}>\n",
        param_name, placeholder, param_name
    ));

    // For array types, add a second example parameter to show multiple items
    if is_array {
        example.push_str(&format!(
            "<param:{}>Another {} here</param:{}>\n",
            param_name, name, param_name
        ));
    }
}

/// Generate usage example for a tool
fn generate_usage_example(tool_name: &str, parameters: &Value) -> String {
    let mut example = format!("<tool:{}>\n", tool_name);

    // Get the properties object
    if let Some(properties) = parameters.get("properties").and_then(|p| p.as_object()) {
        // Get required fields array
        let required_fields = parameters
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
            .unwrap_or_default();

        // Add required parameters first
        for (name, prop) in properties
            .iter()
            .filter(|(name, _)| required_fields.contains(&name.as_str()))
        {
            generate_parameter_example(&mut example, name, prop);
        }

        // Then add optional parameters
        for (name, prop) in properties
            .iter()
            .filter(|(name, _)| !required_fields.contains(&name.as_str()))
        {
            generate_parameter_example(&mut example, name, prop);
        }
    }

    example.push_str(&format!("</tool:{}>\n", tool_name));
    example
}

/// Generate tool documentation for the system message
pub fn generate_tool_documentation(scope: ToolScope) -> String {
    let registry = ToolRegistry::global();
    let tool_defs = registry.get_tool_definitions_for_scope(scope);

    let mut docs = String::new();

    for tool in tool_defs {
        // Skip tools with no parameters
        if !tool
            .parameters
            .get("properties")
            .map_or(false, |p| p.is_object())
        {
            continue;
        }

        // Tool header
        docs.push_str(&format!("## {}\n", tool.name));
        docs.push_str(&format!("Description: {}\n", tool.description));

        // Tool parameters
        docs.push_str("Parameters:\n");
        docs.push_str(&generate_parameters_doc(&tool.parameters));
        docs.push_str("\n");

        // Tool usage
        docs.push_str("Usage:\n");
        docs.push_str(&generate_usage_example(&tool.name, &tool.parameters));
        docs.push_str("\n");
    }

    docs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_format_parameter_doc() {
        let param = json!({
            "type": "string",
            "description": "Path to the file"
        });

        // Parameter not in required list and not marked as required in description
        let result = format_parameter_doc("path", &param, false);
        assert_eq!(result, "- path: Path to the file");

        // Parameter in required list but not marked as required in description
        let result = format_parameter_doc("path", &param, true);
        assert_eq!(result, "- path (required): Path to the file");

        // Parameter marked as required in description but not in required list
        let required_param = json!({
            "type": "string",
            "description": "Project name (required)"
        });

        let result = format_parameter_doc("project", &required_param, false);
        assert_eq!(result, "- project (required): Project name");

        // Parameter both in required list and marked as required in description
        let result = format_parameter_doc("project", &required_param, true);
        assert_eq!(result, "- project (required): Project name");
    }

    #[test]
    fn test_generate_parameters_doc() {
        let params = json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": "string",
                    "description": "Name of the project"
                },
                "path": {
                    "type": "string",
                    "description": "Path to the file"
                }
            },
            "required": ["project"]
        });

        let result = generate_parameters_doc(&params);
        assert!(result.contains("- project (required): Name of the project"));
        assert!(result.contains("- path: Path to the file"));
    }

    #[test]
    fn test_generate_usage_example() {
        // Test with required parameters
        let params = json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": "string",
                    "description": "Name of the project"
                },
                "path": {
                    "type": "string",
                    "description": "Path to the file"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                },
                "append": {
                    "type": "boolean",
                    "description": "Whether to append to the file"
                }
            },
            "required": ["project", "path", "content"]
        });

        let result = generate_usage_example("write_file", &params);

        // Required parameters should come first
        assert!(result.contains("<tool:write_file>"));
        assert!(result.contains("<param:project>project-name</param:project>"));
        assert!(result.contains("<param:path>File path here</param:path>"));
        assert!(result.contains("<param:content>\nYour content here\n</param:content>"));

        // Optional parameter should be present but after required ones
        assert!(result.contains("<param:append>append here</param:append>"));
        assert!(result.contains("</tool:write_file>"));

        // Test with array parameter
        let array_params = json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": "string",
                    "description": "Name of the project"
                },
                "paths": {
                    "type": "array",
                    "description": "Paths to the files",
                    "items": {
                        "type": "string"
                    }
                }
            },
            "required": ["project", "paths"]
        });

        let result = generate_usage_example("list_files", &array_params);

        assert!(result.contains("<tool:list_files>"));
        assert!(result.contains("<param:project>project-name</param:project>"));

        // For array parameters, we use the singular form in tags
        assert!(result.contains("<param:path>File path here</param:path>"));
        assert!(result.contains("<param:path>Another paths here</param:path>"));
        assert!(result.contains("</tool:list_files>"));
    }
}
