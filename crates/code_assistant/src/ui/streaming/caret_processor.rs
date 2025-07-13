
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
//! # Implementation Status
//!
//! ## ✅ Working Features (7/21 tests passing)
//!
//! - **Basic text processing**: Regular text without caret syntax flows through correctly
//! - **Tool recognition**: `^^^tool_name` at line start properly recognized
//! - **Simple tool invocation**: Basic tool start/end processing works
//! - **Line positioning validation**: Caret syntax only recognized at line start
//! - **Message extraction**: Complete message parsing works correctly
//! - **All parsing tests**: Non-streaming parser implementation is complete
//!
//! ## ❌ Known Issues & TODOs
//!
//! ### 1. Buffering Strategy (Critical)
//! The current buffering logic is **too conservative** for small chunk sizes:
//! - ✅ Chunk size 1: Works correctly
//! - ❌ Chunk size 2+: Buffers entire input instead of processing line-by-line
//! - **Root cause**: `should_buffer_incomplete_line()` is too aggressive
//! - **Impact**: 9/21 tests failing due to chunking issues
//!
//! ### 2. Parameter Parsing (High Priority)
//! Inside tool blocks, parameter lines are not being processed:
//! - ❌ `"project: test"` → emitted as PlainText instead of ToolParameter
//! - **Missing**: Proper parameter recognition and parsing within tool blocks
//! - **Impact**: Most tool functionality tests fail
//!
//! ### 3. Advanced Parameter Types (Medium Priority)
//! - ❌ **Array parameters**: `key: [elem1, elem2]` syntax not implemented
//! - ❌ **Multiline parameters**: `key ---\ncontent\n--- key` syntax not implemented
//! - **Status**: Infrastructure exists but parsing logic incomplete
//!
//! ### 4. Buffer Finalization (Low Priority)
//! - ❌ Incomplete tools at end of stream not handled properly
//! - **Issue**: No proper finalization when streaming ends abruptly
//! - **Impact**: Edge case failures in buffer completion tests
//!
//! # Streaming Strategy
//!
//! ## Core Principle: Smart Buffering at Syntax Boundaries
//!
//! The processor needs to buffer potential tool syntax boundaries intelligently.
//! Here are the **specific buffering rules** to implement:
//!
//! ### Tool Invocation Boundary Buffering
//!
//! When receiving chunks ending with these patterns, buffer as follows:
//! - `"some random text\n"` → emit "some random text", buffer "\n" (potential line start)
//! - `"some random text\nStart of next line"` → emit the whole thing (complete line, no ambiguity)
//! - `"some random text\n^"` → emit "some random text", buffer "\n^" (potential tool start)
//! - `"some random text\n^^"` → emit "some random text", buffer "\n^^" (potential tool start)
//! - `"some random text\n^^^"` → emit "some random text", buffer "\n^^^" (potential tool start)
//! - `"some random text\n^^^read_fi"` → emit "some random text", buffer "\n^^^read_fi" (incomplete tool name)
//! - `"some random text\n^1"` → emit the whole thing (definitely not tool syntax - starts with digit)
//!
//! ### Continued Buffering with Partial State
//!
//! When already buffered content receives more chunks:
//! - Already buffered `"\n^^"`, receiving `"^rea"` → keep buffering until complete tool block starting line
//! - Keep buffering until receiving complete tool name with line break: `"^^^tool_name\n"`
//!
//! ### Parameter Boundary Buffering (Inside Tool Blocks)
//!
//! When parser state is inside a tool block, apply similar buffering for parameters:
//! - **Single-line parameters**: Buffer until seeing `":"`, then can emit parameter name and stream value chunks
//! - **Need complete parameter name** before emitting, since value chunks must be associated with established parameter name
//! - **Multiline parameters**: Stream value chunks, but buffer lines ending with `"\n-"`, `"\n--"`, etc.
//! - **Buffer until certain** the line is not marking end of multiline parameter (`"--- param_name"`)
//!
//! ### Key Insight: Conservative Buffering Only at Boundaries
//!
//! - **Buffer conservatively** when content could be start of tool/parameter syntax
//! - **Process aggressively** when enough information available to make decision
//! - **Emit immediately** when content definitely cannot be tool syntax
//! - **Line-oriented approach** - buffer only at line boundaries where syntax could start
//!
//! ## State Machine
//!
//! ### Outside Tool Block
//! - Lines starting with "^^^tool_name" → Enter tool block, emit ToolName
//! - Lines starting with "^^^" (bare) → Treat as plain text (invalid outside tool)
//! - All other lines → Emit as plain text immediately
//!
//! ### Inside Tool Block
//! - Line "^^^" → Exit tool block, emit ToolEnd
//! - Lines "key: value" → Parse parameter, emit ToolParameter
//! - Lines "key: [" → Start array parameter collection (TODO)
//! - Line "]" → End array parameter (TODO)
//! - Lines "param ---" → Start multiline parameter collection (TODO)
//! - Lines "--- param" → End multiline parameter (TODO)
//! - Other lines → Parameter content or emit as plain text
//!
//! ## Critical Insight: Buffering vs Processing Balance
//!
//! The key challenge is determining when to buffer vs when to process:
//! - **Too conservative**: Everything gets buffered, nothing processes
//! - **Too aggressive**: Tool syntax gets split and emitted as plain text
//! - **Sweet spot**: Buffer only when truly ambiguous, process when sufficient info available
//!
//! **Current Issue**: The processor errs too much on the conservative side,
//! especially for small chunk sizes, leading to entire inputs being buffered
//! instead of processed incrementally.

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
    current_array: Option<ArrayState>,  // In-progress array param
}

/// State for collecting array parameter content
#[derive(Debug, Clone)]
struct ArrayState {
    param_name: String,                 // Which parameter we're collecting
    elements: Vec<String>,              // Accumulated array elements
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
    /// # Buffering Strategy
    ///
    /// The key insight is that we need to buffer potential tool syntax boundaries
    /// and parameter boundaries, not just incomplete lines. Examples:
    ///
    /// - "text\n" → emit "text", buffer "\n" (potential line start)
    /// - "text\n^" → emit "text", buffer "\n^" (potential tool start)
    /// - "text\n^^^read_fi" → emit "text", buffer "\n^^^read_fi" (incomplete tool)
    /// - "text\n^1" → emit "text\n^1" (definitely not tool syntax)
    ///
    /// Inside tool blocks, similar logic applies to parameters:
    /// - "param:" → buffer until complete "param: value"
    /// - "---" → buffer until complete "--- param" (multiline end)
    ///
    /// This ensures we never emit fragments of tool syntax or parameters.
    fn process_buffer(&mut self) -> Result<(), UIError> {
        let content = self.buffer.clone();
        let mut processed_until = 0;

        while processed_until < content.len() {
            if let Some(newline_pos) = content[processed_until..].find('\n') {
                // We have a complete line
                let line_start = processed_until;
                let line_end = processed_until + newline_pos;
                let line_content = &content[line_start..line_end];

                // Check if we need to buffer this line based on what follows
                let remaining_after_newline = &content[line_end + 1..];

                if self.should_buffer_at_line_boundary(line_content, remaining_after_newline) {
                    // Buffer this line and what follows - incomplete tool/param syntax
                    break;
                }

                // Process the complete line
                if line_content.starts_with("^^^") {
                    self.process_caret_line(line_content)?;
                } else if self.current_tool.is_some() {
                    self.process_tool_parameter_line(line_content)?;
                } else {
                    // Outside tool block - emit as plain text with newline
                    self.send_plain_text(&content[line_start..=line_end])?;
                }

                processed_until = line_end + 1; // Move past the newline
            } else {
                // No more complete lines - handle remaining content
                let remaining = &content[processed_until..];

                if self.should_buffer_incomplete_line(remaining) {
                    // Buffer incomplete content that might be extended
                    break;
                } else if !remaining.is_empty() {
                    // Emit remaining content that can't be tool syntax
                    self.send_plain_text(remaining)?;
                    processed_until = content.len();
                }
                break;
            }
        }

        // Update buffer with unprocessed content
        self.buffer = content[processed_until..].to_string();
        Ok(())
    }

    /// Determine if we should buffer at a line boundary
    ///
    /// This handles cases like "text\n^" where we need to buffer the newline
    /// and the start of the potential tool syntax.
    fn should_buffer_at_line_boundary(&self, _line_content: &str, remaining_after_newline: &str) -> bool {
        // If the line ends and the next content starts with potential caret syntax,
        // we should buffer to avoid splitting tool boundaries
        if remaining_after_newline.starts_with("^") {
            // Quick check - if it starts with digit, it's definitely not tool syntax
            if remaining_after_newline.len() > 1 {
                let second_char = remaining_after_newline.chars().nth(1).unwrap();
                if second_char.is_ascii_digit() {
                    return false; // "^1", "^2", etc. - not tool syntax
                }
            }

            // If we have a complete tool pattern, we don't need to buffer
            if remaining_after_newline.starts_with("^^^") {
                // Check if this is a complete tool line or tool end
                if let Some(newline_pos) = remaining_after_newline.find('\n') {
                    let potential_tool_line = &remaining_after_newline[..newline_pos];
                    // Allow "^^^" (tool end) and "^^^tool_name" (tool start)
                    if potential_tool_line == "^^^" || self.tool_regex.is_match(potential_tool_line) {
                        return false; // Complete tool pattern, can process normally
                    }
                }
                // Incomplete tool pattern, buffer it
                return true;
            }

            // Other "^" patterns need buffering until we know what they become
            if remaining_after_newline.len() < 3 {
                return true; // "^", "^^" - too short to know
            }
        }

        // Inside tool blocks, buffer for potential parameter boundaries
        if self.current_tool.is_some() {
            // Buffer potential multiline parameter endings
            if remaining_after_newline.starts_with("-") && remaining_after_newline.len() < 4 {
                return true; // Could become "--- param"
            }
        }

        false
    }

    /// Determine if we should buffer an incomplete line (no trailing newline)
    fn should_buffer_incomplete_line(&self, remaining: &str) -> bool {
        if remaining.is_empty() {
            return false;
        }

        // Buffer potential caret syntax starts
        if remaining.starts_with("^") {
            // Quick check - if it starts with digit, it's definitely not tool syntax
            if remaining.len() > 1 {
                let second_char = remaining.chars().nth(1).unwrap();
                if second_char.is_ascii_digit() {
                    return false; // "^1", "^2", etc. - not tool syntax
                }
            }
            return true; // Could be "^", "^^", "^^^", "^^^tool" - buffer it
        }

        // Inside tool blocks, buffer potential parameter syntax
        if self.current_tool.is_some() {
            // Buffer potential parameter names that might contain ":"
            if !remaining.contains(':') &&
               remaining.chars().all(|c| c.is_alphanumeric() || c == '_' || c.is_whitespace()) {
                return true;
            }

            // Buffer potential multiline endings starting with "-"
            if remaining.starts_with("-") {
                return true;
            }
        }

        false
    }

    /// Process a line that starts with "^^^"
    ///
    /// This handles both tool start and tool end patterns.
    /// Invalid patterns (like "^^^invalid syntax") are treated as plain text.
    fn process_caret_line(&mut self, line: &str) -> Result<(), UIError> {
        if line == "^^^" {
            // Tool end - only valid inside a tool block
            if let Some(tool) = &self.current_tool {
                self.send_tool_end(&tool.id)?;
                self.current_tool = None;
            } else {
                // "^^^" outside tool block is invalid - treat as plain text
                self.send_plain_text(&format!("{}\n", line))?;
            }
        } else if let Some(caps) = self.tool_regex.captures(line) {
            // Tool start: "^^^tool_name" - only valid outside tool block
            if self.current_tool.is_none() {
                if let Some(tool_name) = caps.get(1) {
                    let tool_id = format!("{}_{}", tool_name.as_str(), self.request_id);
                    self.send_tool_start(tool_name.as_str(), &tool_id)?;

                    self.current_tool = Some(ToolState {
                        name: tool_name.as_str().to_string(),
                        id: tool_id,
                        parameters: Vec::new(),
                        current_multiline: None,
                        current_array: None,
                    });
                }
            } else {
                // Tool start inside tool block is invalid - treat as plain text
                self.send_plain_text(&format!("{}\n", line))?;
            }
        } else {
            // Invalid caret syntax - treat as plain text
            self.send_plain_text(&format!("{}\n", line))?;
        }
        Ok(())
    }

    /// Process a line when we're inside a tool block
    ///
    /// This handles parameter parsing including arrays and multiline parameters.
    fn process_tool_parameter_line(&mut self, line: &str) -> Result<(), UIError> {
        // First, handle any ongoing multiline or array collection
        if self.current_tool.is_some() {
            // Check multiline collection
            if let Some(ref multiline_state) = self.current_tool.as_ref().unwrap().current_multiline {
                // Look for the end marker: "--- param_name"
                if let Some(caps) = self.multiline_end_regex.captures(line) {
                    if let Some(end_param) = caps.get(1) {
                        if end_param.as_str() == multiline_state.param_name {
                            // End of multiline parameter
                            let param_name = multiline_state.param_name.clone();
                            let param_value = multiline_state.content.clone();

                            // Clear multiline state
                            if let Some(tool) = &mut self.current_tool {
                                tool.current_multiline = None;
                            }

                            self.add_parameter(param_name, param_value)?;
                            return Ok(());
                        }
                    }
                }

                // Add this line to multiline content
                if let Some(tool) = &mut self.current_tool {
                    if let Some(multiline_state) = &mut tool.current_multiline {
                        if !multiline_state.content.is_empty() {
                            multiline_state.content.push('\n');
                        }
                        multiline_state.content.push_str(line);
                    }
                }
                return Ok(());
            }

            // Check array collection
            if let Some(ref array_state) = self.current_tool.as_ref().unwrap().current_array {
                // Look for array end: "]"
                if line.trim() == "]" {
                    // End of array parameter
                    let param_name = array_state.param_name.clone();
                    let array_value = format!("[{}]", array_state.elements.join(", "));

                    // Clear array state
                    if let Some(tool) = &mut self.current_tool {
                        tool.current_array = None;
                    }

                    self.add_parameter(param_name, array_value)?;
                    return Ok(());
                }

                // Add this line as an array element (if not empty)
                let trimmed_line = line.trim();
                if !trimmed_line.is_empty() {
                    if let Some(tool) = &mut self.current_tool {
                        if let Some(array_state) = &mut tool.current_array {
                            array_state.elements.push(trimmed_line.to_string());
                        }
                    }
                }
                return Ok(());
            }
        }

        // Check for multiline parameter start: "param ---"
        if let Some(caps) = self.multiline_start_regex.captures(line) {
            if let Some(param_name) = caps.get(1) {
                if let Some(tool) = &mut self.current_tool {
                    tool.current_multiline = Some(MultilineState {
                        param_name: param_name.as_str().to_string(),
                        content: String::new(),
                    });
                }
                return Ok(());
            }
        }

        // Check for simple parameter: "key: value"
        if let Some((key, value)) = self.parse_simple_parameter(line) {
            if value == "[" {
                // Array start - begin array collection
                if let Some(tool) = &mut self.current_tool {
                    tool.current_array = Some(ArrayState {
                        param_name: key,
                        elements: Vec::new(),
                    });
                }
                return Ok(());
            } else {
                self.add_parameter(key, value)?;
                return Ok(());
            }
        }

        // If we're inside an array or handling other parameter content,
        // we might need to collect it. For now, treat unrecognized lines
        // as potential array elements or multiline content.
        // This is a simplified approach - a full implementation would
        // track array collection state more carefully.

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

    pub fn finalize_buffer(&mut self) -> Result<(), UIError> {
        // Process any remaining content
        if !self.buffer.trim().is_empty() {
            // Try to process the buffer content normally first
            self.process_buffer()?;

            // If there's still content left, handle it based on current state
            if !self.buffer.trim().is_empty() {
                if self.current_tool.is_some() {
                    // Inside tool block - process remaining as tool content and close tool
                    self.process_remaining_tool_content(&self.buffer.clone())?;
                    if let Some(tool) = &self.current_tool {
                        self.send_tool_end(&tool.id)?;
                    }
                    self.current_tool = None;
                } else {
                    // Outside tool block - emit as plain text
                    self.send_plain_text(&self.buffer)?;
                }
                self.buffer.clear();
            }
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
