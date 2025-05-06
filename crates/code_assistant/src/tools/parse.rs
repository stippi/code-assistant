use crate::tools::core::ToolRegistry;
use crate::types::{FileReplacement, ToolError};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::trace;

pub const TOOL_TAG_PREFIX: &str = "tool:";
const PARAM_TAG_PREFIX: &str = "param:";

/// Represents a parsed path with optional line ranges
#[derive(Debug, Clone)]
pub struct PathWithLineRange {
    pub path: PathBuf,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

impl PathWithLineRange {
    /// Parse a path string that may contain line ranges like "file.txt:10-20"
    pub fn parse(path_str: &str) -> Result<Self, ToolError> {
        // Check if the path contains a colon (not part of Windows drive letter)
        if let Some(colon_pos) = path_str.rfind(':') {
            // Skip Windows drive letter (e.g., C:)
            if colon_pos > 1
                || (colon_pos == 1 && !path_str.chars().next().unwrap().is_alphabetic())
            {
                let (file_path, line_range) = path_str.split_at(colon_pos);
                let line_range = &line_range[1..]; // Skip the colon

                // Parse the line range
                if line_range.is_empty() {
                    // Just a colon with nothing after it, treat as normal path
                    return Ok(Self {
                        path: PathBuf::from(path_str),
                        start_line: None,
                        end_line: None,
                    });
                }

                if let Some(dash_pos) = line_range.find('-') {
                    // Range with dash: file.txt:10-20
                    let (start, end) = line_range.split_at(dash_pos);
                    let end = &end[1..]; // Skip the dash

                    let start_line = if start.is_empty() {
                        None // file.txt:-20
                    } else {
                        Some(start.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid start line number: {}", start))
                        })?)
                    };

                    let end_line = if end.is_empty() {
                        None // file.txt:10-
                    } else {
                        Some(end.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid end line number: {}", end))
                        })?)
                    };

                    return Ok(Self {
                        path: PathBuf::from(file_path),
                        start_line,
                        end_line,
                    });
                } else {
                    // Single line: file.txt:15
                    let line_num = line_range.parse::<usize>().map_err(|_| {
                        ToolError::ParseError(format!("Invalid line number: {}", line_range))
                    })?;

                    return Ok(Self {
                        path: PathBuf::from(file_path),
                        start_line: Some(line_num),
                        end_line: Some(line_num),
                    });
                }
            }
        }

        // No line range specified
        Ok(Self {
            path: PathBuf::from(path_str),
            start_line: None,
            end_line: None,
        })
    }
}

pub fn parse_tool_xml(xml: &str) -> Result<(String, Value), ToolError> {
    trace!("Parsing XML:\n{}", xml);

    let tool_name = xml
        .trim()
        .strip_prefix(&format!("<{}", TOOL_TAG_PREFIX))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.strip_suffix('>'))
        .ok_or_else(|| ToolError::ParseError("Missing tool name".into()))?
        .to_string();

    trace!("Found tool name: {}", tool_name);

    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_param = String::new();
    let mut content_start = 0;

    let mut chars = xml.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '<' {
            // Check for parameter tag
            let rest = &xml[i..];
            trace!("Found '<', rest of string: {}", rest);
            if rest.starts_with(&format!("</{}", PARAM_TAG_PREFIX)) {
                // Closing tag
                let param_name = rest[format!("</{}", PARAM_TAG_PREFIX).len()..] // skip the "</param:"
                    .split('>')
                    .next()
                    .ok_or_else(|| ToolError::ParseError("Invalid closing tag format".into()))?;
                trace!("Found closing tag for: {}", param_name);
                if param_name == current_param {
                    let content = &xml[content_start..i];
                    trace!("Found content for {}: {}", current_param, content);
                    params
                        .entry(current_param.clone())
                        .or_default()
                        .push(content.to_string());
                    current_param.clear();
                }
            } else if let Some(param_start) = rest.strip_prefix(&format!("<{}", PARAM_TAG_PREFIX)) {
                // Opening tag
                if let Some(param_name) = param_start.split('>').next() {
                    current_param = param_name.to_string();
                    content_start = i + format!("<{}{}>", PARAM_TAG_PREFIX, param_name).len();
                    trace!("Found param start: {} at {}", current_param, content_start);
                }
            }
        }
    }

    trace!("Final parameters: {:?}", params);

    // Convert parameters to JSON using the ToolRegistry
    let json_params = convert_xml_params_to_json(&tool_name, &params, &ToolRegistry::global())
        .map_err(|e| {
            ToolError::ParseError(format!("Error converting parameters to JSON: {}", e))
        })?;

    Ok((tool_name, json_params))
}

pub(crate) fn parse_search_replace_blocks(
    content: &str,
) -> Result<Vec<FileReplacement>, ToolError> {
    let mut replacements = Vec::new();
    let mut lines = content.lines().peekable();
    let mut had_valid_block = false;

    // Skip leading empty lines
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }

    // Check first non-empty line is a start marker
    if let Some(line) = lines.peek() {
        let trimmed = line.trim_end();
        if !trimmed.starts_with("<<<<<<< SEARCH") {
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content before diff markers".to_string(),
            ));
        }
    } else {
        // Empty content
        return Err(ToolError::ParseError(
            "Malformed diff: No search/replace blocks found. Expecting content to start with <<<<<<< SEARCH".to_string(),
        ));
    }

    while let Some(line) = lines.next() {
        // Skip empty lines between blocks
        if line.trim().is_empty() {
            continue;
        }

        // Match the exact marker without trimming leading whitespace
        let is_search_all = line.trim_end() == "<<<<<<< SEARCH_ALL";
        let is_search = line.trim_end() == "<<<<<<< SEARCH";

        if is_search || is_search_all {
            had_valid_block = true;
            let mut search = String::new();
            let mut replace = String::new();
            let mut found_separator = false;

            // Collect search content until we find the separator
            while let Some(line) = lines.next() {
                if line.trim_end() == "=======" {
                    found_separator = true;
                    break;
                }
                if !search.is_empty() {
                    search.push('\n');
                }
                search.push_str(line);
            }

            if !found_separator {
                return Err(ToolError::ParseError(
                    "Malformed diff: Missing separator marker (=======)".to_string(),
                ));
            }

            // Collect replace content
            let end_marker = if is_search_all {
                ">>>>>>> REPLACE_ALL"
            } else {
                ">>>>>>> REPLACE"
            };
            let mut found_end_marker = false;

            // Before collecting the replace content, we'll check if there are
            // additional separator markers in the remaining content
            {
                // Clone the iterator to peek ahead without consuming it
                let mut preview_iter = lines.clone();
                let mut lines_to_end_marker = Vec::new();
                let mut reached_end_marker = false;

                // Collect all lines until end marker
                while let Some(line) = preview_iter.next() {
                    if line.trim_end() == end_marker {
                        reached_end_marker = true;
                        break;
                    }
                    lines_to_end_marker.push(line);
                }

                if !reached_end_marker {
                    return Err(ToolError::ParseError(
                        "Malformed diff: Missing closing marker".to_string(),
                    ));
                }

                // Check for invalid separators
                let separator_count = lines_to_end_marker
                    .iter()
                    .filter(|line| line.trim_end() == "=======")
                    .count();

                // Special case: allow one separator if it's the last line before end marker
                if separator_count > 0 {
                    let last_line = lines_to_end_marker.last();

                    if separator_count > 1
                        || (last_line.map_or(false, |line| line.trim_end() != "======="))
                    {
                        return Err(ToolError::ParseError(
                            "Malformed diff: Multiple separator markers (=======) found in the content. This is not allowed as it would make it impossible to edit files containing separators.".to_string(),
                        ));
                    }
                }
            }

            // Now actually process the replace content
            while let Some(current_line) = lines.next() {
                // Check for end marker
                if current_line.trim_end() == end_marker {
                    found_end_marker = true;
                    break;
                }

                // Check if this is a separator right before end marker
                if current_line.trim_end() == "=======" {
                    if let Some(next_line) = lines.peek() {
                        if next_line.trim_end() == end_marker {
                            // Skip this separator if it's right before the end marker
                            continue;
                        }
                    }

                    // This should never happen due to our check above, but just in case
                    return Err(ToolError::ParseError(
                        "Malformed diff: Found separator marker (=======) in replace content. This is not allowed as it would make subsequent edits impossible.".to_string(),
                    ));
                }

                // Regular content line - add to replace content
                if !replace.is_empty() {
                    replace.push('\n');
                }
                replace.push_str(current_line);
            }

            if !found_end_marker {
                return Err(ToolError::ParseError(
                    "Malformed diff: Missing closing marker (>>>>>>> REPLACE)".to_string(),
                ));
            }

            replacements.push(FileReplacement {
                search,
                replace,
                replace_all: is_search_all,
            });
        } else {
            // Found a non-empty line that isn't a start marker
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content between diff blocks".to_string(),
            ));
        }
    }

    // Check for non-whitespace content after all blocks are processed
    while let Some(line) = lines.next() {
        if !line.trim().is_empty() {
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content after diff blocks".to_string(),
            ));
        }
    }

    if !had_valid_block {
        return Err(ToolError::ParseError(
            "Malformed diff: No valid search/replace blocks found".to_string(),
        ));
    }

    Ok(replacements)
}

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

#[cfg(test)]
mod tests {
    use super::super::parse::parse_search_replace_blocks;
    use super::*;
    use crate::tools::core::ResourcesTracker;
    use crate::tools::core::{Tool, ToolContext, ToolScope, ToolSpec};
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
                supported_scopes: &[ToolScope::McpServer],
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

    #[test]
    fn test_parse_search_replace_blocks_normal() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "    return a;\n",
            "}\n",
            "=======\n",
            "if a >= b {\n",
            "    return a;\n",
            "}\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "if a > b {\n    return a;\n}");
        assert_eq!(result[0].replace, "if a >= b {\n    return a;\n}");
        assert_eq!(result[0].replace_all, false);
    }

    #[test]
    fn test_parse_search_replace_blocks_multiple() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "=======\n",
            "if a >= b {\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH\n",
            "return a;\n",
            "=======\n",
            "return a + 1;\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "if a > b {");
        assert_eq!(result[0].replace, "if a >= b {");
        assert_eq!(result[1].search, "return a;");
        assert_eq!(result[1].replace, "return a + 1;");
    }

    #[test]
    fn test_parse_search_replace_blocks_with_second_separator() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "    return a;\n",
            "}\n",
            "=======\n",
            "if a >= b {\n",
            "    // Add a comment\n",
            "=======\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "if a > b {\n    return a;\n}");
        assert_eq!(result[0].replace, "if a >= b {\n    // Add a comment");
    }

    #[test]
    fn test_parse_search_replace_blocks_empty_sections() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "// This comment will be removed\n",
            "=======\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "// This comment will be removed");
        assert_eq!(result[0].replace, "");
    }

    #[test]
    fn test_parse_search_replace_all_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH_ALL\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE_ALL"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "console.log(");
        assert_eq!(result[0].replace, "logger.debug(");
        assert_eq!(result[0].replace_all, true);
    }

    #[test]
    fn test_parse_mixed_search_replace_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "=======\n",
            "function renamed() {\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH_ALL\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE_ALL"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "function test() {");
        assert_eq!(result[0].replace, "function renamed() {");
        assert_eq!(result[0].replace_all, false);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert_eq!(result[1].replace_all, true);
    }

    #[test]
    fn test_parse_multiple_search_replace_blocks_whitespace() {
        let content = concat!(
            "\n",
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "=======\n",
            "function renamed() {\n",
            ">>>>>>> REPLACE\n",
            "\n",
            "<<<<<<< SEARCH\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE",
            "\n",
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "function test() {");
        assert_eq!(result[0].replace, "function renamed() {");
        assert_eq!(result[0].replace_all, false);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert_eq!(result[1].replace_all, false);
    }

    #[test]
    fn test_parse_malformed_diff_with_missing_closing_marker() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "        content to search\n",
            "=======\n",
            "        content to replace with\n",
            "======="
        );

        // The diff is malformed (no closing >>>>>>> marker), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Missing closing marker"),
            "Error should mention the missing closing marker: {}",
            error_message
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_multiple_separators() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "        content to search\n",
            "=======\n",
            "        some more content to search\n",
            "=======\n",
            "        content to replace with\n",
            ">>>>>>> REPLACE\n",
        );

        // The diff is malformed (it has multiple separators), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(
            result.is_err(),
            "Expected an error for malformed diff with multiple separators"
        );
        let error_message = result.unwrap_err().to_string();

        assert!(
            error_message.contains("Multiple separator markers"),
            "Error should mention the problem with multiple separator markers: {}",
            error_message
        );
    }

    #[test]
    fn test_parse_malformed_diff_missing_start_marker() {
        let content = concat!(
            "Some regular content\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE"
        );

        // The diff is malformed (no start <<<<<<< marker), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("content before diff markers"),
            "Error should mention unexpected content: {}",
            error_message
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_content_between_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE\n",
            "Unexpected content between blocks\n",
            "<<<<<<< SEARCH\n",
            "second search\n",
            "=======\n",
            "second replace\n",
            ">>>>>>> REPLACE"
        );

        // The diff is malformed (non-whitespace content between blocks), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Unexpected content between diff blocks"),
            "Error should mention unexpected content between blocks: {}",
            error_message
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_content_after_last_block() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE\n",
            "Unexpected content after the last block"
        );

        // The diff is malformed (non-whitespace content after last block), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();

        // With the current implementation, this is detected as content between blocks
        // since we don't distinguish between "after last block" and "between blocks"
        assert!(
            error_message.contains("Unexpected content between diff blocks"),
            "Error should mention unexpected content: {}",
            error_message
        );
    }
}
