use super::DisplayFragment;
use super::StreamProcessorTrait;
use crate::ui::{UIError, UserInterface};
use llm::StreamingChunk;
use std::sync::Arc;
use tracing::debug;

/// Simplified state machine for JSON parsing
#[derive(Debug, Clone, PartialEq)]
enum JsonParseState {
    /// Outside any JSON structure or waiting for start
    Outside,
    /// Inside top-level JSON object
    InObject,
    /// Parsing a parameter name (between quotes)
    InParamName,
    /// After parameter name, expecting colon
    AfterParamName,
    /// After colon, expecting value
    BeforeValue,
    /// Inside a simple value (string, number, boolean, null)
    InSimpleValue,
    /// Inside a complex value (object or array)
    InComplexValue,
}

/// Tag types for thinking text processing
#[derive(PartialEq)]
enum ThinkingTagType {
    None,
    Start,
    End,
}

/// State tracking for JSON processor
struct JsonProcessorState {
    /// Current parse state in the state machine
    state: JsonParseState,
    /// Current parameter name being parsed
    current_param: String,
    /// Current value being accumulated
    current_value: String,
    /// Tool ID for the current parsing context
    tool_id: String,
    /// Tool name for the current parsing context
    tool_name: String,
    /// If we're currently inside a quoted string
    in_quotes: bool,
    /// If the previous character was an escape character
    escaped: bool,
    /// Buffer for incomplete JSON chunks
    buffer: String,
    /// Nesting level for complex values (objects/arrays)
    nesting_level: i32,
    /// Track if we're inside thinking tags for text chunks
    in_thinking: bool,
    /// Track if we're at the beginning of a block (thinking/content)
    /// Used to determine when to trim leading newlines
    at_block_start: bool,
}

impl Default for JsonProcessorState {
    fn default() -> Self {
        Self {
            state: JsonParseState::Outside,
            current_param: String::new(),
            current_value: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            in_quotes: false,
            escaped: false,
            buffer: String::new(),
            nesting_level: 0,
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
                            self.state.state = JsonParseState::Outside;
                            self.state.current_param.clear();
                            self.state.current_value.clear();
                            self.state.in_quotes = false;
                            self.state.escaped = false;
                            self.state.nesting_level = 0;

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
    /// Process a chunk of JSON and extract parameters
    fn process_json(&mut self, content: &str) -> Result<(), UIError> {
        // Combine buffer with new content
        let text = format!("{}{}", self.state.buffer, content);
        self.state.buffer.clear();

        // Process each character
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                // Handle escape character (backslash)
                '\\' => {
                    if self.state.in_quotes {
                        if self.state.escaped {
                            // Double escape becomes a literal backslash
                            self.handle_content_char(c);
                            self.state.escaped = false;
                        } else {
                            self.state.escaped = true;
                        }
                    } else {
                        // Backslash outside quotes is just a regular character
                        self.handle_content_char(c);
                    }
                }

                // Handle quotation marks
                '"' => {
                    if !self.state.escaped {
                        // Toggle quote state
                        self.state.in_quotes = !self.state.in_quotes;

                        match self.state.state {
                            JsonParseState::InObject if self.state.in_quotes => {
                                // Start of parameter name
                                self.state.state = JsonParseState::InParamName;
                                self.state.current_param.clear();
                            }
                            JsonParseState::InParamName if !self.state.in_quotes => {
                                // End of parameter name
                                self.state.state = JsonParseState::AfterParamName;
                            }
                            JsonParseState::BeforeValue if self.state.in_quotes => {
                                // Start of string value
                                self.state.state = JsonParseState::InSimpleValue;
                                self.state.current_value.clear();
                            }
                            JsonParseState::InSimpleValue if !self.state.in_quotes => {
                                // End of string value
                                self.emit_parameter()?;
                                self.state.state = JsonParseState::InObject;
                            }
                            JsonParseState::InComplexValue => {
                                // Quote within a complex value, just add it
                                self.handle_content_char(c);
                            }
                            _ => {
                                // Other quote states, just add it as content
                                self.handle_content_char(c);
                            }
                        }
                    } else {
                        // Escaped quote becomes a literal quote
                        self.handle_content_char(c);
                        self.state.escaped = false;
                    }
                }

                // Handle opening braces - start of an object
                '{' => {
                    if !self.state.in_quotes {
                        match self.state.state {
                            JsonParseState::Outside => {
                                // Start of top-level object
                                self.state.state = JsonParseState::InObject;
                            }
                            JsonParseState::BeforeValue => {
                                // Start of a complex object value
                                self.state.state = JsonParseState::InComplexValue;
                                self.state.nesting_level = 1;
                                self.handle_content_char(c);
                            }
                            JsonParseState::InComplexValue => {
                                // Nested object within complex value
                                self.state.nesting_level += 1;
                                self.handle_content_char(c);
                            }
                            _ => {
                                // Other states, just add as content
                                self.handle_content_char(c);
                            }
                        }
                    } else {
                        // Literal '{' inside quotes
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }

                // Handle closing braces - end of an object
                '}' => {
                    if !self.state.in_quotes {
                        match self.state.state {
                            JsonParseState::InObject => {
                                // End of top-level object
                                self.state.state = JsonParseState::Outside;
                            }
                            JsonParseState::InComplexValue => {
                                // End of a nested object within complex value
                                self.state.nesting_level -= 1;
                                self.handle_content_char(c);

                                // If we've reached the end of the complex value
                                if self.state.nesting_level == 0 {
                                    self.emit_parameter()?;
                                    self.state.state = JsonParseState::InObject;
                                }
                            }
                            _ => {
                                // Other states, just add as content
                                self.handle_content_char(c);
                            }
                        }
                    } else {
                        // Literal '}' inside quotes
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }

                // Handle opening brackets - start of an array
                '[' => {
                    if !self.state.in_quotes {
                        match self.state.state {
                            JsonParseState::BeforeValue => {
                                // Start of a complex array value
                                self.state.state = JsonParseState::InComplexValue;
                                self.state.nesting_level = 1;
                                self.handle_content_char(c);
                            }
                            JsonParseState::InComplexValue => {
                                // Nested array within complex value
                                self.state.nesting_level += 1;
                                self.handle_content_char(c);
                            }
                            _ => {
                                // Other states, just add as content
                                self.handle_content_char(c);
                            }
                        }
                    } else {
                        // Literal '[' inside quotes
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }

                // Handle closing brackets - end of an array
                ']' => {
                    if !self.state.in_quotes {
                        match self.state.state {
                            JsonParseState::InComplexValue => {
                                // End of a nested array within complex value
                                self.state.nesting_level -= 1;
                                self.handle_content_char(c);

                                // If we've reached the end of the complex value
                                if self.state.nesting_level == 0 {
                                    self.emit_parameter()?;
                                    self.state.state = JsonParseState::InObject;
                                }
                            }
                            _ => {
                                // Other states, just add as content
                                self.handle_content_char(c);
                            }
                        }
                    } else {
                        // Literal ']' inside quotes
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }

                // Handle colon - separates parameter name from value
                ':' => {
                    if !self.state.in_quotes {
                        if self.state.state == JsonParseState::AfterParamName {
                            // Transition from parameter name to value
                            self.state.state = JsonParseState::BeforeValue;
                        } else if self.state.state == JsonParseState::InComplexValue {
                            // Colon within a complex value
                            self.handle_content_char(c);
                        }
                    } else {
                        // Literal ':' inside quotes
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }

                // Handle comma - separates parameters or values in arrays
                ',' => {
                    if !self.state.in_quotes {
                        match self.state.state {
                            JsonParseState::InObject => {
                                // Comma between parameters in the top-level object
                                // Just wait for the next parameter name
                            }
                            JsonParseState::InSimpleValue => {
                                // End of a simple value
                                self.emit_parameter()?;
                                self.state.state = JsonParseState::InObject;
                            }
                            JsonParseState::InComplexValue => {
                                // Comma within a complex value
                                self.handle_content_char(c);
                            }
                            _ => {
                                // Other states, ignore or add as content
                                if self.state.state == JsonParseState::InComplexValue {
                                    self.handle_content_char(c);
                                }
                            }
                        }
                    } else {
                        // Literal ',' inside quotes
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }

                // Handle whitespace
                ' ' | '\t' | '\n' | '\r' => {
                    if self.state.in_quotes || self.state.state == JsonParseState::InComplexValue {
                        // Preserve whitespace in quotes and complex values
                        self.handle_content_char(c);
                    }
                    // Otherwise, ignore whitespace
                    self.state.escaped = false;
                }

                // Handle other characters (numbers, letters, etc.)
                _ => {
                    if self.state.state == JsonParseState::BeforeValue && !self.state.in_quotes {
                        // Start of a non-string primitive value (number, boolean, null)
                        self.state.state = JsonParseState::InSimpleValue;
                        self.state.current_value.clear();
                        self.handle_content_char(c);
                    } else {
                        // Any other character just gets added to current content
                        self.handle_content_char(c);
                    }
                    self.state.escaped = false;
                }
            }
        }

        // If we're in the middle of processing, store remaining data
        if !text.is_empty()
            && (self.state.in_quotes
                || self.state.state == JsonParseState::InComplexValue
                || self.state.state == JsonParseState::InSimpleValue)
        {
            // Store any remaining characters that would be needed for the next chunk
            self.state.buffer = chars.collect::<String>();
        }

        Ok(())
    }

    /// Helper to handle adding a character to the current content
    fn handle_content_char(&mut self, c: char) {
        match self.state.state {
            JsonParseState::InParamName => {
                self.state.current_param.push(c);
            }
            JsonParseState::InSimpleValue | JsonParseState::InComplexValue => {
                self.state.current_value.push(c);
            }
            _ => {}
        }
    }

    /// Emit the current parameter to the UI
    fn emit_parameter(&mut self) -> Result<(), UIError> {
        // Only emit if we have both a parameter name and value
        if !self.state.current_param.is_empty() && self.state.state != JsonParseState::InParamName {
            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                name: self.state.current_param.clone(),
                value: self.state.current_value.clone(),
                tool_id: self.state.tool_id.clone(),
            })?;

            // Clear the current value but keep the parameter name
            // in case there are multiple values with the same parameter
            self.state.current_value.clear();
        }
        Ok(())
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
                            let mut final_pre_text = processed_pre_text; // Is a String

                            // If a real thinking tag follows, trim ALL trailing spaces.
                            // Otherwise (not a thinking tag), final_pre_text is not trimmed of trailing spaces here.
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
                            // If any char (e.g. '<') emitted
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
                        // Only set at_block_start if non-empty text was processed
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
            (ThinkingTagType::Start, 10) // Length of "<thinking>"
        } else if text.starts_with("</thinking>") {
            (ThinkingTagType::End, 11) // Length of "</thinking>"
        } else if text.starts_with("<thinking") {
            // Incomplete opening tag
            (ThinkingTagType::Start, 0)
        } else if text.starts_with("</thinking") {
            // Incomplete closing tag
            (ThinkingTagType::End, 0)
        } else {
            (ThinkingTagType::None, 0)
        }
    }

    /// Check if a string is a potential beginning of a thinking tag
    /// This method closely mirrors the XML processor's is_potential_tag_start method
    fn is_potential_thinking_tag_start(&self, text: &str) -> bool {
        // Tag prefixes to check for
        const TAG_PREFIXES: [&str; 2] = ["<thinking>", "</thinking>"];

        // Check if the text could be the start of any tag
        for prefix in &TAG_PREFIXES {
            let text_chars: Vec<char> = text.chars().collect(); // Convert text to Vec<char>
            let prefix_chars: Vec<char> = prefix.chars().collect(); // Convert prefix to Vec<char>

            // Loop through all possible partial matches
            for i in 1..=prefix_chars.len().min(text_chars.len()) {
                // Check if the last `i` characters of text match the first `i` characters of prefix
                if text_chars[text_chars.len() - i..] == prefix_chars[..i] {
                    return true;
                }
            }
        }

        // Also check for incomplete tags that already started
        if text.contains('<') && !text.contains('>') {
            return true;
        }

        false
    }
}
