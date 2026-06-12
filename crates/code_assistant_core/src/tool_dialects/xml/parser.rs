//! Parsing XML-style tool invocations out of LLM response text.

use crate::tool_dialects::convert_params_to_json;
use crate::tools::core::ToolRegistry;
use crate::tools::tool_use_filter::ToolUseFilter;
use crate::tools::ToolRequest;
use crate::types::ToolError;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, trace};

pub(crate) const TOOL_TAG_PREFIX: &str = "tool:";
const PARAM_TAG_PREFIX: &str = "param:";

#[derive(Debug, Clone)]
enum ParseState {
    SearchingForTool,
    InTool {
        tool_name: String,
        start_pos: usize,
    },
    InParam {
        tool_name: String,
        param_name: String,
        start_pos: usize,
    },
}

fn parse_tool_xml(xml: &str, registry: &ToolRegistry) -> Result<(String, Value), ToolError> {
    trace!("Parsing XML:\n{}", xml);

    let tool_name = xml
        .trim()
        .strip_prefix(&format!("<{TOOL_TAG_PREFIX}"))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.strip_suffix('>'))
        .ok_or_else(|| ToolError::ParseError("Missing tool name".into()))?
        .to_string();

    trace!("Found tool name: {}", tool_name);

    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_param = String::new();
    let mut content_start = 0;

    let chars = xml.char_indices().peekable();
    for (i, ch) in chars {
        if ch == '<' {
            // Check for parameter tag
            let rest = &xml[i..];
            trace!("Found '<', rest of string: {}", rest);
            if rest.starts_with(&format!("</{PARAM_TAG_PREFIX}")) {
                // Closing tag
                let param_name = rest[format!("</{PARAM_TAG_PREFIX}").len()..] // skip the "</param:"
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
            } else if let Some(param_start) = rest.strip_prefix(&format!("<{PARAM_TAG_PREFIX}")) {
                // Opening tag
                if let Some(param_name) = param_start.split('>').next() {
                    current_param = param_name.to_string();
                    content_start = i + format!("<{PARAM_TAG_PREFIX}{param_name}>").len();
                    trace!("Found param start: {} at {}", current_param, content_start);
                }
            }
        }
    }

    trace!("Final parameters: {:?}", params);

    // Convert parameters to JSON using the ToolRegistry
    let json_params = convert_params_to_json(&tool_name, &params, registry)
        .map_err(|e| ToolError::ParseError(format!("Error converting parameters to JSON: {e}")))?;

    Ok((tool_name, json_params))
}

pub fn parse_xml_tool_invocations(
    text: &str,
    request_id: u64,
    _start_tool_count: usize,
    filter: Option<&dyn ToolUseFilter>,
    registry: &ToolRegistry,
) -> Result<(Vec<ToolRequest>, String)> {
    let mut tool_requests = Vec::new();
    let mut state = ParseState::SearchingForTool;
    let mut current_pos = 0;
    let mut truncation_pos = text.len(); // Default to end of text

    while current_pos < text.len() {
        // Find next '<' character
        if let Some(tag_start) = text[current_pos..].find('<') {
            let abs_tag_start = current_pos + tag_start;

            // Extract tag content until '>'
            if let Some(tag_end) = text[abs_tag_start..].find('>') {
                let abs_tag_end = abs_tag_start + tag_end;
                let tag_content = &text[abs_tag_start + 1..abs_tag_end]; // Skip '<' and '>'

                debug!("Found tag: <{}>", tag_content);

                // Check if this is a tool tag
                if let Some(tool_name) = tag_content.strip_prefix(TOOL_TAG_PREFIX) {
                    match &state {
                        ParseState::SearchingForTool => {
                            // Check filter before starting this tool
                            let tool_index = tool_requests.len() + 1;
                            if let Some(filter) = filter {
                                if !filter.allow_tool_at_position(tool_name, tool_index) {
                                    // Tool not allowed at this position, keep existing truncation_pos
                                    break;
                                }
                            }

                            // Start of a new tool invocation
                            state = ParseState::InTool {
                                tool_name: tool_name.to_string(),
                                start_pos: abs_tag_start,
                            };
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InTool {
                            tool_name: current_tool,
                            ..
                        }
                        | ParseState::InParam {
                            tool_name: current_tool,
                            ..
                        } => {
                            // Found another tool start while already in a tool - this is an error
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found nested tool invocation. Started '{current_tool}' but found start of '{tool_name}' before closing the first one"
                            )).into());
                        }
                    }
                }
                // Check if this is a tool closing tag
                else if let Some(tool_name) =
                    tag_content.strip_prefix(&format!("/{TOOL_TAG_PREFIX}"))
                {
                    match &state {
                        ParseState::SearchingForTool => {
                            // Found closing tag without opening tag
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found closing tag '</tool:{tool_name}>' without corresponding opening tag"
                            )).into());
                        }
                        ParseState::InTool {
                            tool_name: current_tool,
                            start_pos,
                        } => {
                            if tool_name != current_tool {
                                // Mismatched closing tag
                                return Err(ToolError::ParseError(format!(
                                    "Malformed tool invocation: mismatching tool names in start and end tag. Expected '</tool:{current_tool}>' but found '</tool:{tool_name}>'"
                                )).into());
                            }

                            // Extract the complete tool XML
                            let tool_content = &text[*start_pos..abs_tag_end + 1];
                            debug!("Found complete tool content:\n{}", tool_content);

                            // Parse the tool XML to get tool name and parameters
                            let (parsed_tool_name, tool_params) =
                                parse_tool_xml(tool_content, registry)?;

                            // Check if the tool exists in the registry
                            if registry.get(&parsed_tool_name).is_none() {
                                return Err(ToolError::UnknownTool(parsed_tool_name).into());
                            }

                            // Generate a unique tool ID: tool-<request_id>-<tool_index_in_request>
                            let tool_index = tool_requests.len() + 1;
                            let tool_id = format!("tool-{request_id}-{tool_index}");

                            // Create a ToolRequest with offset information
                            let tool_request = ToolRequest {
                                id: tool_id,
                                name: parsed_tool_name.clone(),
                                input: tool_params,
                                start_offset: Some(*start_pos),
                                end_offset: Some(abs_tag_end + 1),
                            };

                            tool_requests.push(tool_request);

                            // Always update truncation position after each successfully processed tool
                            // This ensures we can truncate cleanly after the last allowed tool
                            truncation_pos = abs_tag_end + 1;

                            // Check if we should allow content after this tool
                            if let Some(filter) = filter {
                                if !filter.allow_content_after_tool(&parsed_tool_name, tool_index) {
                                    // No content allowed after this tool, truncate here
                                    state = ParseState::SearchingForTool;
                                    break;
                                }
                            }

                            // Reset state
                            state = ParseState::SearchingForTool;
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InParam {
                            param_name: current_param,
                            ..
                        } => {
                            // We're still inside a parameter when trying to close the tool - this is an error
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: unclosed parameter '{current_param}' in tool '{tool_name}' - missing closing tag '</param:{current_param}>'"
                            )).into());
                        }
                    }
                }
                // Handle parameter tags and other logic same as original function...
                else if let Some(param_name) = tag_content.strip_prefix(PARAM_TAG_PREFIX) {
                    match &state {
                        ParseState::SearchingForTool => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InTool {
                            tool_name,
                            start_pos,
                        } => {
                            state = ParseState::InParam {
                                tool_name: tool_name.clone(),
                                param_name: param_name.to_string(),
                                start_pos: *start_pos,
                            };
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InParam {
                            tool_name,
                            param_name: current_param,
                            ..
                        } => {
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found nested parameter. Started parameter '{current_param}' in tool '{tool_name}' but found start of parameter '{param_name}' before closing the first one"
                            )).into());
                        }
                    }
                } else if let Some(param_name) =
                    tag_content.strip_prefix(&format!("/{PARAM_TAG_PREFIX}"))
                {
                    match &state {
                        ParseState::SearchingForTool | ParseState::InTool { .. } => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InParam {
                            tool_name,
                            param_name: current_param,
                            start_pos,
                            ..
                        } => {
                            if param_name != current_param {
                                return Err(ToolError::ParseError(format!(
                                    "Malformed tool invocation: mismatching parameter names in start and end tag. Expected '</param:{current_param}>' but found '</param:{param_name}>' in tool '{tool_name}'"
                                )).into());
                            }

                            state = ParseState::InTool {
                                tool_name: tool_name.clone(),
                                start_pos: *start_pos,
                            };
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                    }
                } else {
                    match &state {
                        ParseState::InTool { tool_name, .. } => {
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found unexpected tag '<{tag_content}>' inside tool '{tool_name}'. Only parameter tags are allowed inside tool blocks"
                            )).into());
                        }
                        ParseState::InParam { .. } => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::SearchingForTool => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                    }
                }
            } else {
                current_pos = abs_tag_start + 1;
                continue;
            }
        } else {
            break;
        }
    }

    // Check if we ended in an incomplete state
    match state {
        ParseState::SearchingForTool => {
            // This is fine
        }
        ParseState::InTool { tool_name, .. } => {
            return Err(ToolError::ParseError(format!(
                "Malformed tool invocation: unclosed tool '{tool_name}' - missing closing tag '</tool:{tool_name}>'"
            ))
            .into());
        }
        ParseState::InParam {
            tool_name,
            param_name,
            ..
        } => {
            return Err(ToolError::ParseError(format!(
                "Malformed tool invocation: unclosed parameter '{param_name}' in tool '{tool_name}' - missing closing tag '</param:{param_name}>'"
            )).into());
        }
    }

    // Return the parsed tools and truncated text
    let truncated_text = &text[..truncation_pos];
    Ok((tool_requests, truncated_text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_xml_tool_invocations_with_offsets() {
        let text = concat!(
            "Some introductory text.\n",
            "<tool:read_files>\n",
            "<param:project>test-project</param:project>\n",
            "<param:path>file1.txt</param:path>\n",
            "<param:path>file2.txt</param:path>\n",
            "</tool:read_files>\n",
            "Some concluding text."
        );

        let (tool_requests, truncated_text) =
            parse_xml_tool_invocations(text, 456, 0, None, &crate::tools::test_registry()).unwrap();

        assert_eq!(tool_requests.len(), 1);
        let tool_request = &tool_requests[0];

        // Check that offsets are correctly set
        assert!(tool_request.start_offset.is_some());
        assert!(tool_request.end_offset.is_some());

        let start = tool_request.start_offset.unwrap();
        let end = tool_request.end_offset.unwrap();

        // The tool block should start after "Some introductory text.\n"
        let expected_start = "Some introductory text.\n".len();
        assert_eq!(start, expected_start);

        // Extract the tool block using the offsets
        let tool_block_text = &text[start..end];
        assert!(tool_block_text.starts_with("<tool:read_files>"));
        assert!(tool_block_text.ends_with("</tool:read_files>"));

        // Verify the truncated text includes the tool
        assert!(truncated_text.contains("<tool:read_files>"));
        assert!(truncated_text.contains("</tool:read_files>"));
    }
}
