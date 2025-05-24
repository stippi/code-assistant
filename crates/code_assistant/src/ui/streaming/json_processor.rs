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
    /// Track if we've already emitted the parameter start
    parameter_started: bool,
}

impl Default for JsonProcessorState {
    fn default() -> Self {
        Self {
            state: JsonParseState::Outside,
            current_param: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            in_quotes: false,
            escaped: false,
            buffer: String::new(),
            nesting_level: 0,
            in_thinking: false,
            at_block_start: false,
            parameter_started: false,
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
                            self.state.in_quotes = false;
                            self.state.escaped = false;
                            self.state.nesting_level = 0;
                            self.state.parameter_started = false;

                            // Send the tool name to UI only for new tools
                            self.ui.display_fragment(&DisplayFragment::ToolName {
                                name: name.clone(),
                                id: id.clone(),
                            })?;
                        }
                    }
                }

                // Process the JSON content
                self.process_json_chunk(content)
            }

            // For plain text chunks, process for thinking tags and then display
            StreamingChunk::Text(text) => self.process_text_with_thinking_tags(text),
        }
    }
}

impl JsonStreamProcessor {
    /// Process a chunk of JSON with chunk-based streaming approach
    fn process_json_chunk(&mut self, content: &str) -> Result<(), UIError> {
        // Combine buffer with new content
        let text = format!("{}{}", self.state.buffer, content);
        self.state.buffer.clear();

        let mut pos = 0;
        let chars: Vec<char> = text.chars().collect();

        while pos < chars.len() {
            match self.state.state {
                JsonParseState::Outside => {
                    // Look for opening brace
                    if let Some(brace_pos) = text[pos..].find('{') {
                        pos += brace_pos + 1;
                        self.state.state = JsonParseState::InObject;
                    } else {
                        // No opening brace found, buffer the rest
                        self.state.buffer = text[pos..].to_string();
                        break;
                    }
                }

                JsonParseState::InObject => {
                    // Look for start of parameter name (quote)
                    if let Some(quote_pos) = self.find_next_structural_quote(&text[pos..]) {
                        pos += quote_pos + 1;
                        self.state.state = JsonParseState::InParamName;
                        self.state.current_param.clear();
                    } else {
                        // No quote found, buffer the rest
                        self.state.buffer = text[pos..].to_string();
                        break;
                    }
                }

                JsonParseState::InParamName => {
                    // Look for end of parameter name (closing quote)
                    if let Some(quote_pos) = self.find_next_structural_quote(&text[pos..]) {
                        // Extract parameter name
                        self.state.current_param = text[pos..pos + quote_pos].to_string();
                        println!("DEBUG: Found parameter name: '{}'", self.state.current_param);
                        pos += quote_pos + 1;
                        self.state.state = JsonParseState::AfterParamName;
                    } else {
                        // No closing quote found, buffer the rest
                        self.state.buffer = text[pos..].to_string();
                        break;
                    }
                }

                JsonParseState::AfterParamName => {
                    // Look for colon
                    if let Some(colon_pos) = text[pos..].find(':') {
                        pos += colon_pos + 1;
                        self.state.state = JsonParseState::BeforeValue;
                    } else {
                        // No colon found, buffer the rest
                        self.state.buffer = text[pos..].to_string();
                        break;
                    }
                }

                JsonParseState::BeforeValue => {
                    // Skip whitespace and determine value type
                    while pos < chars.len() && chars[pos].is_whitespace() {
                        pos += 1;
                    }

                    if pos >= chars.len() {
                        // No more characters, buffer empty and wait for more
                        break;
                    }

                    match chars[pos] {
                        '"' => {
                            // String value
                            pos += 1; // Skip opening quote
                            self.state.state = JsonParseState::InSimpleValue;
                            self.state.in_quotes = true;
                            self.state.parameter_started = false;
                            // Don't emit here, wait for actual content
                        }
                        '{' | '[' => {
                            // Complex value (object or array)
                            self.state.state = JsonParseState::InComplexValue;
                            self.state.nesting_level = 1;
                            self.state.in_quotes = false;
                            self.state.parameter_started = false;

                            // Emit parameter start with the opening brace/bracket
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.current_param.clone(),
                                value: chars[pos].to_string(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                            self.state.parameter_started = true;

                            pos += 1; // Skip opening brace/bracket
                        }
                        _ => {
                            // Simple value (number, boolean, null)
                            self.state.state = JsonParseState::InSimpleValue;
                            self.state.in_quotes = false;
                            self.state.parameter_started = false;
                            // Don't emit here, wait for actual content
                        }
                    }
                }

                JsonParseState::InSimpleValue => {
                    // We're now in a parameter value - stream the rest of the chunk
                    let remaining = &text[pos..];
                    if !remaining.is_empty() {
                        // Emit parameter start if not already done
                        if !self.state.parameter_started {
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.current_param.clone(),
                                value: String::new(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                            self.state.parameter_started = true;
                        }

                        // Find the end of this parameter value
                        let (value_content, new_pos) = if self.state.in_quotes {
                            self.extract_quoted_content(remaining)?
                        } else {
                            self.extract_simple_value_content(remaining)
                        };

                        if !value_content.is_empty() {
                            // Emit the value content
                            println!("DEBUG: Emitting simple param '{}' with value: '{}'", self.state.current_param, value_content);
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.current_param.clone(),
                                value: value_content,
                                tool_id: self.state.tool_id.clone(),
                            })?;
                        }

                        pos += new_pos;

                        // Check if we've reached the end of the value
                        if new_pos < remaining.len() || self.value_is_complete(remaining, new_pos) {
                            println!("DEBUG: Parameter '{}' completed, going back to InObject", self.state.current_param);
                            self.state.state = JsonParseState::InObject;
                            self.state.parameter_started = false;
                        } else {
                            // More content expected, buffer any incomplete part
                            break;
                        }
                    } else {
                        break;
                    }
                }

                JsonParseState::InComplexValue => {
                    // We're in a complex value - stream the content
                    let remaining = &text[pos..];
                    if !remaining.is_empty() {
                        // Emit parameter start if not already done
                        if !self.state.parameter_started {
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.current_param.clone(),
                                value: String::new(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                            self.state.parameter_started = true;
                        }

                        // Extract complex value content
                        let (value_content, new_pos, nesting_change) =
                            self.extract_complex_value_content(remaining)?;

                        if !value_content.is_empty() {
                            // Emit the value content
                            println!("DEBUG: Emitting complex param '{}' with value: '{}'", self.state.current_param, value_content);
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.current_param.clone(),
                                value: value_content,
                                tool_id: self.state.tool_id.clone(),
                            })?;
                        }

                        self.state.nesting_level += nesting_change;
                        pos += new_pos;

                        // Check if we've reached the end of the complex value
                        if self.state.nesting_level == 0 {
                            println!("DEBUG: Complex parameter '{}' completed, going back to InObject", self.state.current_param);
                            self.state.state = JsonParseState::InObject;
                            self.state.parameter_started = false;
                        } else if new_pos >= remaining.len() {
                            // More content expected, stop processing
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Find the next structural quote (not escaped)
    fn find_next_structural_quote(&self, text: &str) -> Option<usize> {
        let mut escaped = false;
        for (i, c) in text.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => return Some(i),
                _ => escaped = false,
            }
        }
        None
    }

    /// Extract content from a quoted string value
    fn extract_quoted_content(&self, text: &str) -> Result<(String, usize), UIError> {
        let mut content = String::new();
        let mut pos = 0;
        let mut escaped = false;
        let chars: Vec<char> = text.chars().collect();

        while pos < chars.len() {
            let c = chars[pos];
            match c {
                '\\' if !escaped => {
                    escaped = true;
                    // Don't add the backslash to content, it's just the escape char
                }
                '"' if !escaped => {
                    // End of quoted string
                    return Ok((content, pos + 1));
                }
                '"' if escaped => {
                    // Escaped quote becomes literal quote
                    content.push(c);
                    escaped = false;
                }
                'n' if escaped => {
                    // Escaped newline
                    content.push('\n');
                    escaped = false;
                }
                't' if escaped => {
                    // Escaped tab
                    content.push('\t');
                    escaped = false;
                }
                'r' if escaped => {
                    // Escaped carriage return
                    content.push('\r');
                    escaped = false;
                }
                '\\' if escaped => {
                    // Escaped backslash
                    content.push('\\');
                    escaped = false;
                }
                _ => {
                    if escaped {
                        // Unknown escape sequence, keep the backslash
                        content.push('\\');
                        escaped = false;
                    }
                    content.push(c);
                }
            }
            pos += 1;
        }

        // Incomplete quoted string, return what we have
        Ok((content, pos))
    }

    /// Extract content from a simple (non-quoted) value
    fn extract_simple_value_content(&self, text: &str) -> (String, usize) {
        // For simple values, find the next structural character (, } ])
        let mut pos = 0;
        let chars: Vec<char> = text.chars().collect();

        while pos < chars.len() {
            match chars[pos] {
                ',' | '}' | ']' => break,
                _ => pos += 1,
            }
        }

        (text[..pos].to_string(), pos)
    }

    /// Extract content from a complex value (object or array)
    fn extract_complex_value_content(&self, text: &str) -> Result<(String, usize, i32), UIError> {
        let mut content = String::new();
        let mut pos = 0;
        let mut nesting_change = 0;
        let mut in_quotes = false;
        let mut escaped = false;
        let chars: Vec<char> = text.chars().collect();

        while pos < chars.len() {
            let c = chars[pos];

            match c {
                '\\' if in_quotes && !escaped => {
                    escaped = true;
                    content.push(c);
                }
                '"' if !escaped => {
                    in_quotes = !in_quotes;
                    content.push(c);
                }
                '{' | '[' if !in_quotes => {
                    nesting_change += 1;
                    content.push(c);
                }
                '}' | ']' if !in_quotes => {
                    nesting_change -= 1;
                    content.push(c);
                    if self.state.nesting_level + nesting_change == 0 {
                        // End of complex value
                        return Ok((content, pos + 1, nesting_change));
                    }
                }
                _ => {
                    if escaped {
                        escaped = false;
                    }
                    content.push(c);
                }
            }
            pos += 1;
        }

        // Return what we have so far
        Ok((content, pos, nesting_change))
    }

    /// Check if a value is complete based on the context
    fn value_is_complete(&self, text: &str, pos: usize) -> bool {
        if pos >= text.len() {
            return false;
        }

        // Check if we've hit a structural delimiter
        match text.chars().nth(pos) {
            Some(',') | Some('}') | Some(']') => true,
            _ => false,
        }
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
