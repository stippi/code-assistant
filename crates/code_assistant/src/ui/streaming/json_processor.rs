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

#[derive(PartialEq, Debug, Clone)] // Added Debug and Clone for easier state management if needed
enum JsonParsingState {
    ExpectOpenBrace,        // Looking for '{'
    ExpectKeyOrCloseBrace,  // Looking for "key" or '}'
    InKey,                  // Inside a "key" string, accumulating in temp_chars_for_value
    ExpectColon,            // Looking for ':' after a key
    ExpectValue,            // Looking for the start of a value
    InValueString,          // Inside a "value" string, streaming parts
    InValueComplex,         // Inside an object or array value, accumulating its string representation in temp_chars_for_value
    InValueSimple,          // Inside a number, boolean, or null, accumulating in temp_chars_for_value
    ExpectCommaOrCloseBrace,// Looking for ',' or '}' after a value
}

/// State tracking for JSON processor
struct JsonProcessorState {
    /// Buffer for accumulating incomplete JSON from chunks
    buffer: String,
    /// Tool ID for the current parsing context
    tool_id: String,
    /// Tool name for the current parsing context
    tool_name: String,
    /// Track if we're inside thinking tags for text chunks
    in_thinking: bool,
    /// Track if we're at the beginning of a block (thinking/content) for text chunks
    at_block_start: bool,

    // New JSON specific state
    json_parsing_state: JsonParsingState,
    current_key: Option<String>,
    complex_value_nesting: u32,     // For '{' '[' '}' ']' tracking within a complex value
    temp_chars_for_value: String,   // Accumulates current key, or simple value, or complex value string
    in_string_escape: bool,         // True if current char in a string is after a backslash
    in_string_within_complex: bool, // True if currently inside a string within a complex value being captured
}

impl Default for JsonProcessorState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            in_thinking: false,
            at_block_start: false,

            // Initialize new JSON state fields
            json_parsing_state: JsonParsingState::ExpectOpenBrace,
            current_key: None,
            complex_value_nesting: 0,
            temp_chars_for_value: String::new(),
            in_string_escape: false,
            in_string_within_complex: false,
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
            StreamingChunk::Thinking(text) => self
                .ui
                .display_fragment(&DisplayFragment::ThinkingText(text.clone())),

            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                debug!(
                    "InputJson: content: '{}', tool_name: '{:?}', tool_id: '{:?}', current_json_state: {:?}, current_buffer: '{}'",
                    content, tool_name, tool_id, self.state.json_parsing_state, self.state.buffer
                );

                if let Some(id_from_chunk) = tool_id {
                    if !id_from_chunk.is_empty() {
                        // Determine if this is a new tool context
                        let is_new_tool_invocation = self.state.tool_id != *id_from_chunk || self.state.tool_id.is_empty();

                        if is_new_tool_invocation {
                            debug!(
                                "New tool invocation: name='{:?}', id='{}'. Previous tool_id='{}'",
                                tool_name, id_from_chunk, self.state.tool_id
                            );
                            self.state.tool_name = tool_name.clone().unwrap_or_default(); // Use name if provided
                            self.state.tool_id = id_from_chunk.clone();

                            // Reset JSON parsing state for the new tool invocation
                            self.state.buffer.clear();
                            self.state.json_parsing_state = JsonParsingState::ExpectOpenBrace;
                            self.state.current_key = None;
                            self.state.temp_chars_for_value.clear();
                            self.state.complex_value_nesting = 0;
                            self.state.in_string_escape = false;
                            self.state.in_string_within_complex = false;

                            // Send the tool name to UI
                            // Ensure tool_name is valid before sending fragment
                            if !self.state.tool_name.is_empty() {
                                self.ui.display_fragment(&DisplayFragment::ToolName {
                                    name: self.state.tool_name.clone(),
                                    id: self.state.tool_id.clone(),
                                })?;
                            } else {
                                // If tool_name is not provided with the id, it's a bit strange.
                                // For now, we rely on tool_name being present if id is.
                                debug!("Tool ID '{}' provided without a tool name.", self.state.tool_id);
                            }
                        }
                    }
                } else if !content.is_empty() && self.state.tool_id.is_empty() {
                    // This is a fallback: if content arrives, but we have no tool_id context,
                    // it implies a tool call started without the initial metadata chunk.
                    // This situation might be an error or require a default tool_id.
                    // The tests usually provide tool_id, even with empty content for the first chunk.
                    debug!("Warning: Received JSON content '{}' but tool_id is not set.", content);
                    // Potentially set a default/dummy tool_id or error out.
                    // For now, we'll let it proceed, but fragments might be emitted without a proper tool_id.
                }

                // Process the JSON content using the new stream-based parser
                self.process_json_stream(content)
            }

            StreamingChunk::Text(text) => self.process_text_with_thinking_tags(text),
        }
    }
}

impl JsonStreamProcessor {
    /// Process a chunk of JSON content character by character to enable streaming.
    /// This method iteratively consumes characters from the internal buffer.
    fn process_json_stream(&mut self, new_content: &str) -> Result<(), UIError> {
        self.state.buffer.push_str(new_content);

        let mut made_progress_in_iteration = true;
        'char_processing_loop: while !self.state.buffer.is_empty() && made_progress_in_iteration {
            made_progress_in_iteration = false; // Reset for current pass

            let char_to_process_opt = self.state.buffer.chars().next();
            let char_to_process = match char_to_process_opt {
                Some(c) => c,
                None => break 'char_processing_loop,
            };
            let char_len = char_to_process.len_utf8();
            let mut consumed_char_in_state = true; // Most states consume the char they match

            // Cloned for debugging, avoid multiple calls to current_tool_id!
            // let current_tool_id_for_debug = self.state.tool_id.clone();
            // debug!(
            //     "Process char: '{}', State: {:?}, Key: {:?}, TempVal: '{}', Buffer: '{}', ToolID: '{}'",
            //     char_to_process, self.state.json_parsing_state, self.state.current_key,
            //     self.state.temp_chars_for_value, self.state.buffer, current_tool_id_for_debug
            // );

            match self.state.json_parsing_state {
                JsonParsingState::ExpectOpenBrace => {
                    if char_to_process.is_whitespace() {
                        // Consume whitespace
                    } else if char_to_process == '{' {
                        self.state.json_parsing_state = JsonParsingState::ExpectKeyOrCloseBrace;
                    } else {
                        // Malformed JSON or unexpected content. For now, consume and log.
                        debug!("Expected '{{' or whitespace, got '{}'. Consuming.", char_to_process);
                    }
                }
                JsonParsingState::ExpectKeyOrCloseBrace => {
                    if char_to_process.is_whitespace() {
                        // Consume whitespace
                    } else if char_to_process == '"' {
                        self.state.json_parsing_state = JsonParsingState::InKey;
                        self.state.temp_chars_for_value.clear(); // Used for accumulating key name
                    } else if char_to_process == '}' {
                        let tool_id = if self.state.tool_id.is_empty() {
    debug!("Error: tool_id is empty while trying to emit a fragment. Current state: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    // This is a critical error, as fragments cannot be emitted without a tool_id.
    // Returning an error might be more appropriate, but for now, stop processing this chunk.
    debug!("Critical error: tool_id is empty during JSON processing. Aborting processing for this chunk. State: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    return Ok(()); // Stop processing this chunk, as tool_id is essential and missing.
} else {
    self.state.tool_id.clone()
};
                        self.ui.display_fragment(&DisplayFragment::ToolEnd { id: tool_id })?;
                        self.state.json_parsing_state = JsonParsingState::ExpectOpenBrace; // Reset for next potential JSON object
                        self.state.current_key = None;
                        self.state.buffer.clear(); // Object done, clear buffer of this object. This might be too aggressive if there's trailing content.
                                                  // Let's refine: only clear if this was the *only* content, or handle trailing chars.
                                                  // For now, `drain` handles consumed chars.
                    } else if char_to_process == ',' {
                        // This is for cases like {"a":"b",} -> expecting a key next.
                        // If we see `,,,` this will just loop. Assuming valid JSON structure mostly.
                        // Comma should be handled by ExpectCommaOrCloseBrace moving to this state.
                        // If we are here and see a comma, it implies an empty item e.g. {,"key": ..} which is invalid.
                        // Let's assume for now, if we see a comma, we expect a key, but this state should be after a value or open brace.
                        // This is more robustly handled by ExpectCommaOrCloseBrace.
                        debug!("Unexpected comma in ExpectKeyOrCloseBrace state. Char: '{}'", char_to_process);
                    } else {
                        debug!("Expected '\"' (key start), '}}' (obj end), or whitespace. Got '{}'. Consuming.", char_to_process);
                    }
                }
                JsonParsingState::InKey => {
                    if self.state.in_string_escape {
                        self.state.temp_chars_for_value.push(char_to_process);
                        self.state.in_string_escape = false;
                    } else if char_to_process == '\\' {
                        self.state.in_string_escape = true;
                        // We don't add the backslash to temp_chars_for_value here, it's handled above.
                    } else if char_to_process == '"' {
                        self.state.current_key = Some(self.state.temp_chars_for_value.clone());
                        self.state.temp_chars_for_value.clear(); // No longer needed for key
                        self.state.json_parsing_state = JsonParsingState::ExpectColon;
                    } else {
                        self.state.temp_chars_for_value.push(char_to_process);
                    }
                }
                JsonParsingState::ExpectColon => {
                    if char_to_process.is_whitespace() {
                        // consume
                    } else if char_to_process == ':' {
                        self.state.json_parsing_state = JsonParsingState::ExpectValue;
                    } else {
                        debug!("Expected ':' or whitespace, got '{}'. Consuming.", char_to_process);
                    }
                }
                JsonParsingState::ExpectValue => {
                    if char_to_process.is_whitespace() {
                        // consume
                    } else if char_to_process == '"' { // Start of string value
                        self.state.json_parsing_state = JsonParsingState::InValueString;
                        self.state.in_string_escape = false;
                        // temp_chars_for_value is not used for streaming string parts directly to UI
                    } else if char_to_process == '{' || char_to_process == '[' { // Start of complex value
                        self.state.json_parsing_state = JsonParsingState::InValueComplex;
                        self.state.temp_chars_for_value.clear();
                        self.state.temp_chars_for_value.push(char_to_process); // Start accumulating raw complex value
                        self.state.complex_value_nesting = 1;
                        self.state.in_string_within_complex = false;
                        self.state.in_string_escape = false;
                    } else if char_to_process.is_ascii_digit() || char_to_process == '-' ||
                              char_to_process == 't' || char_to_process == 'f' || char_to_process == 'n' { // Start of simple value (number, bool, null)
                        self.state.json_parsing_state = JsonParsingState::InValueSimple;
                        self.state.temp_chars_for_value.clear();
                        self.state.temp_chars_for_value.push(char_to_process); // Start accumulating simple value
                    } else {
                        debug!("Expected value start (\",{{,[,digit,t,f,n) or whitespace, got '{}'. Consuming.", char_to_process);
                    }
                }
                JsonParsingState::InValueString => {
                    if self.state.current_key.is_none() {
                        debug!("InValueString state but current_key is None. Char: '{}', Buffer: '{}'", char_to_process, self.state.buffer);
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        consumed_char_in_state = false; // Re-process this char in the new state.
                        continue 'char_processing_loop;
                    }
                    let current_key_name = self.state.current_key.as_ref().unwrap(); // Safe due to check above

                    let tool_id = if self.state.tool_id.is_empty() {
    debug!("Error: tool_id is empty while trying to emit a fragment. Current state: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    // This is a critical error, as fragments cannot be emitted without a tool_id.
    // Returning an error might be more appropriate, but for now, stop processing this chunk.
    debug!("Critical error: tool_id is empty during JSON processing. Aborting processing for this chunk. State: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    return Ok(()); // Stop processing this chunk, as tool_id is essential and missing.
} else {
    self.state.tool_id.clone()
};

                    if self.state.in_string_escape {
                        // For escaped chars, emit the backslash and the char itself.
                        let mut val_part = String::with_capacity(2);
                        val_part.push('\\');
                        val_part.push(char_to_process);
                        self.ui.display_fragment(&DisplayFragment::ToolParameter {
                            name: current_key_name.clone(),
                            value: val_part,
                            tool_id,
                        })?;
                        self.state.in_string_escape = false;
                    } else if char_to_process == '\\' {
                        self.state.in_string_escape = true;
                        // The backslash itself isn't emitted as a value part yet.
                        // It modifies the next character.
                    } else if char_to_process == '"' { // End of string value
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        // self.state.current_key = None; // Key is considered used up for this value.
                                                      // Let's keep current_key until ExpectCommaOrCloseBrace clears it,
                                                      // in case of empty string "" we still need it for that last quote.
                    } else { // Regular character in string value
                        self.ui.display_fragment(&DisplayFragment::ToolParameter {
                            name: current_key_name.clone(),
                            value: char_to_process.to_string(),
                            tool_id,
                        })?;
                    }
                }
                JsonParsingState::InValueComplex => {
                    if self.state.current_key.is_none() {
                        debug!("InValueComplex state but current_key is None. Char: '{}', Buffer: '{}'", char_to_process, self.state.buffer);
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        consumed_char_in_state = false; // Re-process this char in the new state.
                        continue 'char_processing_loop;
                    }
                    let current_key_name = self.state.current_key.as_ref().unwrap(); // Safe due to check above

                    let tool_id = if self.state.tool_id.is_empty() {
    debug!("Error: tool_id is empty while trying to emit a fragment. Current state: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    // This is a critical error, as fragments cannot be emitted without a tool_id.
    // Returning an error might be more appropriate, but for now, stop processing this chunk.
    debug!("Critical error: tool_id is empty during JSON processing. Aborting processing for this chunk. State: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    return Ok(()); // Stop processing this chunk, as tool_id is essential and missing.
} else {
    self.state.tool_id.clone()
};

                    self.state.temp_chars_for_value.push(char_to_process);

                    if self.state.in_string_within_complex {
                        if self.state.in_string_escape {
                            self.state.in_string_escape = false;
                        } else if char_to_process == '\\' {
                            self.state.in_string_escape = true;
                        } else if char_to_process == '"' {
                            self.state.in_string_within_complex = false;
                        }
                    } else { // Not in string within complex value
                        if char_to_process == '"' {
                            self.state.in_string_within_complex = true;
                            self.state.in_string_escape = false;
                        } else if char_to_process == '{' || char_to_process == '[' {
                            self.state.complex_value_nesting += 1;
                        } else if char_to_process == '}' || char_to_process == ']' {
                            self.state.complex_value_nesting -= 1;
                            if self.state.complex_value_nesting == 0 {
                                // Complex value finished, emit it as a single string
                                self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                    name: current_key_name.clone(),
                                    value: self.state.temp_chars_for_value.clone(),
                                    tool_id,
                                })?;
                                self.state.temp_chars_for_value.clear();
                                self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                                // self.state.current_key = None; // Key used up
                            }
                        }
                    }
                }
                JsonParsingState::InValueSimple => {
                    if self.state.current_key.is_none() {
                        debug!("InValueSimple state but current_key is None. Char: '{}', Buffer: '{}'", char_to_process, self.state.buffer);
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        consumed_char_in_state = false; // Re-process this char in the new state.
                        continue 'char_processing_loop;
                    }
                    let current_key_name = self.state.current_key.as_ref().unwrap(); // Safe due to check above

                    let tool_id = if self.state.tool_id.is_empty() {
    debug!("Error: tool_id is empty while trying to emit a fragment. Current state: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    // This is a critical error, as fragments cannot be emitted without a tool_id.
    // Returning an error might be more appropriate, but for now, stop processing this chunk.
    debug!("Critical error: tool_id is empty during JSON processing. Aborting processing for this chunk. State: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    return Ok(()); // Stop processing this chunk, as tool_id is essential and missing.
} else {
    self.state.tool_id.clone()
};

                    // Accumulate chars for number, boolean, or null.
                    // These are emitted completely once a terminator (whitespace, ,, }) is found.
                    if char_to_process.is_whitespace() || char_to_process == ',' || char_to_process == '}' || char_to_process == ']' {
                        // Terminator found. Emit accumulated value if any.
                        if !self.state.temp_chars_for_value.is_empty() {
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: current_key_name.clone(),
                                value: self.state.temp_chars_for_value.clone(),
                                tool_id,
                            })?;
                            self.state.temp_chars_for_value.clear();
                        }
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        // self.state.current_key = None; // Key used up
                        consumed_char_in_state = false; // Re-process this char in the new state (ExpectCommaOrCloseBrace)
                    } else {
                        self.state.temp_chars_for_value.push(char_to_process);
                    }
                }
                JsonParsingState::ExpectCommaOrCloseBrace => {
                    if char_to_process.is_whitespace() {
                        // consume
                    } else if char_to_process == ',' {
                        self.state.json_parsing_state = JsonParsingState::ExpectKeyOrCloseBrace;
                        self.state.current_key = None; // Clear current key, expecting a new one
                    } else if char_to_process == '}' {
                        let tool_id = if self.state.tool_id.is_empty() {
    debug!("Error: tool_id is empty while trying to emit a fragment. Current state: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    // This is a critical error, as fragments cannot be emitted without a tool_id.
    // Returning an error might be more appropriate, but for now, stop processing this chunk.
    debug!("Critical error: tool_id is empty during JSON processing. Aborting processing for this chunk. State: {:?}, Buffer: '{}'", self.state.json_parsing_state, self.state.buffer);
    return Ok(()); // Stop processing this chunk, as tool_id is essential and missing.
} else {
    self.state.tool_id.clone()
};
                        self.ui.display_fragment(&DisplayFragment::ToolEnd { id: tool_id })?;
                        self.state.json_parsing_state = JsonParsingState::ExpectOpenBrace; // Reset for next potential JSON object
                        self.state.current_key = None;
                    } else {
                        debug!("Expected ',', '}}', or whitespace, got '{}'. Consuming to attempt recovery.", char_to_process);
                        // This might be an error or trailing characters. For now, consume.
                        // If strict parsing is needed, this could be an error.
                    }
                }
            }

            if consumed_char_in_state {
                self.state.buffer.drain(..char_len);
                made_progress_in_iteration = true;
            }

            // Safety break if buffer isn't shrinking and we didn't make progress by changing state
            // This check is now implicitly handled by `made_progress_in_iteration` not being set if no char is consumed.
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
