//! Caret-style tool invocation processor for streaming responses
use crate::ui::streaming::{DisplayFragment, StreamProcessorTrait};
use crate::ui::{UIError, UserInterface};
use llm::{Message, MessageContent, StreamingChunk};
use regex::Regex;
use std::sync::Arc;

#[derive(Debug, PartialEq, Clone)]
enum ParserState {
    OutsideTool,
    InsideTool,
    CollectingName {
        partial_name: String,
        tool_id: String,
    },
    CollectingType {
        param_name: String,
        tool_id: String,
    },
    CollectingValue {
        param_name: String,
        tool_id: String,
    },
    CollectingMultiline {
        param_name: String,
        content: String,
        tool_id: String,
    },
    CollectingArray {
        param_name: String,
        elements: Vec<String>,
        tool_id: String,
    },
}

pub struct CaretStreamProcessor {
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
    buffer: String,
    state: ParserState,
    tool_regex: Regex,
    multiline_start_regex: Regex,
    multiline_end_regex: Regex,
    current_tool_id: String,
}

impl StreamProcessorTrait for CaretStreamProcessor {
    fn new(ui: Arc<Box<dyn UserInterface>>, request_id: u64) -> Self {
        Self {
            ui,
            request_id,
            buffer: String::new(),
            state: ParserState::OutsideTool,
            tool_regex: Regex::new(r"^\^\^\^([a-zA-Z0-9_]+)$").unwrap(),
            multiline_start_regex: Regex::new(r"^([a-zA-Z0-9_]+)\s+---$").unwrap(),
            multiline_end_regex: Regex::new(r"^---\s+([a-zA-Z0-9_]+)$").unwrap(),
            current_tool_id: String::new(),
        }
    }

    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        if let StreamingChunk::Text(text) = chunk {
            self.buffer.push_str(text);
        }
        self.process_buffer()?;
        Ok(())
    }

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let text = if let MessageContent::Text(text) = &message.content {
            text
        } else {
            return Ok(vec![]);
        };

        let mut fragments = Vec::new();
        if let Some(plain_text) = text.split("^^^").next() {
            if !plain_text.is_empty() {
                fragments.push(DisplayFragment::PlainText(plain_text.to_string()));
            }
        }

        let tool_blocks = text.split("^^^").skip(1);
        for block in tool_blocks {
            if let Some(tool_name) = block.lines().next().map(|l| l.trim()) {
                if tool_name.is_empty() {
                    continue;
                }
                let tool_id = format!("{}_{}", tool_name, self.request_id);
                fragments.push(DisplayFragment::ToolName {
                    name: tool_name.to_string(),
                    id: tool_id.clone(),
                });

                let tool_content = block.lines().skip(1).collect::<Vec<_>>().join("\n");
                let params = self.parse_tool_parameters(&tool_content)?;

                for (name, value) in params {
                    fragments.push(DisplayFragment::ToolParameter {
                        name,
                        value,
                        tool_id: tool_id.clone(),
                    });
                }
                fragments.push(DisplayFragment::ToolEnd { id: tool_id });
            }
        }

        Ok(fragments)
    }
}

impl CaretStreamProcessor {
    fn process_buffer(&mut self) -> Result<(), UIError> {
        loop {
            // Check if we should process any content from the buffer
            if let Some(to_emit) = self.get_content_to_emit()? {
                if to_emit.is_empty() {
                    break;
                }

                // For complete lines, process them
                if to_emit.ends_with('\n') {
                    self.process_line(to_emit.trim_end())?;
                } else {
                    // For partial content that's safe to emit, send as plain text
                    self.send_plain_text(&to_emit)?;
                }

                // Remove the processed content from buffer
                self.buffer.drain(..to_emit.len());
            } else {
                // If we can't emit anything, check for complete patterns without newlines
                let trimmed = self.buffer.trim();
                if trimmed == "^^^" && !matches!(self.state, ParserState::OutsideTool) {
                    // Process the tool closing marker
                    self.process_line("^^^")?;
                    self.buffer.clear();
                } else {
                    // Nothing more to process - buffered newlines will remain for trimming
                    break;
                }
            }
        }
        Ok(())
    }

    /// Determines what content can be safely emitted from the current buffer
    /// Returns None if we need to wait for more input, or Some(content) to emit
    fn get_content_to_emit(&self) -> Result<Option<String>, UIError> {
        if self.buffer.is_empty() {
            return Ok(None);
        }

        match &self.state {
            ParserState::OutsideTool => self.get_content_to_emit_outside_tool(),

            ParserState::InsideTool => {
                // Inside tool blocks, only emit complete lines for parameter processing
                if let Some(newline_pos) = self.buffer.find('\n') {
                    let line_content = &self.buffer[..=newline_pos];
                    return Ok(Some(line_content.to_string()));
                }
                Ok(None)
            }

            ParserState::CollectingMultiline { .. } => {
                // During multiline collection, we can stream content chunks
                // But we need to be careful about lines that might end the multiline block
                self.get_content_to_emit_multiline()
            }

            ParserState::CollectingValue { .. } => {
                // During value collection, we can stream the value content
                // But need to watch for line endings that might indicate parameter end
                self.get_content_to_emit_value()
            }

            // For other states, be conservative and wait for complete lines
            _ => {
                if let Some(newline_pos) = self.buffer.find('\n') {
                    let line_content = &self.buffer[..=newline_pos];
                    return Ok(Some(line_content.to_string()));
                }
                Ok(None)
            }
        }
    }

    /// Determines what content to emit when outside a tool block
    /// We buffer standalone newlines to provide natural trimming at block boundaries
    fn get_content_to_emit_outside_tool(&self) -> Result<Option<String>, UIError> {
        if let Some(newline_pos) = self.buffer.find('\n') {
            // We have at least one complete line
            let line_with_newline = &self.buffer[..=newline_pos];
            let line_content = line_with_newline.trim_end();

            // Check if this line is tool syntax
            if self.tool_regex.is_match(line_content) || line_content == "^^^" {
                // This is tool syntax, emit as a complete line for processing
                return Ok(Some(line_with_newline.to_string()));
            } else {
                // Not tool syntax, emit as plain text
                return Ok(Some(line_with_newline.to_string()));
            }
        }

        // No complete line yet - check if we should buffer or emit
        let remaining = &self.buffer;

        // Buffer standalone newlines for boundary trimming
        if remaining == "\n" {
            return Ok(None);
        }

        // If what we have definitely cannot be tool syntax, emit it
        if !self.could_be_tool_syntax_start(remaining) {
            return Ok(Some(remaining.to_string()));
        }

        // Potential tool syntax - keep buffering
        Ok(None)
    }

    /// Check if a line could potentially be the start of caret tool syntax
    /// This is used to decide whether to buffer or emit content immediately
    fn could_be_tool_syntax_start(&self, line: &str) -> bool {
        // Empty line - could become anything
        if line.is_empty() {
            return false; // Empty lines are never tool syntax
        }

        // Check if line starts with caret characters
        if line.starts_with("^^^") {
            // If we have at least 3 carets, check if it could be valid tool name
            let after_carets = &line[3..];
            if after_carets.is_empty() {
                return true; // Still building the tool name
            }

            // Check if what follows could be a valid tool name
            // Tool names must be alphanumeric + underscore
            after_carets
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
        } else if line.starts_with("^^") {
            // Could become ^^^tool_name
            true
        } else if line.starts_with("^") {
            // Could become ^^^tool_name
            true
        } else {
            // Doesn't start with caret, definitely not tool syntax
            false
        }
    }

    /// Determines what content to emit when collecting multiline parameter content
    /// We can stream content but need to watch for potential end markers
    fn get_content_to_emit_multiline(&self) -> Result<Option<String>, UIError> {
        if let ParserState::CollectingMultiline { param_name, .. } = &self.state {
            // Look for potential end markers: "--- param_name"
            if let Some(newline_pos) = self.buffer.find('\n') {
                let line_content = &self.buffer[..newline_pos];

                // Check if this line could be the end marker
                if let Some(caps) = self.multiline_end_regex.captures(line_content) {
                    if caps.get(1).map_or(false, |m| m.as_str() == param_name) {
                        // This is the end marker, emit the complete line for processing
                        return Ok(Some(self.buffer[..=newline_pos].to_string()));
                    }
                }

                // Not an end marker, we can emit this line as part of the content
                // But we'll let the normal processing handle it to build up the content properly
                return Ok(Some(self.buffer[..=newline_pos].to_string()));
            }

            // No complete line yet, check if we have partial content that could be an end marker
            if self.buffer.trim().starts_with("---") {
                // Might be start of end marker, keep buffering
                return Ok(None);
            }

            // Partial content that definitely isn't an end marker could be emitted,
            // but for now let's be conservative and wait for complete lines
            Ok(None)
        } else {
            Ok(None)
        }
    }

    /// Determines what content to emit when collecting parameter values
    /// We can stream value content but watch for line boundaries
    fn get_content_to_emit_value(&self) -> Result<Option<String>, UIError> {
        // For now, be conservative and wait for complete lines
        // This could be enhanced to stream value content more aggressively
        if let Some(newline_pos) = self.buffer.find('\n') {
            let line_content = &self.buffer[..=newline_pos];
            return Ok(Some(line_content.to_string()));
        }
        Ok(None)
    }

    fn process_line(&mut self, line: &str) -> Result<(), UIError> {
        let state = self.state.clone();
        match state {
            ParserState::OutsideTool => self.process_line_outside_tool(line),
            ParserState::InsideTool => self.process_line_inside_tool(line),

            // New granular states for enhanced streaming
            ParserState::CollectingName {
                partial_name: _,
                tool_id: _,
            } => {
                // For now, treat this the same as InsideTool - this could be enhanced later
                self.state = ParserState::InsideTool;
                self.process_line_inside_tool(line)
            }

            ParserState::CollectingType {
                param_name: _,
                tool_id: _,
            } => {
                // For now, treat this the same as InsideTool - this could be enhanced later
                self.state = ParserState::InsideTool;
                self.process_line_inside_tool(line)
            }

            ParserState::CollectingValue {
                param_name: _,
                tool_id: _,
            } => {
                // For now, treat this the same as InsideTool - this could be enhanced later
                self.state = ParserState::InsideTool;
                self.process_line_inside_tool(line)
            }

            ParserState::CollectingMultiline {
                param_name,
                mut content,
                tool_id,
            } => {
                if let Some(caps) = self.multiline_end_regex.captures(line) {
                    if caps.get(1).map_or(false, |m| m.as_str() == param_name) {
                        self.send_tool_parameter(&param_name, content.trim_end(), &tool_id)?;
                        self.state = ParserState::InsideTool;
                    } else {
                        content.push_str(line);
                        content.push('\n');
                        self.state = ParserState::CollectingMultiline {
                            param_name,
                            content,
                            tool_id,
                        };
                    }
                } else {
                    content.push_str(line);
                    content.push('\n');
                    self.state = ParserState::CollectingMultiline {
                        param_name,
                        content,
                        tool_id,
                    };
                }
                Ok(())
            }
            ParserState::CollectingArray {
                param_name,
                mut elements,
                tool_id,
            } => {
                if line.trim() == "]" {
                    let value = format!(
                        "[{}]",
                        elements
                            .iter()
                            .map(|e| format!("\"{}\"", e))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    self.send_tool_parameter(&param_name, &value, &tool_id)?;
                    self.state = ParserState::InsideTool;
                } else if !line.trim().is_empty() {
                    elements.push(line.trim().to_string());
                    self.state = ParserState::CollectingArray {
                        param_name,
                        elements,
                        tool_id,
                    };
                }
                Ok(())
            }
        }
    }

    fn process_line_outside_tool(&mut self, line: &str) -> Result<(), UIError> {
        if let Some(caps) = self.tool_regex.captures(line) {
            let tool_name = caps.get(1).unwrap().as_str();
            self.current_tool_id = format!("{}_{}", tool_name, self.request_id);
            self.send_tool_start(tool_name, &self.current_tool_id)?;
            self.state = ParserState::InsideTool;
        } else {
            self.send_plain_text(&format!("{}\n", line))?;
        }
        Ok(())
    }

    fn process_line_inside_tool(&mut self, line: &str) -> Result<(), UIError> {
        if line == "^^^" {
            self.send_tool_end(&self.current_tool_id)?;
            self.state = ParserState::OutsideTool;
            // Note: Any trailing newlines after this will be buffered and trimmed naturally
        } else if let Some(caps) = self.multiline_start_regex.captures(line) {
            let param_name = caps.get(1).unwrap().as_str().to_string();
            self.state = ParserState::CollectingMultiline {
                param_name,
                content: String::new(),
                tool_id: self.current_tool_id.clone(),
            };
        } else if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim();
            if value == "[" {
                self.state = ParserState::CollectingArray {
                    param_name: key,
                    elements: vec![],
                    tool_id: self.current_tool_id.clone(),
                };
            } else {
                self.send_tool_parameter(&key, value, &self.current_tool_id)?;
            }
        }
        Ok(())
    }

    fn parse_tool_parameters(&self, content: &str) -> Result<Vec<(String, String)>, UIError> {
        let mut params = Vec::new();
        let mut lines = content.lines().peekable();
        while let Some(line) = lines.next() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim();
                if value == "[" {
                    let mut elements = vec![];
                    while let Some(element_line) = lines.next() {
                        if element_line.trim() == "]" {
                            break;
                        }
                        elements.push(format!("\"{}\"", element_line.trim()));
                    }
                    params.push((key, format!("[{}]", elements.join(","))));
                } else {
                    params.push((key, value.to_string()));
                }
            } else if let Some(caps) = self.multiline_start_regex.captures(line) {
                let param_name = caps.get(1).unwrap().as_str();
                let mut content = String::new();
                while let Some(content_line) = lines.next() {
                    if let Some(end_caps) = self.multiline_end_regex.captures(content_line) {
                        if end_caps.get(1).map_or(false, |m| m.as_str() == param_name) {
                            break;
                        }
                    }
                    content.push_str(content_line);
                    content.push('\n');
                }
                params.push((param_name.to_string(), content.trim_end().to_string()));
            }
        }
        Ok(params)
    }

    fn send_plain_text(&self, text: &str) -> Result<(), UIError> {
        if !text.is_empty() {
            self.ui
                .display_fragment(&DisplayFragment::PlainText(text.to_string()))?;
        }
        Ok(())
    }

    fn send_tool_start(&self, name: &str, id: &str) -> Result<(), UIError> {
        self.ui.display_fragment(&DisplayFragment::ToolName {
            name: name.to_string(),
            id: id.to_string(),
        })?;
        Ok(())
    }

    fn send_tool_parameter(&self, name: &str, value: &str, tool_id: &str) -> Result<(), UIError> {
        self.ui.display_fragment(&DisplayFragment::ToolParameter {
            name: name.to_string(),
            value: value.to_string(),
            tool_id: tool_id.to_string(),
        })?;
        Ok(())
    }

    fn send_tool_end(&self, id: &str) -> Result<(), UIError> {
        self.ui
            .display_fragment(&DisplayFragment::ToolEnd { id: id.to_string() })?;
        Ok(())
    }
}
