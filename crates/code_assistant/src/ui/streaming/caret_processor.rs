
//! Caret-style tool invocation processor for streaming responses
//!
//! # Caret Syntax Design Principles
//!
//! The caret processor implements a line-oriented tool syntax where all elements
//! must appear on their own lines:
//!
//! ```text
//! ^^^tool_name
//! param1: value1
//! param2: [
//! element1
//! element2
//! ]
//! multiline_param ---
//! This is multiline content
//! More content here
//! --- multiline_param
//! ^^^
//! ```
//!
//! # Streaming Strategy
//!
//! Unlike the XML processor which uses complex tag-start detection, the caret
//! processor uses a much simpler approach:
//!
//! ## Core Principle: Only Buffer Potential Caret Lines
//!
//! 1. **Regular text lines**: Emit immediately with exact formatting preservation
//! 2. **Lines starting with "^^^"**: Hold in buffer until complete
//! 3. **Parser state awareness**: Track whether we're inside/outside tool blocks
//!
//! This approach ensures:
//! - Streaming performance: Most text flows through without buffering
//! - Exact whitespace preservation: No trimming or formatting changes
//! - Simple logic: Only caret-specific lines need special handling
//!
//! ## State Machine
//!
//! The processor maintains two main states:
//!
//! ### Outside Tool Block
//! - Lines starting with "^^^tool_name" → Start tool, emit ToolName fragment
//! - Lines starting with "^^^" (invalid) → Emit as plain text
//! - All other lines → Emit as plain text immediately
//!
//! ### Inside Tool Block
//! - Line "^^^" → End tool, emit ToolEnd fragment
//! - Lines "key: value" → Parse as parameters, emit ToolParameter fragments
//! - Lines "key: [" → Start array parameter collection
//! - Line "]" → End array parameter
//! - Lines "param ---" → Start multiline parameter collection
//! - Lines "--- param" → End multiline parameter
//! - All other lines → Either parameter content or emit as plain text
//!
//! ## Chunking Handling
//!
//! The key insight is that caret syntax is line-oriented, so we only need to
//! handle incomplete lines at chunk boundaries:
//!
//! 1. Process all complete lines (ending with \n) immediately
//! 2. Hold back incomplete lines that start with "^^^"
//! 3. Emit other incomplete lines as plain text (preserving formatting)
//! 4. When new chunks arrive, re-evaluate buffered content
//!
//! This is much simpler than XML tag detection because:
//! - No complex partial tag patterns to track
//! - No mid-line syntax to worry about
//! - Clear line boundaries make buffering decisions straightforward

use crate::ui::streaming::{DisplayFragment, StreamProcessorTrait};
use crate::ui::{UIError, UserInterface};
use llm::{Message, StreamingChunk};
use regex::Regex;
use std::sync::Arc;

/// Stream processor for caret-style tool invocations (^^^tool_name)
///
/// # Parser State Management
///
/// The processor maintains explicit state about whether it's currently inside
/// a tool block or outside. This is crucial for determining how to interpret
/// incoming lines:
///
/// - **Outside tool**: Only lines starting with "^^^tool_name" are special
/// - **Inside tool**: Various parameter patterns need recognition
///
/// This state-driven approach prevents false positives and ensures that
/// parameter parsing only happens when appropriate.
pub struct CaretStreamProcessor {
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,

    /// Buffer for incomplete lines that might be caret syntax
    /// Only holds content when the last line starts with "^^^"
    buffer: String,

    /// Compiled regexes for efficient pattern matching
    tool_regex: Regex,                  // Matches "^^^tool_name" patterns
    multiline_start_regex: Regex,       // Matches "param ---" patterns
    multiline_end_regex: Regex,         // Matches "--- param" patterns

    /// Current parser state - tracks whether we're inside a tool block
    /// This is essential for determining how to process incoming lines
    current_tool: Option<ToolState>,
}

/// Represents the state when we're inside a tool block
///
/// This tracks both the tool metadata (for generating fragment IDs) and
/// any in-progress parameter parsing (like multiline content collection).
#[derive(Debug, Clone)]
struct ToolState {
    name: String,                       // Tool name for debugging
    id: String,                         // Fragment ID for UI consistency
    parameters: Vec<(String, String)>,  // Collected parameters
    current_multiline: Option<MultilineState>, // In-progress multiline param
}

/// State for collecting multiline parameter content
///
/// When we encounter "param ---", we start collecting content until
/// we see "--- param". This struct tracks that collection process.
#[derive(Debug, Clone)]
struct MultilineState {
    param_name: String,                 // Which parameter we're collecting
    content: String,                    // Accumulated content
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
    /// Core processing logic implementing the streaming strategy
    ///
    /// # Algorithm Overview
    ///
    /// 1. **Process complete lines**: Any line ending with \n gets processed immediately
    /// 2. **State-aware parsing**: Use current_tool state to determine interpretation
    /// 3. **Selective buffering**: Only hold back incomplete lines starting with "^^^"
    /// 4. **Exact formatting**: Preserve all whitespace and formatting in regular text
    ///
    /// # State Transitions
    ///
    /// ## Outside Tool Block (current_tool = None)
    /// - Line "^^^tool_name" → Enter tool block, emit ToolName
    /// - Line "^^^" (bare) → Treat as plain text (invalid)
    /// - Any other line → Emit as plain text
    ///
    /// ## Inside Tool Block (current_tool = Some(...))
    /// - Line "^^^" → Exit tool block, emit ToolEnd
    /// - Line "key: value" → Parse parameter, emit ToolParameter
    /// - Line "key: [" → Start array collection (TODO: implement)
    /// - Line "]" → End array collection (TODO: implement)
    /// - Line "param ---" → Start multiline collection (TODO: implement)
    /// - Line "--- param" → End multiline collection (TODO: implement)
    /// - Other lines → Could be multiline content or emit as plain text
    ///
    /// # Chunking Edge Cases
    ///
    /// The most complex case is when a caret line spans chunk boundaries:
    ///
    /// Chunk 1: "Regular text\n^^"
    /// Chunk 2: "^tool_name\nparameter: value\n^^^"
    ///
    /// Our algorithm handles this by:
    /// 1. Processing "Regular text\n" immediately
    /// 2. Buffering "^^" (starts with "^")
    /// 3. When Chunk 2 arrives, "^^^tool_name" is now complete
    /// 4. Processing continues with the complete line
    fn process_buffer(&mut self) -> Result<(), UIError> {
        // Process complete lines, hold back incomplete lines that could be caret syntax

        let content = self.buffer.clone();
        let mut processed_until = 0;

        // Process line by line
        while processed_until < content.len() {
            if let Some(newline_pos) = content[processed_until..].find('\n') {
                let line_start = processed_until;
                let line_end = processed_until + newline_pos;
                let line_content = &content[line_start..line_end];

                // State-driven line interpretation
                if line_content.starts_with("^^^") {
                    self.process_caret_line(line_content)?;
                } else if self.current_tool.is_some() {
                    // Inside tool block - check for parameter patterns
                    self.process_tool_parameter_line(line_content)?;
                } else {
                    // Outside tool block - emit as plain text with newline
                    self.send_plain_text(&content[line_start..=line_end])?;
                }

                processed_until = line_end + 1; // Move past the newline
            } else {
                // Incomplete line at end - apply buffering strategy
                let remaining_line = &content[processed_until..];

                if remaining_line.starts_with("^^^") {
                    // Potential caret syntax - hold in buffer
                    break;
                } else if remaining_line.is_empty() {
                    // Nothing left to process
                    processed_until = content.len();
                } else {
                    // Regular text - emit immediately (preserving exact formatting)
                    self.send_plain_text(remaining_line)?;
                    processed_until = content.len();
                }
                break;
            }
        }

        // Update buffer with unprocessed content
        self.buffer = content[processed_until..].to_string();
        Ok(())
    }

    /// Process a line that starts with "^^^"
    ///
    /// This handles both tool start and tool end patterns.
    /// Invalid patterns (like "^^^invalid syntax") are treated as plain text.
    fn process_caret_line(&mut self, line: &str) -> Result<(), UIError> {
        if line == "^^^" {
            // Tool end
            if let Some(tool) = &self.current_tool {
                self.send_tool_end(&tool.id)?;
            }
            self.current_tool = None;
        } else if let Some(caps) = self.tool_regex.captures(line) {
            // Tool start: "^^^tool_name"
            if let Some(tool_name) = caps.get(1) {
                let tool_id = format!("{}_{}", tool_name.as_str(), self.request_id);
                self.send_tool_start(tool_name.as_str(), &tool_id)?;

                self.current_tool = Some(ToolState {
                    name: tool_name.as_str().to_string(),
                    id: tool_id,
                    parameters: Vec::new(),
                    current_multiline: None,
                });
            }
        } else {
            // Invalid caret syntax - treat as plain text
            self.send_plain_text(&format!("{}\n", line))?;
        }
        Ok(())
    }

    /// Process a line when we're inside a tool block
    ///
    /// This is where parameter parsing happens. The current implementation
    /// is simplified and needs to be extended for:
    /// - Array parameters (key: [ ... ])
    /// - Multiline parameters (key --- ... --- key)
    /// - Proper error handling for malformed parameters
    fn process_tool_parameter_line(&mut self, line: &str) -> Result<(), UIError> {
        // TODO: Implement full parameter parsing
        // For now, just handle simple "key: value" patterns

        if let Some((key, value)) = self.parse_simple_parameter(line) {
            if value == "[" {
                // Array start - TODO: implement array collection
                self.add_parameter(key, "[".to_string())?;
            } else {
                self.add_parameter(key, value)?;
            }
        } else if let Some(_caps) = self.multiline_start_regex.captures(line) {
            // Multiline parameter start - TODO: implement multiline collection
            // For now, just ignore these patterns
        }
        // If no patterns match, the line might be:
        // - Part of multiline content (if we're collecting)
        // - Array content (if we're in an array)
        // - Invalid syntax (should emit as plain text)

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
        // Always send text as-is, even if it's just whitespace
        if !text.is_empty() {
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
