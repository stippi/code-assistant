
//! Caret-style tool invocation processor for streaming responses

use crate::ui::streaming::{DisplayFragment, StreamProcessorTrait};
use crate::ui::{UIError, UserInterface};
use llm::{Message, StreamingChunk};
use regex::Regex;
use std::sync::Arc;

/// Stream processor for caret-style tool invocations (^^^tool_name)
pub struct CaretStreamProcessor {
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
    buffer: String,
    tool_regex: Regex,
    multiline_start_regex: Regex,
    multiline_end_regex: Regex,
    current_tool: Option<ToolState>,
}

#[derive(Debug, Clone)]
struct ToolState {
    name: String,
    id: String,
    parameters: Vec<(String, String)>,
    current_multiline: Option<MultilineState>,
}

#[derive(Debug, Clone)]
struct MultilineState {
    param_name: String,
    content: String,
}

impl StreamProcessorTrait for CaretStreamProcessor {
    fn new(ui: Arc<Box<dyn UserInterface>>, request_id: u64) -> Self {
        Self {
            ui,
            request_id,
            buffer: String::new(),
            tool_regex: Regex::new(r"(?m)^\^\^\^([a-zA-Z0-9_]+)$").unwrap(),
            multiline_start_regex: Regex::new(r"(?m)^([a-zA-Z0-9_]+)\s+---\s*$").unwrap(),
            multiline_end_regex: Regex::new(r"(?m)^---\s+([a-zA-Z0-9_]+)\s*$").unwrap(),
            current_tool: None,
        }
    }

    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        match chunk {
            StreamingChunk::Text(content) => {
                self.buffer.push_str(content);
                self.process_buffer()?;
            }
            StreamingChunk::Thinking(content) => {
                self.buffer.push_str(content);
                self.process_buffer()?;
            }
            _ => {
                // Handle other chunk types (InputJson, RateLimit, etc.)
                // For now, we don't process these in caret mode
            }
        }
        Ok(())
    }

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        let content_text = match &message.content {
            llm::MessageContent::Text(text) => text.as_str(),
            llm::MessageContent::Structured(blocks) => {
                // Extract text from structured content blocks
                let mut combined_text = String::new();
                for block in blocks {
                    match block {
                        llm::ContentBlock::Text { text } => combined_text.push_str(text),
                        llm::ContentBlock::Thinking { thinking, .. } => combined_text.push_str(thinking),
                        _ => {} // Skip other block types for now
                    }
                }
                return Ok(vec![DisplayFragment::PlainText(combined_text)]);
            }
        };

        // Parse the complete message to extract caret tool blocks
        let mut remaining = content_text;

        while let Some(tool_match) = self.tool_regex.find(remaining) {
            // Add any text before the tool as plain text
            if tool_match.start() > 0 {
                let before_text = &remaining[..tool_match.start()];
                if !before_text.trim().is_empty() {
                    fragments.push(DisplayFragment::PlainText(before_text.to_string()));
                }
            }

            // Extract tool name
            let tool_name = self.tool_regex
                .captures(&remaining[tool_match.start()..])
                .and_then(|caps| caps.get(1))
                .map(|m| m.as_str())
                .unwrap_or("unknown");

            let tool_id = format!("{}_{}", tool_name, fragments.len());
            fragments.push(DisplayFragment::ToolName {
                name: tool_name.to_string(),
                id: tool_id.clone(),
            });

            // Find the end of this tool block
            let tool_start = tool_match.end();
            let remaining_after_tool = &remaining[tool_start..];

            if let Some(end_match) = Regex::new(r"(?m)^\^\^\^$").unwrap().find(remaining_after_tool) {
                let tool_content = &remaining_after_tool[..end_match.start()];

                // Parse parameters from tool content
                let params = self.parse_tool_parameters(tool_content)?;
                for (name, value) in params {
                    fragments.push(DisplayFragment::ToolParameter {
                        name,
                        value,
                        tool_id: tool_id.clone(),
                    });
                }

                fragments.push(DisplayFragment::ToolEnd { id: tool_id });

                // Move to after this tool block
                remaining = &remaining_after_tool[end_match.end()..];
            } else {
                // No end found, treat rest as tool content
                let params = self.parse_tool_parameters(remaining_after_tool)?;
                for (name, value) in params {
                    fragments.push(DisplayFragment::ToolParameter {
                        name,
                        value,
                        tool_id: tool_id.clone(),
                    });
                }
                fragments.push(DisplayFragment::ToolEnd { id: tool_id });
                break;
            }
        }

        // Add any remaining text as plain text
        if !remaining.trim().is_empty() {
            fragments.push(DisplayFragment::PlainText(remaining.to_string()));
        }

        Ok(fragments)
    }
}

impl CaretStreamProcessor {
    fn process_buffer(&mut self) -> Result<(), UIError> {
        // Look for complete tool blocks or process streaming content
        loop {
            let tool_match = self.tool_regex.find(&self.buffer);
            if let Some(tool_match) = tool_match {
                // Send any text before the tool as plain text
                if tool_match.start() > 0 {
                    let before_text = self.buffer[..tool_match.start()].to_string();
                    if !before_text.trim().is_empty() {
                        self.send_plain_text(&before_text)?;
                    }
                    self.buffer.drain(..tool_match.start());
                    continue;
                }

                // Extract tool name
                let buffer_copy = self.buffer.clone();
                if let Some(caps) = self.tool_regex.captures(&buffer_copy) {
                    if let Some(tool_name) = caps.get(1) {
                        let tool_id = format!("{}_{}", tool_name.as_str(), self.request_id);

                        self.send_tool_start(tool_name.as_str(), &tool_id)?;

                        self.current_tool = Some(ToolState {
                            name: tool_name.as_str().to_string(),
                            id: tool_id,
                            parameters: Vec::new(),
                            current_multiline: None,
                        });

                        // Remove the tool start line from buffer
                        self.buffer.drain(..tool_match.end());
                        break;
                    }
                }
            } else {
                break;
            }
        }

        // Process tool content if we're inside a tool
        if self.current_tool.is_some() {
            self.process_tool_content()?;
        }

        Ok(())
    }

    fn process_tool_content(&mut self) -> Result<(), UIError> {
        // Check for tool end
        let end_regex = Regex::new(r"(?m)^\^\^\^$").unwrap();
        let buffer_copy = self.buffer.clone();

        if let Some(end_match) = end_regex.find(&buffer_copy) {
            // Process content before the end
            let content = buffer_copy[..end_match.start()].to_string();
            if !content.trim().is_empty() {
                self.process_remaining_tool_content(&content)?;
            }

            // Send tool end
            let tool_id = self.current_tool.as_ref().map(|t| t.id.clone());
            if let Some(id) = tool_id {
                self.send_tool_end(&id)?;
            }

            self.current_tool = None;
            self.buffer.drain(..end_match.end());
            return Ok(());
        }

        // Look for parameter patterns in the current buffer
        let lines: Vec<&str> = buffer_copy.lines().collect();
        let mut processed_lines = 0;

        for (i, line) in lines.iter().enumerate() {
            // Check for simple key: value parameters
            if let Some((key, value)) = self.parse_simple_parameter(line) {
                self.add_parameter(key, value)?;
                processed_lines = i + 1;
                continue;
            }

            // Check for multiline parameter start
            if let Some(caps) = self.multiline_start_regex.captures(line) {
                if let Some(param_name) = caps.get(1) {
                    if let Some(tool) = &mut self.current_tool {
                        tool.current_multiline = Some(MultilineState {
                            param_name: param_name.as_str().to_string(),
                            content: String::new(),
                        });
                    }
                    processed_lines = i + 1;
                    continue;
                }
            }

            // Check for multiline parameter end
            if let Some(caps) = self.multiline_end_regex.captures(line) {
                if let Some(param_name) = caps.get(1) {
                    let multiline_data = if let Some(tool) = &self.current_tool {
                        tool.current_multiline.as_ref().and_then(|ml| {
                            if ml.param_name == param_name.as_str() {
                                Some((ml.param_name.clone(), ml.content.clone()))
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    };

                    if let Some((param_name, content)) = multiline_data {
                        self.add_parameter(param_name, content)?;
                        if let Some(tool) = &mut self.current_tool {
                            tool.current_multiline = None;
                        }
                        processed_lines = i + 1;
                        continue;
                    }
                }
            }

            // If we're in a multiline parameter, add the line to content
            if let Some(tool) = &mut self.current_tool {
                if let Some(multiline) = &mut tool.current_multiline {
                    if !multiline.content.is_empty() {
                        multiline.content.push('\n');
                    }
                    multiline.content.push_str(line);
                    processed_lines = i + 1;
                    continue;
                }
            }

            // If we can't process this line, stop here
            break;
        }

        // Remove processed lines from buffer
        if processed_lines > 0 {
            let lines_to_remove: String = lines[..processed_lines].join("\n");
            if processed_lines < lines.len() {
                // Add newline after removed content if there are remaining lines
                self.buffer.drain(..lines_to_remove.len() + 1);
            } else {
                self.buffer.drain(..lines_to_remove.len());
            }
        }

        Ok(())
    }

    fn parse_simple_parameter(&self, line: &str) -> Option<(String, String)> {
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

    fn add_parameter(&mut self, name: String, value: String) -> Result<(), UIError> {
        let tool_id = if let Some(tool) = &mut self.current_tool {
            tool.parameters.push((name.clone(), value.clone()));
            tool.id.clone()
        } else {
            return Ok(());
        };

        self.send_tool_parameter(&name, &value, &tool_id)?;
        Ok(())
    }

    fn process_remaining_tool_content(&mut self, content: &str) -> Result<(), UIError> {
        let params = self.parse_tool_parameters(content)?;
        for (name, value) in params {
            self.add_parameter(name, value)?;
        }
        Ok(())
    }

    fn parse_tool_parameters(&self, content: &str) -> Result<Vec<(String, String)>, UIError> {
        let mut params = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            if line.is_empty() {
                i += 1;
                continue;
            }

            // Check for simple key: value
            if let Some((key, value)) = self.parse_simple_parameter(line) {
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

                    // Convert array to JSON-like format for now
                    let array_value = format!("[{}]", array_content.join(", "));
                    params.push((key, array_value));
                } else {
                    params.push((key, value));
                }
                i += 1;
                continue;
            }

            // Check for multiline parameter start
            if let Some(caps) = self.multiline_start_regex.captures(line) {
                if let Some(param_name) = caps.get(1) {
                    let mut multiline_content = String::new();
                    i += 1;

                    // Collect content until end marker
                    while i < lines.len() {
                        if let Some(end_caps) = self.multiline_end_regex.captures(lines[i]) {
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

                    params.push((param_name.as_str().to_string(), multiline_content));
                }
                i += 1;
                continue;
            }

            i += 1;
        }

        Ok(params)
    }

    fn finalize_buffer(&mut self) -> Result<(), UIError> {
        // Process any remaining content
        if !self.buffer.trim().is_empty() {
            if self.current_tool.is_some() {
                self.process_remaining_tool_content(&self.buffer.clone())?;
                if let Some(tool) = &self.current_tool {
                    self.send_tool_end(&tool.id)?;
                }
                self.current_tool = None;
            } else {
                self.send_plain_text(&self.buffer)?;
            }
            self.buffer.clear();
        }
        Ok(())
    }

    fn send_plain_text(&self, text: &str) -> Result<(), UIError> {
        if !text.trim().is_empty() {
            let fragment = DisplayFragment::PlainText(text.to_string());
            self.ui.display_fragment(&fragment)?;
        }
        Ok(())
    }

    fn send_tool_start(&self, name: &str, id: &str) -> Result<(), UIError> {
        let fragment = DisplayFragment::ToolName {
            name: name.to_string(),
            id: id.to_string(),
        };
        self.ui.display_fragment(&fragment)?;
        Ok(())
    }

    fn send_tool_parameter(&self, name: &str, value: &str, tool_id: &str) -> Result<(), UIError> {
        let fragment = DisplayFragment::ToolParameter {
            name: name.to_string(),
            value: value.to_string(),
            tool_id: tool_id.to_string(),
        };
        self.ui.display_fragment(&fragment)?;
        Ok(())
    }

    fn send_tool_end(&self, id: &str) -> Result<(), UIError> {
        let fragment = DisplayFragment::ToolEnd {
            id: id.to_string(),
        };
        self.ui.display_fragment(&fragment)?;
        Ok(())
    }
}
