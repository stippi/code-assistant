//! Caret-style tool invocation processor for streaming responses
use crate::tools::core::{ToolRegistry, ToolScope};
use crate::tools::tool_use_filter::{SmartToolFilter, ToolUseFilter};
use crate::ui::streaming::{DisplayFragment, StreamProcessorTrait};
use crate::ui::{UIError, UserInterface};
use llm::{Message, MessageContent, StreamingChunk};
use regex::Regex;
use std::sync::Arc;

#[derive(Debug, PartialEq, Clone)]
enum ParserState {
    OutsideTool,
    InsideTool,
    #[allow(dead_code)]
    CollectingName {
        partial_name: String,
        tool_id: String,
    },
    #[allow(dead_code)]
    CollectingType {
        param_name: String,
        tool_id: String,
    },
    #[allow(dead_code)]
    CollectingValue {
        param_name: String,
        tool_id: String,
    },
    CollectingMultiline {
        param_name: String,
        content: String,
        tool_id: String,
        streamed: bool, // Track if we've already streamed chunks
    },
    CollectingArray {
        param_name: String,
        elements: Vec<String>,
        tool_id: String,
    },
}

/// Streaming state for managing tool filtering and buffering
#[derive(Debug, PartialEq, Clone)]
enum StreamingState {
    /// Stream everything immediately (before any tools)
    PreFirstTool,
    /// Buffer content between tools until next tool is evaluated
    BufferingAfterTool {
        last_tool_name: String,
        buffered_fragments: Vec<DisplayFragment>,
    },
    /// Stop streaming (tool was denied)
    Blocked,
}

pub struct CaretStreamProcessor {
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
    tool_counter: u64,
    buffer: String,
    state: ParserState,
    tool_regex: Regex,
    multiline_start_regex: Regex,
    multiline_end_regex: Regex,
    current_tool_id: String,
    current_tool_name: String,
    current_tool_hidden: bool,
    filter: Box<dyn ToolUseFilter>,
    streaming_state: StreamingState,
}

impl StreamProcessorTrait for CaretStreamProcessor {
    fn new(ui: Arc<Box<dyn UserInterface>>, request_id: u64) -> Self {
        Self {
            ui,
            request_id,
            tool_counter: 0,
            buffer: String::new(),
            state: ParserState::OutsideTool,
            tool_regex: Regex::new(r"^\^\^\^([a-zA-Z0-9_]+)$").unwrap(),
            multiline_start_regex: Regex::new(r"^([a-zA-Z0-9_]+)\s+---$").unwrap(),
            multiline_end_regex: Regex::new(r"^---\s+([a-zA-Z0-9_]+)$").unwrap(),
            current_tool_id: String::new(),
            current_tool_name: String::new(),
            current_tool_hidden: false,
            filter: Box::new(SmartToolFilter::new()),
            streaming_state: StreamingState::PreFirstTool,
        }
    }

    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        match chunk {
            StreamingChunk::Text(text) => {
                self.buffer.push_str(text);
                self.process_buffer()?;
            }
            StreamingChunk::StreamingComplete => {
                // Emit any remaining buffered fragments since no more content is coming
                self.flush_buffered_content()?;
            }
            _ => {
                // Other chunk types are not handled by caret processor
            }
        }
        Ok(())
    }

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        // For User messages, don't process caret syntax - just create a single PlainText fragment
        if message.role == llm::MessageRole::User {
            match &message.content {
                MessageContent::Text(text) => {
                    if !text.trim().is_empty() {
                        fragments.push(DisplayFragment::PlainText(text.clone()));
                    }
                }
                MessageContent::Structured(blocks) => {
                    // For structured user messages, combine all text into a single fragment
                    let mut combined_text = String::new();
                    for block in blocks {
                        match block {
                            llm::ContentBlock::Text { text } => {
                                combined_text.push_str(text);
                            }
                            llm::ContentBlock::ToolResult { content, .. } => {
                                // Include tool result content for user messages
                                combined_text.push_str(content);
                            }
                            _ => {} // Skip other block types for user messages
                        }
                    }
                    if !combined_text.trim().is_empty() {
                        fragments.push(DisplayFragment::PlainText(combined_text));
                    }
                }
            }
        } else {
            // For Assistant messages, process normally with caret syntax parsing
            match &message.content {
                MessageContent::Text(text) => {
                    // Process text for caret syntax
                    fragments.extend(self.extract_fragments_from_text(text, message.request_id)?);
                }
                MessageContent::Structured(blocks) => {
                    for block in blocks {
                        match block {
                            llm::ContentBlock::Thinking { thinking, .. } => {
                                fragments.push(DisplayFragment::ThinkingText(thinking.clone()));
                            }
                            llm::ContentBlock::Text { text } => {
                                // Process text for caret syntax
                                fragments.extend(
                                    self.extract_fragments_from_text(text, message.request_id)?,
                                );
                            }
                            llm::ContentBlock::ToolUse { id, name, input } => {
                                // Check if tool is hidden
                                let tool_hidden =
                                    ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent);

                                // Only add fragments if tool is not hidden
                                if !tool_hidden {
                                    // Convert JSON ToolUse to caret-style fragments
                                    fragments.push(DisplayFragment::ToolName {
                                        name: name.clone(),
                                        id: id.clone(),
                                    });

                                    // Parse JSON input into caret-style tool parameters
                                    if let Some(obj) = input.as_object() {
                                        for (key, value) in obj {
                                            let value_str = if value.is_string() {
                                                value.as_str().unwrap_or("").to_string()
                                            } else {
                                                value.to_string()
                                            };

                                            fragments.push(DisplayFragment::ToolParameter {
                                                name: key.clone(),
                                                value: value_str,
                                                tool_id: id.clone(),
                                            });
                                        }
                                    }

                                    fragments.push(DisplayFragment::ToolEnd { id: id.clone() });
                                }
                            }
                            llm::ContentBlock::ToolResult { .. } => {
                                // Tool results are typically not part of assistant messages
                            }
                            llm::ContentBlock::RedactedThinking { .. } => {
                                // Redacted thinking blocks are not displayed
                            }
                            llm::ContentBlock::Image { media_type, data } => {
                                // Images in assistant messages - preserve for display
                                fragments.push(DisplayFragment::Image {
                                    media_type: media_type.clone(),
                                    data: data.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(fragments)
    }
}

impl CaretStreamProcessor {
    /// Extract fragments from text without sending to UI (used for session loading)
    fn extract_fragments_from_text(
        &self,
        text: &str,
        request_id: Option<u64>,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        let mut current_pos = 0;
        let mut tool_counter = 1;
        let request_id = request_id.unwrap_or(1);

        while current_pos < lines.len() {
            // Look for tool start pattern: ^^^tool_name on its own line
            if let Some(tool_start_idx) = self.find_tool_start(&lines[current_pos..]) {
                let absolute_tool_start = current_pos + tool_start_idx;

                // Add any plain text before the tool block
                if tool_start_idx > 0 {
                    let plain_text_lines = &lines[current_pos..absolute_tool_start];
                    let plain_text = plain_text_lines.join("\n");
                    if !plain_text.trim().is_empty() {
                        fragments.push(DisplayFragment::PlainText(plain_text));
                    }
                }

                // Parse the tool block
                if let Some((tool_fragments, tool_end_idx)) =
                    self.parse_tool_block(&lines[absolute_tool_start..], request_id, tool_counter)?
                {
                    fragments.extend(tool_fragments);
                    current_pos = absolute_tool_start + tool_end_idx + 1; // +1 to skip the ^^^ line
                    tool_counter += 1;
                } else {
                    // No valid tool block found, treat as plain text
                    fragments.push(DisplayFragment::PlainText(
                        lines[absolute_tool_start].to_string(),
                    ));
                    current_pos = absolute_tool_start + 1;
                }
            } else {
                // No more tool blocks, add remaining lines as plain text
                let remaining_lines = &lines[current_pos..];
                let remaining_text = remaining_lines.join("\n");
                if !remaining_text.trim().is_empty() {
                    fragments.push(DisplayFragment::PlainText(remaining_text));
                }
                break;
            }
        }

        Ok(fragments)
    }

    /// Find the next tool start pattern in the given lines
    /// Returns the relative index of the line containing ^^^tool_name
    fn find_tool_start(&self, lines: &[&str]) -> Option<usize> {
        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if self.tool_regex.is_match(trimmed) {
                return Some(idx);
            }
        }
        None
    }

    /// Parse a complete tool block starting from ^^^tool_name
    /// Returns (fragments, end_line_index) where end_line_index is the index of the closing ^^^
    fn parse_tool_block(
        &self,
        lines: &[&str],
        request_id: u64,
        tool_counter: u64,
    ) -> Result<Option<(Vec<DisplayFragment>, usize)>, UIError> {
        if lines.is_empty() {
            return Ok(None);
        }

        // First line should be ^^^tool_name
        let first_line = lines[0].trim();
        if let Some(caps) = self.tool_regex.captures(first_line) {
            let tool_name = caps.get(1).unwrap().as_str();
            let tool_id = format!("tool-{request_id}-{tool_counter}");

            // Find the closing ^^^
            let mut tool_end_idx = None;
            for (idx, line) in lines.iter().enumerate().skip(1) {
                if line.trim() == "^^^" {
                    tool_end_idx = Some(idx);
                    break;
                }
            }

            if let Some(end_idx) = tool_end_idx {
                let mut fragments = Vec::new();

                // Add tool name fragment
                fragments.push(DisplayFragment::ToolName {
                    name: tool_name.to_string(),
                    id: tool_id.clone(),
                });

                // Parse parameters between start and end
                let param_lines = &lines[1..end_idx];
                let param_content = param_lines.join("\n");
                let params = self.parse_tool_parameters(&param_content)?;

                for (name, value) in params {
                    fragments.push(DisplayFragment::ToolParameter {
                        name,
                        value,
                        tool_id: tool_id.clone(),
                    });
                }

                // Add tool end fragment
                fragments.push(DisplayFragment::ToolEnd { id: tool_id });

                return Ok(Some((fragments, end_idx)));
            }
        }

        Ok(None)
    }
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
        if let Some(after_carets) = line.strip_prefix("^^^") {
            // If we have at least 3 carets, check if it could be valid tool name
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
            // Look for complete lines that we can process
            if let Some(newline_pos) = self.buffer.find('\n') {
                let line_content = &self.buffer[..newline_pos];

                // Check if this line is the end marker
                if let Some(caps) = self.multiline_end_regex.captures(line_content) {
                    if caps.get(1).is_some_and(|m| m.as_str() == param_name) {
                        // This is the end marker, emit the complete line for processing
                        return Ok(Some(self.buffer[..=newline_pos].to_string()));
                    }
                }

                // Not an end marker, we can emit this line as part of the content
                return Ok(Some(self.buffer[..=newline_pos].to_string()));
            }

            // No complete line yet, check if we should buffer
            // Only buffer if we might have a partial end marker
            if self.buffer.trim_end().starts_with("---") {
                // Might be start of end marker, keep buffering
                return Ok(None);
            }

            // Nothing unsafe, no need to emit partial content
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
                streamed: _,
            } => {
                if let Some(caps) = self.multiline_end_regex.captures(line) {
                    if caps.get(1).is_some_and(|m| m.as_str() == param_name) {
                        // End marker found, just transition back to InsideTool state
                        // No need to send final parameter since we've been streaming chunks
                        self.state = ParserState::InsideTool;
                    } else {
                        // Not the right end marker, treat as regular content
                        let is_first_line = content.is_empty();
                        content.push_str(line);
                        content.push('\n');
                        // Stream this line immediately as part of the parameter value
                        self.stream_multiline_content(line, &param_name, &tool_id, is_first_line)?;
                        self.state = ParserState::CollectingMultiline {
                            param_name,
                            content,
                            tool_id,
                            streamed: true,
                        };
                    }
                } else {
                    // Regular content line, add to buffer
                    let is_first_line = content.is_empty();
                    content.push_str(line);
                    content.push('\n');
                    // Only stream for live processing, not for complete message processing
                    // The complete message processing will create a single final fragment in extract_fragments_from_text
                    self.stream_multiline_content(line, &param_name, &tool_id, is_first_line)?;
                    self.state = ParserState::CollectingMultiline {
                        param_name,
                        content,
                        tool_id,
                        streamed: true,
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
                            .map(|e| format!("\"{e}\""))
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
            self.tool_counter += 1; // Increment first, so first tool gets counter 1
            self.current_tool_id = format!("tool-{}-{}", self.request_id, self.tool_counter);
            self.current_tool_name = tool_name.to_string();

            // Check if tool is hidden and update state
            self.current_tool_hidden =
                ToolRegistry::global().is_tool_hidden(tool_name, ToolScope::Agent);

            let tool_id = self.current_tool_id.clone();
            self.send_tool_start(tool_name, &tool_id)?;
            self.state = ParserState::InsideTool;
        } else {
            self.send_plain_text(&format!("{line}\n"))?;
        }
        Ok(())
    }

    fn process_line_inside_tool(&mut self, line: &str) -> Result<(), UIError> {
        if line == "^^^" {
            let tool_id = self.current_tool_id.clone();
            self.send_tool_end(&tool_id)?;
            self.state = ParserState::OutsideTool;
            // Clear current tool info
            self.current_tool_name.clear();
            // Note: Any trailing newlines after this will be buffered and trimmed naturally
        } else if let Some(caps) = self.multiline_start_regex.captures(line) {
            let param_name = caps.get(1).unwrap().as_str().to_string();
            self.state = ParserState::CollectingMultiline {
                param_name,
                content: String::new(),
                tool_id: self.current_tool_id.clone(),
                streamed: false,
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
                let tool_id = self.current_tool_id.clone();
                self.send_tool_parameter(&key, value, &tool_id)?;
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
                    for element_line in lines.by_ref() {
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
                for content_line in lines.by_ref() {
                    if let Some(end_caps) = self.multiline_end_regex.captures(content_line) {
                        if end_caps.get(1).is_some_and(|m| m.as_str() == param_name) {
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

    /// Emit fragments through this central function that handles filtering and buffering
    fn emit_fragment(&mut self, fragment: DisplayFragment) -> Result<(), UIError> {
        // Filter out tool-related fragments for hidden tools
        if self.current_tool_hidden {
            match &fragment {
                DisplayFragment::ToolName { .. }
                | DisplayFragment::ToolParameter { .. }
                | DisplayFragment::ToolEnd { .. } => {
                    // Skip tool-related fragments for hidden tools
                    return Ok(());
                }
                _ => {
                    // Allow non-tool fragments even when current tool is hidden
                }
            }
        }

        match &self.streaming_state {
            StreamingState::Blocked => {
                // Already blocked, check if this is just whitespace and ignore silently
                if let DisplayFragment::PlainText(text) = &fragment {
                    if text.trim().is_empty() {
                        return Ok(());
                    }
                }
                // Non-whitespace content after blocking - return the blocking error
                return Err(UIError::IOError(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Tool limit reached - no additional text after complete tool block allowed",
                )));
            }
            StreamingState::PreFirstTool => {
                // Before first tool, emit everything immediately
                match &fragment {
                    DisplayFragment::ToolName { name, .. } => {
                        // First tool starting - check if it's allowed (should always be allowed)
                        if !self.filter.allow_tool_at_position(name, 1) {
                            // First tool denied - block streaming
                            self.streaming_state = StreamingState::Blocked;
                            return Err(UIError::IOError(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "Tool limit reached - no additional text after complete tool block allowed",
                            )));
                        }
                        // Emit the tool name fragment
                        self.ui.display_fragment(&fragment)?;
                    }
                    DisplayFragment::ToolEnd { .. } => {
                        // Tool ended - emit fragment and check if we should buffer after this tool
                        self.ui.display_fragment(&fragment)?;

                        // Get the tool name from current_tool_id or extract from somewhere
                        let tool_name = self.extract_tool_name_from_current_context();

                        if self.filter.allow_content_after_tool(&tool_name, 1) {
                            // Transition to buffering state
                            self.streaming_state = StreamingState::BufferingAfterTool {
                                last_tool_name: tool_name,
                                buffered_fragments: Vec::new(),
                            };
                        } else {
                            // No content allowed after this tool - block
                            self.streaming_state = StreamingState::Blocked;
                        }
                    }
                    _ => {
                        // Regular fragment - emit immediately
                        self.ui.display_fragment(&fragment)?;
                    }
                }
            }
            StreamingState::BufferingAfterTool {
                last_tool_name,
                buffered_fragments,
            } => {
                match &fragment {
                    DisplayFragment::ToolName { name, .. } => {
                        // New tool starting - check if it's allowed
                        let tool_count = self.tool_counter + 1; // Next tool count
                        if self
                            .filter
                            .allow_tool_at_position(name, tool_count as usize)
                        {
                            // Tool allowed - emit all buffered fragments first
                            let mut buffered = buffered_fragments.clone();
                            for buffered_fragment in buffered.drain(..) {
                                self.ui.display_fragment(&buffered_fragment)?;
                            }
                            // Then emit the tool name fragment
                            self.ui.display_fragment(&fragment)?;
                            // Update state to continue buffering after this tool
                            self.streaming_state = StreamingState::BufferingAfterTool {
                                last_tool_name: name.clone(),
                                buffered_fragments: Vec::new(),
                            };
                        } else {
                            // Tool denied - discard buffered content and block
                            self.streaming_state = StreamingState::Blocked;
                            return Err(UIError::IOError(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "Tool limit reached - no additional text after complete tool block allowed",
                            )));
                        }
                    }
                    DisplayFragment::ToolEnd { .. } => {
                        // Tool ended - emit fragment and check if we should continue buffering
                        self.ui.display_fragment(&fragment)?;

                        let tool_name = last_tool_name.clone();
                        let tool_count = self.tool_counter as usize;

                        if self.filter.allow_content_after_tool(&tool_name, tool_count) {
                            // Continue buffering
                            self.streaming_state = StreamingState::BufferingAfterTool {
                                last_tool_name: tool_name,
                                buffered_fragments: Vec::new(),
                            };
                        } else {
                            // No content allowed after this tool - block
                            self.streaming_state = StreamingState::Blocked;
                        }
                    }
                    DisplayFragment::ToolParameter { .. } => {
                        // Tool parameter - emit immediately (we've already decided to allow the tool)
                        self.ui.display_fragment(&fragment)?;
                    }
                    DisplayFragment::PlainText(_) | DisplayFragment::ThinkingText(_) => {
                        // Text or thinking - buffer it until we know if next tool is allowed
                        if let StreamingState::BufferingAfterTool {
                            buffered_fragments, ..
                        } = &mut self.streaming_state
                        {
                            buffered_fragments.push(fragment);
                        }
                    }
                    DisplayFragment::Image { .. } => {
                        // Image - buffer it until we know if next tool is allowed
                        if let StreamingState::BufferingAfterTool {
                            buffered_fragments, ..
                        } = &mut self.streaming_state
                        {
                            buffered_fragments.push(fragment);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Extract tool name from current context (helper method)
    fn extract_tool_name_from_current_context(&self) -> String {
        self.current_tool_name.clone()
    }

    /// Flush any buffered content when streaming completes
    fn flush_buffered_content(&mut self) -> Result<(), UIError> {
        if let StreamingState::BufferingAfterTool {
            buffered_fragments, ..
        } = &mut self.streaming_state
        {
            // Emit all buffered fragments since no more tools are coming
            for fragment in buffered_fragments.drain(..) {
                self.ui.display_fragment(&fragment)?;
            }
        }
        Ok(())
    }

    fn send_plain_text(&mut self, text: &str) -> Result<(), UIError> {
        if !text.is_empty() {
            self.emit_fragment(DisplayFragment::PlainText(text.to_string()))?;
        }
        Ok(())
    }

    fn send_tool_start(&mut self, name: &str, id: &str) -> Result<(), UIError> {
        self.emit_fragment(DisplayFragment::ToolName {
            name: name.to_string(),
            id: id.to_string(),
        })
    }

    fn send_tool_parameter(
        &mut self,
        name: &str,
        value: &str,
        tool_id: &str,
    ) -> Result<(), UIError> {
        self.emit_fragment(DisplayFragment::ToolParameter {
            name: name.to_string(),
            value: value.to_string(),
            tool_id: tool_id.to_string(),
        })
    }

    fn send_tool_end(&mut self, id: &str) -> Result<(), UIError> {
        self.emit_fragment(DisplayFragment::ToolEnd { id: id.to_string() })
    }

    /// Stream multiline content immediately as part of a parameter value
    fn stream_multiline_content(
        &mut self,
        content: &str,
        param_name: &str,
        tool_id: &str,
        is_first_line: bool,
    ) -> Result<(), UIError> {
        // Add newline prefix for non-first lines to maintain line separation when fragments are merged
        let content_to_stream = if is_first_line {
            content.to_string()
        } else {
            format!("\n{content}")
        };

        self.emit_fragment(DisplayFragment::ToolParameter {
            name: param_name.to_string(),
            value: content_to_stream,
            tool_id: tool_id.to_string(),
        })
    }
}
