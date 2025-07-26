use super::DisplayFragment;
use super::StreamProcessorTrait;
use crate::ui::{UIError, UserInterface};
use llm::{ContentBlock, Message, MessageContent, StreamingChunk};
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
    ExpectOpenBrace,         // Looking for '{'
    ExpectKeyOrCloseBrace,   // Looking for "key" or '}'
    InKey,                   // Inside a "key" string, accumulating in temp_chars_for_value
    ExpectColon,             // Looking for ':' after a key
    ExpectValue,             // Looking for the start of a value
    InValueString,           // Inside a "value" string, streaming parts
    InValueComplex, // Inside an object or array value, accumulating its string representation in temp_chars_for_value
    InValueSimple,  // Inside a number, boolean, or null, accumulating in temp_chars_for_value
    ExpectCommaOrCloseBrace, // Looking for ',' or '}' after a value
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
    /// True if any part of the current string value being parsed has been emitted.
    /// Used to detect and emit empty strings: ""
    emitted_string_part_for_current_key: bool,

    // JSON specific state
    json_parsing_state: JsonParsingState,
    current_key: Option<String>,
    complex_value_nesting: u32, // For '{' '[' '}' ']' tracking within a complex value
    temp_chars_for_value: String, // Accumulates current key, or simple value, or complex value string
    in_string_escape: bool,       // True if current char in a string is after a backslash
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
            emitted_string_part_for_current_key: false,
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
    fn new(ui: Arc<Box<dyn UserInterface>>, _request_id: u64) -> Self {
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

            StreamingChunk::RateLimit { seconds_remaining } => {
                // Notify UI about rate limit with countdown
                self.ui.notify_rate_limit(*seconds_remaining);
                Ok(())
            }

            StreamingChunk::RateLimitClear => {
                // Clear rate limit notification
                self.ui.clear_rate_limit();
                Ok(())
            }

            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                // If there's text in the buffer from previous Text chunks, process it first.
                if !self.state.buffer.is_empty() {
                    debug!(
                        "Flushing text buffer before processing InputJson. Buffer content: '{}'",
                        self.state.buffer
                    );
                    // Call process_text_with_thinking_tags with an empty string
                    // to make it process its current buffer contents.
                    self.process_text_with_thinking_tags("")?;
                    // After this, self.state.buffer should ideally be empty or contain
                    // a genuinely incomplete tag part that process_text_with_thinking_tags decided to keep.
                    // The JSON processing logic, especially for a new tool invocation, typically
                    // clears self.state.buffer. This flush ensures text finalization.
                }

                debug!(
                    "InputJson: content: '{}', tool_name: '{:?}', tool_id: '{:?}', current_json_state: {:?}, current_buffer: '{}'",
                    content, tool_name, tool_id, self.state.json_parsing_state, self.state.buffer
                );

                if let Some(id_from_chunk) = tool_id {
                    if !id_from_chunk.is_empty() {
                        // Determine if this is a new tool context
                        let is_new_tool_invocation =
                            self.state.tool_id != *id_from_chunk || self.state.tool_id.is_empty();

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
                                debug!(
                                    "Tool ID '{}' provided without a tool name.",
                                    self.state.tool_id
                                );
                            }
                        }
                    }
                } else if !content.is_empty() && self.state.tool_id.is_empty() {
                    // This is a fallback: if content arrives, but we have no tool_id context,
                    // it implies a tool call started without the initial metadata chunk.
                    // This situation might be an error or require a default tool_id.
                    // The tests usually provide tool_id, even with empty content for the first chunk.
                    debug!(
                        "Warning: Received JSON content '{}' but tool_id is not set.",
                        content
                    );
                    // Potentially set a default/dummy tool_id or error out.
                    // For now, we'll let it proceed, but fragments might be emitted without a proper tool_id.
                }

                // Process the JSON content using the new stream-based parser
                self.process_json_stream(content)
            }

            StreamingChunk::StreamingComplete => {
                // JSON processor doesn't need buffering like XML/Caret processors
                // Just acknowledge the completion
                Ok(())
            }

            StreamingChunk::Text(text) => self.process_text_with_thinking_tags(text),
        }
    }

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        match &message.content {
            MessageContent::Text(text) => {
                // Process text for thinking tags, similar to process_text_with_thinking_tags
                // but collect fragments instead of sending to UI
                fragments.extend(self.extract_fragments_from_text(text, message.request_id)?);
            }
            MessageContent::Structured(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Thinking { thinking, .. } => {
                            fragments.push(DisplayFragment::ThinkingText(thinking.clone()));
                        }
                        ContentBlock::Text { text } => {
                            // Process text for any thinking tags
                            fragments.extend(
                                self.extract_fragments_from_text(text, message.request_id)?,
                            );
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            // Add tool name fragment
                            fragments.push(DisplayFragment::ToolName {
                                name: name.clone(),
                                id: id.clone(),
                            });

                            // Parse JSON input into tool parameters
                            if let Some(obj) = input.as_object() {
                                for (key, value) in obj {
                                    let value_str = if value.is_string() {
                                        // Remove quotes from string values
                                        value.as_str().unwrap_or("").to_string()
                                    } else {
                                        // For non-string values, serialize as JSON
                                        value.to_string()
                                    };

                                    fragments.push(DisplayFragment::ToolParameter {
                                        name: key.clone(),
                                        value: value_str,
                                        tool_id: id.clone(),
                                    });
                                }
                            }

                            // Add tool end fragment
                            fragments.push(DisplayFragment::ToolEnd { id: id.clone() });
                        }
                        ContentBlock::ToolResult { .. } => {
                            // Tool results are typically not part of assistant messages
                            // but we could handle them if needed
                        }
                        ContentBlock::RedactedThinking { .. } => {
                            // Redacted thinking blocks are not displayed
                        }
                    }
                }
            }
        }

        Ok(fragments)
    }
}

impl JsonStreamProcessor {
    /// Process a chunk of JSON content character by character to enable streaming.
    /// This method iteratively consumes characters from the internal buffer.
    #[allow(unused_assignments)]
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
            let char_len = char_to_process.len_utf8(); // Byte length of the current character
                                                       // Bytes consumed in this iteration. Defaults to current char's length.
                                                       // Will be updated by states like InValueString if they consume more.
            let mut iteration_consumed_bytes = char_len;

            // Cloned for debugging, avoid multiple calls to current_tool_id!
            // let current_tool_id_for_debug = self.state.tool_id.clone();
            // debug!(
            //     "Process char: '{}', State: {:?}, Key: {:?}, TempVal: '{}', Buffer: '{}', ToolID: '{}', iter_consumed: {}b",
            //     char_to_process, self.state.json_parsing_state, self.state.current_key,
            //     self.state.temp_chars_for_value, self.state.buffer, current_tool_id_for_debug, iteration_consumed_bytes
            // );

            match self.state.json_parsing_state {
                JsonParsingState::ExpectOpenBrace => {
                    if char_to_process.is_whitespace() {
                        // Consume whitespace
                    } else if char_to_process == '{' {
                        self.state.json_parsing_state = JsonParsingState::ExpectKeyOrCloseBrace;
                    } else {
                        // Malformed JSON or unexpected content. For now, consume and log.
                        debug!(
                            "Expected '{{' or whitespace, got '{}'. Consuming.",
                            char_to_process
                        );
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
                        self.ui
                            .display_fragment(&DisplayFragment::ToolEnd { id: tool_id })?;
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
                        debug!(
                            "Unexpected comma in ExpectKeyOrCloseBrace state. Char: '{}'",
                            char_to_process
                        );
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
                        debug!(
                            "Expected ':' or whitespace, got '{}'. Consuming.",
                            char_to_process
                        );
                    }
                }
                JsonParsingState::ExpectValue => {
                    if char_to_process.is_whitespace() {
                        // consume
                    } else if char_to_process == '"' {
                        // Start of string value
                        self.state.json_parsing_state = JsonParsingState::InValueString;
                        self.state.in_string_escape = false;
                        self.state.emitted_string_part_for_current_key = false; // Initialize for new string value
                                                                                // temp_chars_for_value is not used for streaming string parts directly to UI
                    } else if char_to_process == '{' || char_to_process == '[' {
                        // Start of complex value
                        self.state.json_parsing_state = JsonParsingState::InValueComplex;
                        self.state.temp_chars_for_value.clear();
                        self.state.temp_chars_for_value.push(char_to_process); // Start accumulating raw complex value
                        self.state.complex_value_nesting = 1;
                        self.state.in_string_within_complex = false;
                        self.state.in_string_escape = false;
                    } else if char_to_process.is_ascii_digit()
                        || char_to_process == '-'
                        || char_to_process == 't'
                        || char_to_process == 'f'
                        || char_to_process == 'n'
                    {
                        // Start of simple value (number, bool, null)
                        self.state.json_parsing_state = JsonParsingState::InValueSimple;
                        self.state.temp_chars_for_value.clear();
                        self.state.temp_chars_for_value.push(char_to_process); // Start accumulating simple value
                    } else {
                        debug!("Expected value start (\",{{,[,digit,t,f,n) or whitespace, got '{}'. Consuming.", char_to_process);
                    }
                }
                JsonParsingState::InValueString => {
                    if self.state.current_key.is_none() {
                        debug!(
                            "InValueString state but current_key is None. Char: '{}', Buffer: '{}'",
                            char_to_process, self.state.buffer
                        );
                        // This could happen if JSON is malformed, like {"key": "value" "another_key": ...} (missing comma)
                        // or if a previous state incorrectly transitioned.
                        // Attempt to recover by expecting a comma or brace.
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        // consumed_char_in_state = false; // Re-process this char in the new state. -> REMOVED
                        made_progress_in_iteration = true; // Ensure loop continues for re-processing
                        continue 'char_processing_loop; // Skip current char consumption for this iteration
                    }
                    // Ensure current_key_name and tool_id are valid before extensive use
                    let current_key_name = self.state.current_key.as_ref().unwrap().clone(); // Safe due to check above

                    let tool_id = if self.state.tool_id.is_empty() {
                        debug!("Critical error: tool_id is empty during JSON processing (InValueString). Aborting processing for this chunk. Current key: {:?}, Buffer: '{}'", self.state.current_key, self.state.buffer);
                        // Stop processing this chunk. Without tool_id, fragments are meaningless.
                        return Ok(());
                    } else {
                        self.state.tool_id.clone()
                    };

                    if self.state.in_string_escape {
                        // Handle the character after a backslash
                        let escaped_char_as_string = match char_to_process {
                            'n' => "\n".to_string(),
                            'r' => "\r".to_string(),
                            't' => "\t".to_string(),
                            '\"' => "\"".to_string(), // String literal for a single double quote
                            '\\' => "\\".to_string(), // String literal for a single backslash
                            '/' => "/".to_string(),
                            'b' => "\x08".to_string(), // Rust string escape for backspace
                            'f' => "\x0C".to_string(), // Rust string escape for form feed
                            _ => {
                                // For an invalid JSON escape like \z, we want to output the literal chars '\' and 'z'.
                                // So the string should be "\z".
                                debug!(
                                    "Invalid JSON escape sequence in string: \\\\{}",
                                    char_to_process
                                ); // Log actual chars \ and {}
                                format!("\\{}", char_to_process) // Create string like "\z"
                            }
                        };

                        self.ui.display_fragment(&DisplayFragment::ToolParameter {
                            name: current_key_name,
                            value: escaped_char_as_string,
                            tool_id,
                        })?;
                        self.state.emitted_string_part_for_current_key = true;
                        self.state.in_string_escape = false;
                        // iteration_consumed_bytes remains char_len (for the char_to_process like 'n', '"', etc.)
                    } else if char_to_process == '\\' {
                        // Start of an escape sequence
                        self.state.in_string_escape = true;
                        // This backslash char is consumed. No fragment emitted yet.
                        // iteration_consumed_bytes remains char_len (for this backslash char)
                    } else if char_to_process == '"' {
                        // End of string value
                        if !self.state.emitted_string_part_for_current_key {
                            // This means the string was empty, e.g., ""
                            // current_key_name and tool_id are already validated and cloned above
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: current_key_name, // Already cloned
                                value: String::new(),   // Empty string
                                tool_id,                // Already cloned
                            })?;
                            // self.state.emitted_string_part_for_current_key remains false, but it's for the *next* string.
                        }
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        // Current quote char is consumed.
                        // iteration_consumed_bytes remains char_len (for this quote char)
                    } else {
                        // Regular character in string value - greedy consumption
                        let mut segment = String::new();
                        segment.push(char_to_process); // Start with the current char

                        // Track bytes for the segment being built, starting with current char's byte length
                        let mut current_segment_byte_length = char_len;

                        // Look ahead in the *rest* of the buffer (after the current char_to_process)
                        // The buffer slice starts *after* the current char_to_process.
                        // So, if buffer is "abc", and char_to_process is 'a', rest_of_buffer_after_current_char is "bc"
                        let mut next_char_scan_offset_in_buffer = char_len;

                        while next_char_scan_offset_in_buffer < self.state.buffer.len() {
                            // Peek at the char at the current offset in the *original full buffer*
                            // This character has not been processed by the main loop yet.
                            if let Some(next_peek_char) = self.state.buffer
                                [next_char_scan_offset_in_buffer..]
                                .chars()
                                .next()
                            {
                                if next_peek_char == '\\' || next_peek_char == '"' {
                                    break; // Stop segment at escape or end quote
                                }
                                segment.push(next_peek_char);
                                let next_peek_char_byte_len = next_peek_char.len_utf8();
                                current_segment_byte_length += next_peek_char_byte_len;
                                next_char_scan_offset_in_buffer += next_peek_char_byte_len;
                            } else {
                                // This case (Some(next_peek_char) being None) should ideally not be hit if
                                // next_char_scan_offset_in_buffer < self.state.buffer.len() is true
                                // and the buffer contains valid UTF-8. Breaking defensively.
                                debug!("Unexpected end of buffer peek while in greedy string consumption. Offset: {}", next_char_scan_offset_in_buffer);
                                break;
                            }
                        }

                        if !segment.is_empty() {
                            debug!(
                                "Emitting segment for key '{}': '{}'",
                                current_key_name, segment
                            );
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: current_key_name.clone(), // Clone as key_name is used again if string continues
                                value: segment,
                                tool_id,
                            })?;
                            self.state.emitted_string_part_for_current_key = true;
                        }
                        // This InValueString state instance consumed `current_segment_byte_length` from the buffer.
                        iteration_consumed_bytes = current_segment_byte_length;
                    }
                }
                JsonParsingState::InValueComplex => {
                    if self.state.current_key.is_none() {
                        debug!("InValueComplex state but current_key is None. Char: '{}', Buffer: '{}'", char_to_process, self.state.buffer);
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        made_progress_in_iteration = true; // Ensure loop continues for re-processing
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
                    } else {
                        // Not in string within complex value
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
                                self.state.json_parsing_state =
                                    JsonParsingState::ExpectCommaOrCloseBrace;
                                // self.state.current_key = None; // Key used up
                            }
                        }
                    }
                }
                JsonParsingState::InValueSimple => {
                    if self.state.current_key.is_none() {
                        debug!(
                            "InValueSimple state but current_key is None. Char: '{}', Buffer: '{}'",
                            char_to_process, self.state.buffer
                        );
                        self.state.json_parsing_state = JsonParsingState::ExpectCommaOrCloseBrace;
                        made_progress_in_iteration = true; // Ensure loop continues for re-processing
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
                    if char_to_process.is_whitespace()
                        || char_to_process == ','
                        || char_to_process == '}'
                        || char_to_process == ']'
                    {
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
                        made_progress_in_iteration = true; // Ensure loop continues for re-processing
                                                           // CRITICAL: The char (terminator) should be re-processed, so we must continue here.
                        continue 'char_processing_loop; // Add continue to re-process current char in new state
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
                        self.ui
                            .display_fragment(&DisplayFragment::ToolEnd { id: tool_id })?;
                        self.state.json_parsing_state = JsonParsingState::ExpectOpenBrace; // Reset for next potential JSON object
                        self.state.current_key = None;
                    } else {
                        debug!("Expected ',', '}}', or whitespace, got '{}'. Consuming to attempt recovery.", char_to_process);
                        // This might be an error or trailing characters. For now, consume.
                        // If strict parsing is needed, this could be an error.
                    }
                }
            }

            // If a 'continue 'char_processing_loop;' was hit in any of the states above,
            // this part of the code is skipped for that iteration.
            // Otherwise, the character (or segment) was processed, and should be drained.
            self.state.buffer.drain(..iteration_consumed_bytes);
            made_progress_in_iteration = true;

            // If consumed_char_in_state is false (e.g., continue 'char_processing_loop' was hit),
            // iteration_consumed_bytes is not used for draining, and made_progress_in_iteration
            // remains false unless a state change occurred that will lead to progress in the next iteration.
            // The loop condition `!self.state.buffer.is_empty() && made_progress_in_iteration`
            // handles termination if no progress is made.

            // Safety break if buffer isn't shrinking and we didn't make progress (e.g. state didn't change or consume)
            // This is handled by `made_progress_in_iteration` logic.
        }
        Ok(())
    }

    /// Process text chunks and extract <thinking> blocks
    fn process_text_with_thinking_tags(&mut self, text: &str) -> Result<(), UIError> {
        let is_flushing_call = text.is_empty() && !self.state.buffer.is_empty();

        // Combine old buffer with new text
        let mut combined_text = self.state.buffer.clone();
        combined_text.push_str(text);

        if combined_text.is_empty() {
            self.state.buffer.clear(); // Ensure it is clear if nothing to process
            return Ok(());
        }

        let processing_text: String;
        // `self.state.buffer` will be managed based on whether it's a flushing call or not,
        // and then potentially updated by the main processing loop if `processing_text` ends
        // in an incomplete tag.

        if is_flushing_call {
            processing_text = combined_text; // Process the entire current_text (which is the old buffer)
            self.state.buffer.clear(); // Clear buffer; loop below will repopulate if `processing_text` ends in a truly partial tag
        } else {
            // Suffix logic, operating on `combined_text`.
            // This determines `processing_text` and what suffix (if any) goes into `self.state.buffer`.
            let temp_combined_text_for_suffix_eval = combined_text.clone();
            let mut safe_len_for_processing = temp_combined_text_for_suffix_eval.len();
            let mut suffix_identified_and_buffered = false;

            for j in (1..=temp_combined_text_for_suffix_eval.len().min(20)).rev() {
                if !temp_combined_text_for_suffix_eval
                    .is_char_boundary(temp_combined_text_for_suffix_eval.len() - j)
                {
                    continue;
                }
                let suffix_candidate = &temp_combined_text_for_suffix_eval
                    [temp_combined_text_for_suffix_eval.len() - j..];

                if suffix_candidate.ends_with('\n') && j == 1 {
                    safe_len_for_processing = temp_combined_text_for_suffix_eval.len() - 1;
                    self.state.buffer = "\n".to_string();
                    suffix_identified_and_buffered = true;
                    break;
                } else if self.is_potential_thinking_tag_start(suffix_candidate) {
                    safe_len_for_processing = temp_combined_text_for_suffix_eval.len() - j;
                    self.state.buffer = suffix_candidate.to_string();
                    suffix_identified_and_buffered = true;
                    break;
                }
            }

            if safe_len_for_processing < temp_combined_text_for_suffix_eval.len() {
                // A suffix was cut and placed in self.state.buffer.
                // Ensure safe_len_for_processing is at a char boundary (it should be, but double check).
                while safe_len_for_processing > 0
                    && !temp_combined_text_for_suffix_eval.is_char_boundary(safe_len_for_processing)
                {
                    safe_len_for_processing -= 1;
                }
                processing_text =
                    temp_combined_text_for_suffix_eval[..safe_len_for_processing].to_string();
            } else {
                // No suffix was cut, or the cut part was not put in self.state.buffer by the loop.
                // Process all of combined_text.
                processing_text = temp_combined_text_for_suffix_eval;
                if !suffix_identified_and_buffered {
                    // If no suffix was explicitly put in buffer by the logic above, clear it.
                    self.state.buffer.clear();
                }
            }
        }

        // The main processing loop for `processing_text` starts from here.
        // `self.state.buffer` has been prepared (either cleared for flushing, or set by suffix logic).
        // This loop may further update `self.state.buffer` if `processing_text` itself ends in an incomplete tag.

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

    /// Extract fragments from text without sending to UI (used for session loading)
    fn extract_fragments_from_text(
        &mut self,
        text: &str,
        _request_id: Option<u64>,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        // Local state for processing this text
        let mut local_in_thinking = false;
        let mut at_block_start = false;
        let mut current_pos = 0;

        while current_pos < text.len() {
            if let Some(tag_start_offset) = text[current_pos..].find('<') {
                let absolute_tag_pos = current_pos + tag_start_offset;

                // Process text before tag
                if tag_start_offset > 0 {
                    let pre_tag_slice = &text[current_pos..absolute_tag_pos];

                    // Check what kind of tag follows to determine trimming
                    let after_lt_slice = &text[absolute_tag_pos..];
                    let (tag_type, _tag_len) = self.detect_thinking_tag(after_lt_slice);

                    let mut processed_pre_text = pre_tag_slice.to_string();
                    if processed_pre_text.ends_with('\n') {
                        processed_pre_text.pop();
                    }
                    if at_block_start && !processed_pre_text.is_empty() {
                        processed_pre_text = processed_pre_text.trim_start().to_string();
                    }

                    if !processed_pre_text.is_empty() {
                        let mut final_pre_text = processed_pre_text;

                        // If a real thinking tag follows, trim ALL trailing spaces
                        if tag_type == ThinkingTagType::Start || tag_type == ThinkingTagType::End {
                            while final_pre_text.ends_with(' ') {
                                final_pre_text.pop();
                            }
                        }

                        if !final_pre_text.is_empty() {
                            if local_in_thinking {
                                fragments.push(DisplayFragment::ThinkingText(final_pre_text));
                            } else {
                                fragments.push(DisplayFragment::PlainText(final_pre_text));
                            }
                        }
                    }
                    at_block_start = false;
                }

                // Check for thinking tags
                let tag_slice = &text[absolute_tag_pos..];
                let (tag_type, tag_len) = self.detect_thinking_tag(tag_slice);

                if tag_len > 0 {
                    match tag_type {
                        ThinkingTagType::Start => {
                            local_in_thinking = true;
                            at_block_start = true;
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        ThinkingTagType::End => {
                            local_in_thinking = false;
                            at_block_start = true;
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        _ => {
                            // Not a thinking tag, process as regular character
                            let char_len = tag_slice.chars().next().map_or(1, |c| c.len_utf8());
                            let char_text = &text[absolute_tag_pos..absolute_tag_pos + char_len];

                            if !char_text.is_empty() {
                                if local_in_thinking {
                                    fragments
                                        .push(DisplayFragment::ThinkingText(char_text.to_string()));
                                } else {
                                    fragments
                                        .push(DisplayFragment::PlainText(char_text.to_string()));
                                }
                            }
                            current_pos = absolute_tag_pos + char_len;
                            if !char_text.is_empty() {
                                at_block_start = false;
                            }
                        }
                    }
                } else {
                    // Incomplete or invalid tag, process as regular character
                    let char_len = tag_slice.chars().next().map_or(1, |c| c.len_utf8());
                    let char_text = &text[absolute_tag_pos..absolute_tag_pos + char_len];

                    if !char_text.is_empty() {
                        if local_in_thinking {
                            fragments.push(DisplayFragment::ThinkingText(char_text.to_string()));
                        } else {
                            fragments.push(DisplayFragment::PlainText(char_text.to_string()));
                        }
                    }
                    current_pos = absolute_tag_pos + char_len;
                    if !char_text.is_empty() {
                        at_block_start = false;
                    }
                }
            } else {
                // No more tags, process remaining text
                let remaining = &text[current_pos..];
                if !remaining.is_empty() {
                    let mut processed_remaining_text = remaining.to_string();
                    if processed_remaining_text.ends_with('\n') {
                        processed_remaining_text.pop();
                    }
                    if at_block_start && !processed_remaining_text.is_empty() {
                        processed_remaining_text =
                            processed_remaining_text.trim_start().to_string();
                    }

                    if !processed_remaining_text.is_empty() {
                        at_block_start = false;
                    }

                    if !processed_remaining_text.is_empty() {
                        if local_in_thinking {
                            fragments.push(DisplayFragment::ThinkingText(processed_remaining_text));
                        } else {
                            fragments.push(DisplayFragment::PlainText(processed_remaining_text));
                        }
                    }
                }
                current_pos = text.len();
            }
        }

        Ok(fragments)
    }
}
