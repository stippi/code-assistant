//! Parser registry for different tool invocation syntaxes

use crate::agent::ToolSyntax;
use crate::tools::{parse_caret_tool_invocations, parse_xml_tool_invocations, ToolRequest};
use crate::ui::streaming::StreamProcessorTrait;
use crate::ui::UserInterface;
use anyhow::Result;
use llm::{ContentBlock, LLMResponse};
use std::sync::Arc;

/// Trait for parsing tool invocations from LLM responses
pub trait ToolInvocationParser: Send + Sync {
    /// Extract `ToolRequest`s from a complete LLM response and return truncated response.
    /// Implementations may inspect either the raw text blocks, the `ToolUse`
    /// blocks, or both.
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)>;

    /// A stream-processor that renders *this syntax* for the UI.
    fn stream_processor(
        &self,
        ui: Arc<Box<dyn UserInterface>>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait>;
}

/// Parse Caret tool requests from LLM response and return both requests and truncated response after first tool
fn parse_and_truncate_caret_response(
    response: &LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();
    let mut truncated_content = Vec::new();

    for block in &response.content {
        if let ContentBlock::Text { text } = block {
            // Parse Caret tool invocations and get truncation position
            let (block_tool_requests, truncated_text) =
                parse_caret_tool_invocations_with_truncation(text, request_id, tool_requests.len())?;

            tool_requests.extend(block_tool_requests.clone());

            // If tools were found in this text block, use truncated text
            if !block_tool_requests.is_empty() {
                truncated_content.push(ContentBlock::Text {
                    text: truncated_text,
                });
                break; // Stop processing after first tool block
            } else {
                // No tools in this block, keep original text
                truncated_content.push(block.clone());
            }
        } else {
            // Keep other blocks (Thinking, etc.) if no tools found yet
            if tool_requests.is_empty() {
                truncated_content.push(block.clone());
            }
        }
    }

    // Create truncated response
    let truncated_response = LLMResponse {
        content: truncated_content,
        usage: response.usage.clone(),
        rate_limit_info: response.rate_limit_info.clone(),
    };

    Ok((tool_requests, truncated_response))
}

/// Parse XML tool requests from LLM response and return both requests and truncated response after first tool
fn parse_and_truncate_xml_response(
    response: &LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();
    let mut truncated_content = Vec::new();

    for block in &response.content {
        if let ContentBlock::Text { text } = block {
            // Parse XML tool invocations and get truncation position
            let (block_tool_requests, truncated_text) =
                parse_xml_tool_invocations_with_truncation(text, request_id, tool_requests.len())?;

            tool_requests.extend(block_tool_requests.clone());

            // If tools were found in this text block, use truncated text
            if !block_tool_requests.is_empty() {
                truncated_content.push(ContentBlock::Text {
                    text: truncated_text,
                });
                break; // Stop processing after first tool block
            } else {
                // No tools in this block, keep original text
                truncated_content.push(block.clone());
            }
        } else {
            // Keep other blocks (Thinking, etc.) if no tools found yet
            if tool_requests.is_empty() {
                truncated_content.push(block.clone());
            }
        }
    }

    // Create truncated response
    let truncated_response = LLMResponse {
        content: truncated_content,
        usage: response.usage.clone(),
        rate_limit_info: response.rate_limit_info.clone(),
    };

    Ok((tool_requests, truncated_response))
}

/// Parse JSON tool requests from LLM response and return both requests and truncated response after first tool
fn parse_and_truncate_json_response(
    response: &LLMResponse,
    _request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();
    let mut truncated_content = Vec::new();

    for block in &response.content {
        if let ContentBlock::ToolUse { id, name, input } = block {
            // For ToolUse blocks, create ToolRequest directly
            let tool_request = ToolRequest {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            };
            tool_requests.push(tool_request);

            // Keep the ToolUse block in truncated content
            truncated_content.push(block.clone());

            // Stop processing after first tool (native mode)
            break;
        } else {
            // Keep other blocks if no tools found yet
            if tool_requests.is_empty() {
                truncated_content.push(block.clone());
            }
        }
    }

    // Create truncated response
    let truncated_response = LLMResponse {
        content: truncated_content,
        usage: response.usage.clone(),
        rate_limit_info: response.rate_limit_info.clone(),
    };

    Ok((tool_requests, truncated_response))
}

/// Parse Caret tool invocations and return both tool requests and truncated text
fn parse_caret_tool_invocations_with_truncation(
    text: &str,
    request_id: u64,
    start_tool_count: usize,
) -> Result<(Vec<ToolRequest>, String)> {
    // Parse tool requests using existing function
    let all_tool_requests = parse_caret_tool_invocations(text, request_id, start_tool_count)?;

    if all_tool_requests.is_empty() {
        // No tools found, return original text
        return Ok((all_tool_requests, text.to_string()));
    }

    // Only keep the first tool request to enforce single tool per message
    let tool_requests = vec![all_tool_requests[0].clone()];

    // Find the end position of the first tool to truncate text
    let truncated_text = find_first_caret_tool_end_and_truncate(text)?;

    Ok((tool_requests, truncated_text))
}

/// Parse XML tool invocations and return both tool requests and truncated text
fn parse_xml_tool_invocations_with_truncation(
    text: &str,
    request_id: u64,
    start_tool_count: usize,
) -> Result<(Vec<ToolRequest>, String)> {
    // Parse tool requests using existing function
    let all_tool_requests = parse_xml_tool_invocations(text, request_id, start_tool_count)?;

    if all_tool_requests.is_empty() {
        // No tools found, return original text
        return Ok((all_tool_requests, text.to_string()));
    }

    // Only keep the first tool request to enforce single tool per message
    let tool_requests = vec![all_tool_requests[0].clone()];

    // Find the end position of the first tool to truncate text
    let truncated_text = find_first_tool_end_and_truncate(text)?;

    Ok((tool_requests, truncated_text))
}

/// Find the end of the first caret tool and truncate there
fn find_first_caret_tool_end_and_truncate(text: &str) -> Result<String> {
    let tool_start_regex = regex::Regex::new(r"(?m)^\^\^\^([a-zA-Z0-9_]+)$").unwrap();
    let tool_end_regex = regex::Regex::new(r"(?m)^\^\^\^$").unwrap();

    // Find the first tool start
    if let Some(start_match) = tool_start_regex.find(text) {
        let after_start = &text[start_match.end()..];

        // Find the corresponding tool end
        if let Some(end_match) = tool_end_regex.find(after_start) {
            let end_pos = start_match.end() + end_match.end();
            return Ok(text[..end_pos].to_string());
        }
    }

    // No complete tool found, return original text
    Ok(text.to_string())
}

/// Find the end of the first tool in text and truncate there
fn find_first_tool_end_and_truncate(text: &str) -> Result<String> {
    let mut current_pos = 0;
    let mut in_tool = false;
    let mut tool_depth = 0;

    while current_pos < text.len() {
        if let Some(tag_start) = text[current_pos..].find('<') {
            let absolute_pos = current_pos + tag_start;

            // Look for tool tags
            if let Some(tag_end) = text[absolute_pos..].find('>') {
                let tag_content = &text[absolute_pos + 1..absolute_pos + tag_end];

                if tag_content.starts_with("tool:") {
                    // Tool start tag
                    in_tool = true;
                    tool_depth += 1;
                } else if tag_content.starts_with("/tool:") {
                    // Tool end tag
                    if in_tool {
                        tool_depth -= 1;
                        if tool_depth == 0 {
                            // Found the end of the first tool, truncate here
                            let end_pos = absolute_pos + tag_end + 1;
                            return Ok(text[..end_pos].to_string());
                        }
                    }
                }
                current_pos = absolute_pos + tag_end + 1;
            } else {
                current_pos = absolute_pos + 1;
            }
        } else {
            break;
        }
    }

    // No complete tool found, return original text
    Ok(text.to_string())
}

/// XML-based tool invocation parser
pub struct XmlParser;

impl ToolInvocationParser for XmlParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_xml_response(response, req_id)
    }

    fn stream_processor(
        &self,
        ui: Arc<Box<dyn UserInterface>>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::StreamProcessorTrait;
        use crate::ui::streaming::XmlStreamProcessor;
        Box::new(XmlStreamProcessor::new(ui, request_id))
    }


}

/// Caret-based tool invocation parser
pub struct CaretParser;

impl ToolInvocationParser for CaretParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_caret_response(response, req_id)
    }

    fn stream_processor(
        &self,
        ui: Arc<Box<dyn UserInterface>>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::CaretStreamProcessor;
        use crate::ui::streaming::StreamProcessorTrait;
        Box::new(CaretStreamProcessor::new(ui, request_id))
    }
}

/// JSON-based (native) tool invocation parser
pub struct JsonParser;

impl ToolInvocationParser for JsonParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_json_response(response, req_id)
    }

    fn stream_processor(
        &self,
        ui: Arc<Box<dyn UserInterface>>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::JsonStreamProcessor;
        use crate::ui::streaming::StreamProcessorTrait;
        Box::new(JsonStreamProcessor::new(ui, request_id))
    }


}

/// Registry for tool invocation parsers
pub struct ParserRegistry;

impl ParserRegistry {
    /// Get the appropriate parser for the given tool syntax
    pub fn get(syntax: ToolSyntax) -> Box<dyn ToolInvocationParser> {
        match syntax {
            ToolSyntax::Xml => Box::new(XmlParser),
            ToolSyntax::Native => Box::new(JsonParser),
            ToolSyntax::Caret => Box::new(CaretParser),
        }
    }
}
