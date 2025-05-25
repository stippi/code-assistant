use super::DisplayFragment;
use super::StreamProcessorTrait;
use crate::ui::{UIError, UserInterface};
use llm::StreamingChunk;
use std::sync::Arc;
use tracing::debug;

/// Tag types for thinking text processing
#[derive(PartialEq)]
enum ThinkingTagType {
    None,
    Start,
    End,
}

/// State tracking for JSON processor
struct JsonProcessorState {
    /// Buffer for accumulating incomplete JSON
    buffer: String,
    /// Tool ID for the current parsing context
    tool_id: String,
    /// Tool name for the current parsing context
    tool_name: String,
    /// Track if we're inside thinking tags for text chunks
    in_thinking: bool,
    /// Track if we're at the beginning of a block (thinking/content)
    at_block_start: bool,
}

impl Default for JsonProcessorState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            in_thinking: false,
            at_block_start: false,
        }
    }
}

/// Process JSON chunks from LLM providers
pub struct JsonStreamProcessor {
    state: JsonProcessorState,
    ui: Arc<Box<dyn UserInterface>>,
}

impl StreamProcessorTrait for JsonStreamProcessor {
    fn new(ui: Arc<Box<dyn UserInterface>>) -> Self {
        Self {
            state: JsonProcessorState::default(),
            ui,
        }
    }

    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        match chunk {
            // For thinking chunks, send directly as ThinkingText
            StreamingChunk::Thinking(text) => self
                .ui
                .display_fragment(&DisplayFragment::ThinkingText(text.clone())),

            // For JSON input, use the JSON processor
            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                debug!(
                    "Received InputJson chunk, tool_name: '{:?}', tool_id: '{:?}'",
                    tool_name, tool_id
                );
                // If this is the first part with tool info or a new tool, initialize tool context
                if let (Some(name), Some(id)) = (tool_name, tool_id) {
                    if !name.is_empty() && !id.is_empty() {
                        // Check if this is a new tool or the same one continuing
                        let is_new_tool = self.state.tool_id != *id;

                        // Store tool info
                        self.state.tool_name = name.clone();
                        self.state.tool_id = id.clone();

                        // Only reset parser state if this is a new tool
                        if is_new_tool {
                            self.state.buffer.clear();

                            // Send the tool name to UI only for new tools
                            self.ui.display_fragment(&DisplayFragment::ToolName {
                                name: name.clone(),
                                id: id.clone(),
                            })?;
                        }
                    }
                }

                // Process the JSON content
                self.process_json(content)
            }

            // For plain text chunks, process for thinking tags and then display
            StreamingChunk::Text(text) => self.process_text_with_thinking_tags(text),
        }
    }
}

impl JsonStreamProcessor {
    /// Process a chunk of JSON with proper buffering and streaming
    fn process_json(&mut self, content: &str) -> Result<(), UIError> {
        // Add new content to buffer
        self.state.buffer.push_str(content);

        // Try to extract and emit complete parameters
        loop {
            let initial_buffer_len = self.state.buffer.len();
            self.try_extract_parameter()?;
            
            // If buffer didn't change, we can't make more progress
            if self.state.buffer.len() == initial_buffer_len {
                break;
            }
        }

        Ok(())
    }

    /// Try to extract one complete parameter from the buffer
    fn try_extract_parameter(&mut self) -> Result<(), UIError> {
        let json_str = self.state.buffer.trim_start();
        
        if json_str.is_empty() {
            return Ok(());
        }

        // Find or continue with JSON object
        let working_json = if let Some(brace_pos) = json_str.find('{') {
            &json_str[brace_pos..]
        } else if json_str.starts_with(',') || json_str.starts_with('"') {
            // This looks like a continuation of JSON - create a temporary complete JSON
            &format!("{{{}", json_str)
        } else {
            return Ok(()); // Not ready yet
        };

        // Try to find a complete parameter
        if let Some((param_name, param_value, consumed_chars)) = self.find_complete_parameter(working_json) {
            // Emit the parameter
            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                name: param_name,
                value: param_value,
                tool_id: self.state.tool_id.clone(),
            })?;

            // Remove consumed content from buffer
            let chars_to_remove = if json_str.starts_with(',') || json_str.starts_with('"') {
                consumed_chars - 1 // Account for the fake opening brace
            } else {
                let brace_pos = json_str.find('{').unwrap();
                brace_pos + consumed_chars
            };
            
            // Calculate actual position in original buffer
            let trim_start_offset = self.state.buffer.len() - json_str.len();
            let total_chars_to_remove = trim_start_offset + chars_to_remove;
            
            if total_chars_to_remove < self.state.buffer.len() {
                self.state.buffer = self.state.buffer[total_chars_to_remove..].to_string();
            } else {
                self.state.buffer.clear();
            }
        }

        Ok(())
    }

    /// Find a complete parameter (name + value) in the JSON string
    /// Returns (param_name, param_value, chars_consumed) if found
    fn find_complete_parameter(&self, json_str: &str) -> Option<(String, String, usize)> {
        let chars: Vec<char> = json_str.chars().collect();
        let mut pos = 0;

        // Skip opening brace and whitespace
        while pos < chars.len() && (chars[pos] == '{' || chars[pos].is_whitespace()) {
            pos += 1;
        }

        // Skip comma if present
        if pos < chars.len() && chars[pos] == ',' {
            pos += 1;
            while pos < chars.len() && chars[pos].is_whitespace() {
                pos += 1;
            }
        }

        // Parse parameter name
        if pos >= chars.len() || chars[pos] != '"' {
            return None;
        }

        let (param_name, name_end) = self.parse_string(&chars, pos)?;
        pos = name_end;

        // Skip whitespace and find colon
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() || chars[pos] != ':' {
            return None;
        }
        pos += 1; // Skip colon

        // Skip whitespace before value
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() {
            return None;
        }

        // Parse value based on type
        let (param_value, value_end) = match chars[pos] {
            '"' => self.parse_string(&chars, pos)?,
            '{' => self.parse_object(&chars, pos)?,
            '[' => self.parse_array(&chars, pos)?,
            _ => self.parse_simple_value(&chars, pos)?,
        };

        Some((param_name, param_value, value_end))
    }

    /// Parse a quoted string starting at position
    fn parse_string(&self, chars: &[char], start_pos: usize) -> Option<(String, usize)> {
        if start_pos >= chars.len() || chars[start_pos] != '"' {
            return None;
        }

        let mut pos = start_pos + 1;
        let mut result = String::new();
        let mut escaped = false;

        while pos < chars.len() {
            let c = chars[pos];

            if escaped {
                match c {
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    '\\' => result.push('\\'),
                    '"' => result.push('"'),
                    _ => {
                        result.push('\\');
                        result.push(c);
                    }
                }
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                return Some((result, pos + 1));
            } else {
                result.push(c);
            }

            pos += 1;
        }

        None // Incomplete string
    }

    /// Parse an object starting at position
    fn parse_object(&self, chars: &[char], start_pos: usize) -> Option<(String, usize)> {
        if start_pos >= chars.len() || chars[start_pos] != '{' {
            return None;
        }

        let mut pos = start_pos;
        let mut result = String::new();
        let mut nesting_level = 0;
        let mut in_string = false;
        let mut escaped = false;

        while pos < chars.len() {
            let c = chars[pos];
            result.push(c);

            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
            } else {
                match c {
                    '"' => in_string = true,
                    '{' => nesting_level += 1,
                    '}' => {
                        nesting_level -= 1;
                        if nesting_level == 0 {
                            return Some((result, pos + 1));
                        }
                    }
                    _ => {}
                }
            }

            pos += 1;
        }

        None // Incomplete object
    }

    /// Parse an array starting at position
    fn parse_array(&self, chars: &[char], start_pos: usize) -> Option<(String, usize)> {
        if start_pos >= chars.len() || chars[start_pos] != '[' {
            return None;
        }

        let mut pos = start_pos;
        let mut result = String::new();
        let mut nesting_level = 0;
        let mut in_string = false;
        let mut escaped = false;

        while pos < chars.len() {
            let c = chars[pos];
            result.push(c);

            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
            } else {
                match c {
                    '"' => in_string = true,
                    '[' => nesting_level += 1,
                    ']' => {
                        nesting_level -= 1;
                        if nesting_level == 0 {
                            return Some((result, pos + 1));
                        }
                    }
                    _ => {}
                }
            }

            pos += 1;
        }

        None // Incomplete array
    }

    /// Parse a simple value (number, boolean, null)
    fn parse_simple_value(&self, chars: &[char], start_pos: usize) -> Option<(String, usize)> {
        let mut pos = start_pos;
        let mut result = String::new();

        while pos < chars.len() {
            let c = chars[pos];

            if matches!(c, ',' | '}' | ']') || c.is_whitespace() {
                if result.is_empty() {
                    return None;
                }
                return Some((result, pos));
            }

            result.push(c);
            pos += 1;
        }

        None // Incomplete simple value
    }

    /// Process text chunks and extract <thinking> blocks
    fn process_text_with_thinking_tags(&mut self, text: &str) -> Result<(), UIError> {
        // Combine buffer with new text
        let current_text = format!("{}{}", self.state.buffer, text);

        // Buffer truncating logic
        let mut processing_text = current_text.clone();
        let mut safe_length = processing_text.len();
        for j in (1..=processing_text.len().min(20)).rev() {
            if !processing_text.is_char_boundary(processing_text.len() - j) {
                continue;
            }
            let suffix = &processing_text[processing_text.len() - j..];
            if suffix.ends_with('\n') && j == 1 {
                safe_length = processing_text.len() - 1;
                self.state.buffer = "\n".to_string();
                break;
            } else if self.is_potential_thinking_tag_start(suffix) {
                safe_length = processing_text.len() - j;
                self.state.buffer = suffix.to_string();
                break;
            }
        }
        if safe_length < processing_text.len() {
            // Ensure safe_length is at a char boundary
            while safe_length > 0 && !processing_text.is_char_boundary(safe_length) {
                safe_length -= 1;
            }
            processing_text = processing_text[..safe_length].to_string();
        } else {
            self.state.buffer.clear();
        }

        let mut current_pos = 0;
        while current_pos < processing_text.len() {
            let text_to_scan = &processing_text[current_pos..];
            if let Some(tag_start_offset) = text_to_scan.find('<') {
                let absolute_tag_pos = current_pos + tag_start_offset;
                let pre_tag_slice = &processing_text[current_pos..absolute_tag_pos];

                let after_lt_slice = &processing_text[absolute_tag_pos..];
                let (tag_type, tag_len) = self.detect_thinking_tag(after_lt_slice);

                // Process pre_tag_slice
                if !pre_tag_slice.is_empty() {
                    let mut processed_pre_text = pre_tag_slice.to_string();
                    if processed_pre_text.ends_with('\n') {
                        processed_pre_text.pop();
                    }
                    if self.state.at_block_start && !processed_pre_text.is_empty() {
                        processed_pre_text = processed_pre_text.trim_start().to_string();
                    }

                    if !processed_pre_text.is_empty() {
                        if self.state.in_thinking {
                            self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                processed_pre_text,
                            ))?;
                        } else {
                            let mut final_pre_text = processed_pre_text;

                            // If a real thinking tag follows, trim ALL trailing spaces.
                            if tag_type == ThinkingTagType::Start
                                || tag_type == ThinkingTagType::End
                            {
                                while final_pre_text.ends_with(' ') {
                                    final_pre_text.pop();
                                }
                            }

                            if !final_pre_text.is_empty() {
                                self.ui.display_fragment(&DisplayFragment::PlainText(
                                    final_pre_text,
                                ))?;
                            }
                        }
                    }
                    self.state.at_block_start = false;
                }

                // Handle the tag itself or incomplete tags
                let is_incomplete_definition = tag_type != ThinkingTagType::None && tag_len == 0;
                let is_incomplete_stream =
                    tag_len > 0 && (absolute_tag_pos + tag_len > processing_text.len());

                if is_incomplete_definition || is_incomplete_stream {
                    self.state.buffer = processing_text[absolute_tag_pos..].to_string();
                    break;
                }

                match tag_type {
                    ThinkingTagType::Start if tag_len > 0 => {
                        self.state.in_thinking = true;
                        self.state.at_block_start = true;
                        current_pos = absolute_tag_pos + tag_len;
                    }
                    ThinkingTagType::End if tag_len > 0 => {
                        self.state.in_thinking = false;
                        self.state.at_block_start = true;
                        current_pos = absolute_tag_pos + tag_len;
                    }
                    _ => {
                        let char_len = after_lt_slice.chars().next().map_or(1, |c| c.len_utf8());
                        let end_char_pos = (absolute_tag_pos + char_len).min(processing_text.len());
                        let single_char_slice_str =
                            &processing_text[absolute_tag_pos..end_char_pos];

                        if !single_char_slice_str.is_empty() {
                            if self.state.in_thinking {
                                self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                    single_char_slice_str.to_string(),
                                ))?;
                            } else {
                                self.ui.display_fragment(&DisplayFragment::PlainText(
                                    single_char_slice_str.to_string(),
                                ))?;
                            }
                        }
                        current_pos = end_char_pos;
                        if !single_char_slice_str.is_empty() {
                            self.state.at_block_start = false;
                        }
                    }
                }
            } else {
                let remaining = &processing_text[current_pos..];
                if !remaining.is_empty() {
                    let mut processed_remaining_text = remaining.to_string();
                    if processed_remaining_text.ends_with('\n') {
                        processed_remaining_text.pop();
                    }
                    if self.state.at_block_start && !processed_remaining_text.is_empty() {
                        processed_remaining_text =
                            processed_remaining_text.trim_start().to_string();
                    }

                    if !processed_remaining_text.is_empty() {
                        self.state.at_block_start = false;
                    }

                    if !processed_remaining_text.is_empty() {
                        if self.state.in_thinking {
                            self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                processed_remaining_text,
                            ))?;
                        } else {
                            self.ui.display_fragment(&DisplayFragment::PlainText(
                                processed_remaining_text,
                            ))?;
                        }
                    }
                }
                current_pos = processing_text.len();
            }
        }
        Ok(())
    }

    /// Detect if the given text starts with a thinking tag
    fn detect_thinking_tag(&self, text: &str) -> (ThinkingTagType, usize) {
        if text.starts_with("<thinking>") {
            (ThinkingTagType::Start, 10)
        } else if text.starts_with("</thinking>") {
            (ThinkingTagType::End, 11)
        } else if text.starts_with("<thinking") {
            (ThinkingTagType::Start, 0)
        } else if text.starts_with("</thinking") {
            (ThinkingTagType::End, 0)
        } else {
            (ThinkingTagType::None, 0)
        }
    }

    /// Check if a string is a potential beginning of a thinking tag
    fn is_potential_thinking_tag_start(&self, text: &str) -> bool {
        const TAG_PREFIXES: [&str; 2] = ["<thinking>", "</thinking>"];

        for prefix in &TAG_PREFIXES {
            let text_chars: Vec<char> = text.chars().collect();
            let prefix_chars: Vec<char> = prefix.chars().collect();

            for i in 1..=prefix_chars.len().min(text_chars.len()) {
                if text_chars[text_chars.len() - i..] == prefix_chars[..i] {
                    return true;
                }
            }
        }

        if text.contains('<') && !text.contains('>') {
            return true;
        }

        false
    }
}