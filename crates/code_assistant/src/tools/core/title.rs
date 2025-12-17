//! Tool title generation utilities
//!
//! This module provides functions for generating human-readable tool titles
//! from tool specs and their parameters. Used by ACP mode and sub-agent output rendering.

use crate::tools::core::ToolRegistry;
use std::collections::HashMap;

/// Maximum length for title parameter values before truncation
const MAX_TITLE_LENGTH: usize = 50;

/// Generate a tool title from a tool's title_template and its parameters.
///
/// Returns the generated title if template substitution was successful,
/// otherwise returns None (caller should fall back to tool name or other default).
pub fn generate_tool_title(
    tool_name: &str,
    parameters: &HashMap<String, String>,
) -> Option<String> {
    let registry = ToolRegistry::global();
    let tool = registry.get(tool_name)?;
    let spec = tool.spec();
    let template = spec.title_template?;

    generate_title_from_template(template, parameters)
}

/// Generate a title by substituting {parameter_name} placeholders in the template.
///
/// Returns Some(title) if at least one substitution was made, None otherwise.
pub fn generate_title_from_template(
    template: &str,
    parameters: &HashMap<String, String>,
) -> Option<String> {
    let mut result = template.to_string();
    let mut has_substitution = false;

    // Find all {parameter_name} patterns and replace them
    let re = regex::Regex::new(r"\{([^}]+)\}").ok()?;

    result = re
        .replace_all(&result, |caps: &regex::Captures| {
            let param_name = &caps[1];
            if let Some(param_value) = parameters.get(param_name) {
                let formatted_value = format_parameter_for_title(param_value);
                if !formatted_value.trim().is_empty() {
                    has_substitution = true;
                    formatted_value
                } else {
                    caps[0].to_string() // Keep placeholder if value is empty
                }
            } else {
                caps[0].to_string() // Keep placeholder if parameter not found
            }
        })
        .to_string();

    // Only return the new title if we actually made substitutions
    if has_substitution {
        Some(result)
    } else {
        None
    }
}

/// Format a parameter value for display in a title.
///
/// Handles JSON arrays (showing first item + count), truncates long values, etc.
pub fn format_parameter_for_title(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Try to parse as JSON and extract meaningful parts
    let formatted = if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        match json_val {
            serde_json::Value::Array(arr) if !arr.is_empty() => {
                let first = arr[0].as_str().unwrap_or("...").to_string();
                if arr.len() > 1 {
                    format!("{} and {} more", first, arr.len() - 1)
                } else {
                    first
                }
            }
            serde_json::Value::String(s) => s,
            _ => trimmed.to_string(),
        }
    } else {
        trimmed.to_string()
    };

    // Truncate if needed
    if formatted.len() > MAX_TITLE_LENGTH {
        format!(
            "{}...",
            formatted.chars().take(MAX_TITLE_LENGTH).collect::<String>()
        )
    } else {
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_parameter_simple() {
        assert_eq!(format_parameter_for_title("hello"), "hello");
        assert_eq!(format_parameter_for_title("  hello  "), "hello");
        assert_eq!(format_parameter_for_title(""), "");
    }

    #[test]
    fn test_format_parameter_json_array() {
        assert_eq!(format_parameter_for_title(r#"["file1.txt"]"#), "file1.txt");
        assert_eq!(
            format_parameter_for_title(r#"["file1.txt", "file2.txt", "file3.txt"]"#),
            "file1.txt and 2 more"
        );
    }

    #[test]
    fn test_format_parameter_truncation() {
        let long_value = "a".repeat(100);
        let result = format_parameter_for_title(&long_value);
        assert!(result.len() <= MAX_TITLE_LENGTH + 3); // +3 for "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_generate_title_from_template() {
        let mut params = HashMap::new();
        params.insert("path".to_string(), "src/main.rs".to_string());

        let result = generate_title_from_template("Editing {path}", &params);
        assert_eq!(result, Some("Editing src/main.rs".to_string()));
    }

    #[test]
    fn test_generate_title_no_substitution() {
        let params = HashMap::new();
        let result = generate_title_from_template("Editing {path}", &params);
        assert_eq!(result, None); // No substitution made
    }
}
