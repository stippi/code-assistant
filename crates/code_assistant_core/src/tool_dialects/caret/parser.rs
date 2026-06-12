//! Parsing Caret (triple-caret fenced) tool invocations out of LLM
//! response text.

use crate::tool_dialects::convert_params_to_json;
use crate::tools::core::ToolRegistry;
use crate::tools::tool_use_filter::ToolUseFilter;
use crate::tools::ToolRequest;
use crate::types::ToolError;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

pub fn parse_caret_tool_invocations(
    text: &str,
    request_id: u64,
    _start_tool_count: usize,
    filter: Option<&dyn ToolUseFilter>,
    registry: &ToolRegistry,
) -> Result<(Vec<ToolRequest>, String)> {
    let mut tool_requests = Vec::new();
    let tool_regex = regex::Regex::new(r"(?m)^\^\^\^([a-zA-Z0-9_]+)$").unwrap();
    let multiline_start_regex = regex::Regex::new(r"(?m)^([a-zA-Z0-9_]+)\s+---\s*$").unwrap();
    let multiline_end_regex = regex::Regex::new(r"(?m)^---\s+([a-zA-Z0-9_]+)\s*$").unwrap();
    let tool_end_regex = regex::Regex::new(r"(?m)^\^\^\^$").unwrap();

    let mut remaining_text = text;
    let mut truncation_pos = text.len(); // Default to end of text
    let mut current_offset = 0; // Track position in original text

    while let Some(tool_match) = tool_regex.find(remaining_text) {
        // Calculate absolute positions in original text
        let tool_start_abs = current_offset + tool_match.start();

        // Extract tool name
        let tool_name = tool_regex
            .captures(&remaining_text[tool_match.start()..])
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
            .ok_or_else(|| ToolError::ParseError("Failed to extract tool name".to_string()))?;

        // Check filter before processing this tool
        let tool_index = tool_requests.len() + 1;
        if let Some(filter) = filter {
            if !filter.allow_tool_at_position(tool_name, tool_index) {
                // Tool not allowed, keep existing truncation_pos
                break;
            }
        }

        // Check if the tool exists in the registry
        if registry.get(tool_name).is_none() {
            return Err(ToolError::UnknownTool(tool_name.to_string()).into());
        }

        // Find the end of this tool block
        let tool_start = tool_match.end();
        let remaining_after_tool = &remaining_text[tool_start..];

        let (tool_content, tool_end_pos, tool_end_abs) =
            if let Some(end_match) = tool_end_regex.find(remaining_after_tool) {
                let content = &remaining_after_tool[..end_match.start()];
                let end_pos = tool_start + end_match.end();
                let end_abs = current_offset + end_pos;
                (content, Some(end_pos), end_abs)
            } else {
                // No end found, treat rest as tool content
                let end_abs = text.len();
                (remaining_after_tool, None, end_abs)
            };

        // Parse parameters from tool content (returns raw HashMap)
        let raw_params = parse_caret_tool_parameters_raw(
            tool_content,
            &multiline_start_regex,
            &multiline_end_regex,
        )?;

        // Convert parameters to JSON using schema-based approach if tool exists, otherwise use fallback
        let tool_params = if registry.get(tool_name).is_some() {
            // Use schema-based conversion for registered tools
            convert_params_to_json(tool_name, &raw_params, registry).map_err(|e| {
                ToolError::ParseError(format!("Error converting caret parameters to JSON: {e}"))
            })?
        } else {
            // Fallback to legacy conversion for unregistered tools (mainly for tests)
            convert_raw_params_to_json_fallback(&raw_params)
        };

        // Generate a unique tool ID: tool-<request_id>-<tool_index_in_request>
        let tool_id = format!("tool-{request_id}-{tool_index}");

        // Create a ToolRequest with offset information
        let tool_request = ToolRequest {
            id: tool_id,
            name: tool_name.to_string(),
            input: tool_params,
            start_offset: Some(tool_start_abs),
            end_offset: Some(tool_end_abs),
        };

        tool_requests.push(tool_request);

        // Update remaining text for next iteration
        if let Some(end_pos) = tool_end_pos {
            // Always update truncation position after each successfully processed tool
            // This ensures we can truncate cleanly after the last allowed tool
            let absolute_tool_end = current_offset + end_pos;
            truncation_pos = absolute_tool_end;

            // Check if we should allow content after this tool
            if let Some(filter) = filter {
                if !filter.allow_content_after_tool(tool_name, tool_index) {
                    // No content allowed after this tool, truncate here
                    break;
                }
            }

            current_offset += end_pos;
            remaining_text = &remaining_text[end_pos..];
        } else {
            // No tool end found, stop processing
            break;
        }
    }

    // Return the parsed tools and truncated text
    let truncated_text = &text[..truncation_pos];
    Ok((tool_requests, truncated_text.to_string()))
}

fn parse_caret_tool_parameters_raw(
    content: &str,
    multiline_start_regex: &regex::Regex,
    multiline_end_regex: &regex::Regex,
) -> Result<HashMap<String, Vec<String>>> {
    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        if line.is_empty() {
            i += 1;
            continue;
        }

        // Check for simple key: value parameters
        if let Some((key, value)) = parse_simple_caret_parameter(line) {
            // Handle array syntax: key: [
            if value == "[" {
                let mut array_content = Vec::new();
                i += 1;

                // Collect array elements until ]
                while i < lines.len() {
                    let array_line = lines[i].trim();
                    if array_line == "]" {
                        break;
                    }
                    if !array_line.is_empty() {
                        array_content.push(array_line.to_string());
                    }
                    i += 1;
                }

                params.insert(key, array_content);
            } else {
                params.entry(key).or_default().push(value);
            }
            i += 1;
            continue;
        }

        // Check for multiline parameter start
        if let Some(caps) = multiline_start_regex.captures(line) {
            if let Some(param_name) = caps.get(1) {
                let mut multiline_content = String::new();
                i += 1;

                // Collect content until end marker
                while i < lines.len() {
                    if let Some(end_caps) = multiline_end_regex.captures(lines[i]) {
                        if let Some(end_param) = end_caps.get(1) {
                            if end_param.as_str() == param_name.as_str() {
                                break;
                            }
                        }
                    }

                    if !multiline_content.is_empty() {
                        multiline_content.push('\n');
                    }
                    multiline_content.push_str(lines[i]);
                    i += 1;
                }

                params
                    .entry(param_name.as_str().to_string())
                    .or_default()
                    .push(multiline_content);
            }
            i += 1;
            continue;
        }

        i += 1;
    }

    Ok(params)
}

/// Convert raw parameters to JSON without schema validation (fallback for unregistered tools)
fn convert_raw_params_to_json_fallback(params: &HashMap<String, Vec<String>>) -> Value {
    let mut result = serde_json::Map::new();
    for (key, values) in params {
        if values.len() == 1 {
            result.insert(key.clone(), Value::String(values[0].clone()));
        } else if !values.is_empty() {
            result.insert(
                key.clone(),
                Value::Array(values.iter().map(|v| Value::String(v.clone())).collect()),
            );
        }
    }
    Value::Object(result)
}

fn parse_simple_caret_parameter(line: &str) -> Option<(String, String)> {
    if let Some((key, value)) = line.split_once(':') {
        let key = key.trim();
        let value = value.trim();

        // Skip lines that look like multiline markers
        if value == "---" || key.is_empty() {
            return None;
        }

        Some((key.to_string(), value.to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_simple() {
        let text = concat!("^^^list_projects\n", "^^^");

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_projects");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiline() {
        let text = concat!(
            "^^^write_file\n",
            "project: test\n",
            "path: test.txt\n",
            "content ---\n",
            "This is multiline content\n",
            "with several lines\n",
            "--- content\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "write_file");
        assert_eq!(result[0].input["project"], "test");
        assert_eq!(result[0].input["path"], "test.txt");
        assert!(result[0].input["content"]
            .as_str()
            .unwrap()
            .contains("multiline content"));
        assert!(result[0].input["content"]
            .as_str()
            .unwrap()
            .contains("several lines"));
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_array() {
        let text = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "src/main.rs\n",
            "Cargo.toml\n",
            "README.md\n",
            "]\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");
        assert_eq!(result[0].input["project"], "test");
        assert!(result[0].input["paths"].is_array());
        let paths_array = result[0].input["paths"].as_array().unwrap();
        assert_eq!(paths_array.len(), 3);
        assert_eq!(paths_array[0], "src/main.rs");
        assert_eq!(paths_array[1], "Cargo.toml");
        assert_eq!(paths_array[2], "README.md");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiple_multiline() {
        let text = concat!(
            "^^^edit\n",
            "project: test\n",
            "path: test.rs\n",
            "old_text ---\n",
            "old code here\n",
            "--- old_text\n",
            "new_text ---\n",
            "new code here\n",
            "--- new_text\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "edit");
        assert_eq!(result[0].input["project"], "test");
        assert_eq!(result[0].input["path"], "test.rs");
        assert_eq!(result[0].input["old_text"], "old code here");
        assert_eq!(result[0].input["new_text"], "new code here");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_with_text_before() {
        let text = concat!("I'll help you with that.\n\n", "^^^list_projects\n", "^^^");

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_projects");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_no_tools() {
        let text = "This is just plain text with no tools.";

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_unknown_tool() {
        let text = concat!("^^^unknown_tool\n", "param: value\n", "^^^");

        let result =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown tool: unknown_tool"));
    }

    #[test]
    fn test_parse_simple_caret_parameter() {
        assert_eq!(
            parse_simple_caret_parameter("key: value"),
            Some(("key".to_string(), "value".to_string()))
        );

        assert_eq!(
            parse_simple_caret_parameter("  project  :  test-project  "),
            Some(("project".to_string(), "test-project".to_string()))
        );

        // Should skip multiline markers
        assert_eq!(parse_simple_caret_parameter("content: ---"), None);
        assert_eq!(parse_simple_caret_parameter("content ---"), None);

        // Should skip lines without colon
        assert_eq!(parse_simple_caret_parameter("just text"), None);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_array_vs_single_value() {
        // Test that single value parameters remain as strings with write_file (has single path param)
        let text_single = concat!(
            "^^^write_file\n",
            "project: test\n",
            "path: single-file.txt\n",
            "content: test content\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text_single, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "write_file");
        assert_eq!(result[0].input["project"], "test");

        // Single path should be a string
        let path = &result[0].input["path"];
        assert!(path.is_string());
        assert_eq!(path.as_str().unwrap(), "single-file.txt");

        // Test that array parameters are always arrays, even with single element
        let text_array = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "single-file.txt\n",
            "]\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text_array, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");
        assert_eq!(result[0].input["project"], "test");

        // Array with single element should still be an array
        let paths = &result[0].input["paths"];
        assert!(paths.is_array());
        let paths_array = paths.as_array().unwrap();
        assert_eq!(paths_array.len(), 1);
        assert_eq!(paths_array[0], "single-file.txt");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_empty_and_whitespace_arrays() {
        // Test with different array scenarios for read_files paths parameter
        let text_empty = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "]\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text_empty, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");

        // Empty array should be empty
        let empty_array = result[0].input["paths"].as_array().unwrap();
        assert_eq!(empty_array.len(), 0);

        // Test with whitespace-only and mixed content
        let text_mixed = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "file1.txt\n",
            "  \n",
            "file2.txt\n",
            "\t\n",
            "]\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text_mixed, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");

        // Mixed array should only contain non-empty elements (whitespace trimmed)
        let mixed_array = result[0].input["paths"].as_array().unwrap();
        assert_eq!(mixed_array.len(), 2);
        assert_eq!(mixed_array[0], "file1.txt");
        assert_eq!(mixed_array[1], "file2.txt");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiline_and_array_mix() {
        // Test a combination of array parameters and numeric parameters
        let text = concat!(
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src/main.rs\n",
            "docs/README.md\n",
            "]\n",
            "max_depth: 3\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_files");

        // Single parameters should be strings
        assert_eq!(result[0].input["project"].as_str().unwrap(), "test");

        // Array parameters should be arrays
        let paths = result[0].input["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "src/main.rs");
        assert_eq!(paths[1], "docs/README.md");

        // Numeric parameters should be converted properly
        assert!(result[0].input["max_depth"].is_number());
        assert_eq!(result[0].input["max_depth"].as_u64().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_with_numeric_parameters() {
        // Test with list_files that has max_depth as integer parameter
        let text = concat!(
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "docs\n",
            "]\n",
            "max_depth: 3\n",
            "^^^"
        );

        let (result, _) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_files");

        // String parameters should remain strings
        assert_eq!(result[0].input["project"].as_str().unwrap(), "test");

        // Array parameters should be arrays
        let paths = result[0].input["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "src");
        assert_eq!(paths[1], "docs");

        // Integer parameters should be converted to numbers
        assert!(result[0].input["max_depth"].is_number());
        assert_eq!(result[0].input["max_depth"].as_u64().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_with_offsets() {
        let text = concat!(
            "Here's some text before the tool.\n",
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "]\n",
            "^^^\n",
            "And some text after the tool."
        );

        let (tool_requests, truncated_text) =
            parse_caret_tool_invocations(text, 123, 0, None, &crate::tools::test_registry())
                .unwrap();

        assert_eq!(tool_requests.len(), 1);
        let tool_request = &tool_requests[0];

        // Check that offsets are correctly set
        assert!(tool_request.start_offset.is_some());
        assert!(tool_request.end_offset.is_some());

        let start = tool_request.start_offset.unwrap();
        let end = tool_request.end_offset.unwrap();

        // The tool block should start after "Here's some text before the tool.\n"
        let expected_start = "Here's some text before the tool.\n".len();
        assert_eq!(start, expected_start);

        // Extract the tool block using the offsets
        let tool_block_text = &text[start..end];
        assert!(tool_block_text.starts_with("^^^list_files"));
        assert!(tool_block_text.ends_with("^^^"));

        // Verify the truncated text includes the tool
        assert!(truncated_text.contains("^^^list_files"));
        assert!(truncated_text.contains("^^^"));
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_tool_id_format() {
        let text = concat!(
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "]\n",
            "^^^"
        );

        // Test with specific request_id (start_tool_count is not used anymore)
        let request_id = 456;
        let start_tool_count = 0; // Not used in new format

        let (result, _) = parse_caret_tool_invocations(
            text,
            request_id,
            start_tool_count,
            None,
            &crate::tools::test_registry(),
        )
        .unwrap();
        assert_eq!(result.len(), 1);

        // Tool ID should follow the format: "tool-<request_id>-<tool_index_in_request>"
        // First tool in request gets index 1
        let expected_id = format!("tool-{}-{}", request_id, 1);
        assert_eq!(result[0].id, expected_id);
        assert_eq!(result[0].id, "tool-456-1");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiple_tools_id_format() {
        let text = concat!(
            "^^^list_projects\n",
            "^^^\n",
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "]\n",
            "^^^\n",
            "^^^write_file\n",
            "project: test\n",
            "path: test.txt\n",
            "content: hello\n",
            "^^^"
        );

        let request_id = 789;
        let (result, _) =
            parse_caret_tool_invocations(text, request_id, 0, None, &crate::tools::test_registry())
                .unwrap();
        assert_eq!(result.len(), 3);

        // First tool should get index 1
        assert_eq!(result[0].id, "tool-789-1");
        assert_eq!(result[0].name, "list_projects");

        // Second tool should get index 2
        assert_eq!(result[1].id, "tool-789-2");
        assert_eq!(result[1].name, "list_files");

        // Third tool should get index 3
        assert_eq!(result[2].id, "tool-789-3");
        assert_eq!(result[2].name, "write_file");
    }
}
