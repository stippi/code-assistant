use super::DisplayFragment;
use super::StreamProcessorTrait;
use crate::llm::StreamingChunk;
use crate::ui::{UIError, UserInterface};
use std::sync::Arc;

/// Enum representing the current state of JSON parsing
#[derive(Debug, Clone, PartialEq)]
enum JsonParseState {
    /// Not currently parsing JSON
    None,
    /// Expecting a parameter name (after '{' or ',')
    ExpectingParamName,
    /// Currently parsing a parameter name (between quotes)
    InParamName,
    /// Expecting a colon after parameter name
    ExpectingColon,
    /// Expecting a parameter value (after colon)
    ExpectingParamValue,
    /// Currently parsing a parameter value
    InParamValue,
}

/// State for JSON processor that handles streaming JSON chunks from native API
struct JsonProcessorState {
    /// Current JSON parse state
    json_state: JsonParseState,
    /// Current parameter name being parsed
    current_param_name: Option<String>,
    /// Current parameter value being accumulated
    current_param_value: String,
    /// Current tool ID (if known)
    tool_id: String,
    /// Current tool name (if known)
    tool_name: String,
    /// Track the nesting level of JSON objects
    json_depth: i32,
    /// Track if we're inside a quoted string
    in_quotes: bool,
    /// Track if the previous character was an escape character
    escaped: bool,
    /// Buffer for incomplete chunks
    buffer: String,
}

impl Default for JsonProcessorState {
    fn default() -> Self {
        Self {
            json_state: JsonParseState::None,
            current_param_name: None,
            current_param_value: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            json_depth: 0,
            in_quotes: false,
            escaped: false,
            buffer: String::new(),
        }
    }
}

/// Stream processor for handling native JSON parameter chunks
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
            // For native thinking chunks, send directly as ThinkingText
            StreamingChunk::Thinking(text) => self
                .ui
                .display_fragment(&DisplayFragment::ThinkingText(text.clone())),

            // For native JSON input, use the JSON parser
            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                // If this is the first part with tool info, send a ToolName fragment
                // and initialize tool context
                if let (Some(name), Some(id)) = (tool_name, tool_id) {
                    if !name.is_empty() && !id.is_empty() {
                        // Store tool name and ID for parameter context
                        self.state.tool_name = name.clone();
                        self.state.tool_id = id.clone();

                        // Reset parsing state
                        self.state.json_depth = 0;
                        self.state.json_state = JsonParseState::None;
                        self.state.current_param_name = None;
                        self.state.current_param_value.clear();
                        self.state.in_quotes = false;
                        self.state.escaped = false;

                        // Send the tool name to UI
                        self.ui.display_fragment(&DisplayFragment::ToolName {
                            name: name.clone(),
                            id: id.clone(),
                        })?;
                    }
                }

                // Process the JSON content
                self.process_json_chunk(content)
            }

            // For plain text chunks, just display them as-is
            StreamingChunk::Text(text) => self
                .ui
                .display_fragment(&DisplayFragment::PlainText(text.clone())),
        }
    }
}

impl JsonStreamProcessor {
    /// Process a chunk of JSON and extract parameters
    fn process_json_chunk(&mut self, content: &str) -> Result<(), UIError> {
        // Combine buffer with new content
        let text = format!("{}{}", self.state.buffer, content);
        self.state.buffer.clear();

        // Process each character in the JSON
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    if !self.state.in_quotes {
                        self.state.json_depth += 1;
                        if self.state.json_depth == 1 {
                            // Start of the top-level object
                            self.state.json_state = JsonParseState::ExpectingParamName;
                        } else if self.state.json_state == JsonParseState::InParamValue {
                            // Nested object in a parameter value
                            self.state.current_param_value.push(c);
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue
                        || self.state.json_state == JsonParseState::InParamName
                    {
                        // Literal '{' inside quotes
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                '}' => {
                    if !self.state.in_quotes {
                        // End of an object
                        if self.state.json_depth > 0 {
                            self.state.json_depth -= 1;

                            // If we're at the root level and were in a value, emit the parameter
                            if self.state.json_depth == 0
                                && self.state.json_state == JsonParseState::InParamValue
                            {
                                self.emit_parameter()?;
                                self.state.json_state = JsonParseState::None;
                            } else if self.state.json_depth >= 1
                                && self.state.json_state == JsonParseState::InParamValue
                            {
                                // Nested object close in a parameter value
                                self.state.current_param_value.push(c);
                            }
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue
                        || self.state.json_state == JsonParseState::InParamName
                    {
                        // Literal '}' inside quotes
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                '"' => {
                    if !self.state.escaped {
                        // Toggle quote state
                        self.state.in_quotes = !self.state.in_quotes;

                        if self.state.json_state == JsonParseState::ExpectingParamName
                            && self.state.in_quotes
                        {
                            // Start of parameter name
                            self.state.json_state = JsonParseState::InParamName;
                            self.state.current_param_name = Some(String::new());
                        } else if self.state.json_state == JsonParseState::InParamName
                            && !self.state.in_quotes
                        {
                            // End of parameter name
                            self.state.json_state = JsonParseState::ExpectingColon;
                        } else if self.state.json_state == JsonParseState::ExpectingParamValue
                            && self.state.in_quotes
                        {
                            // Start of string parameter value
                            self.state.json_state = JsonParseState::InParamValue;
                            self.state.current_param_value = String::new();
                        } else if self.state.json_state == JsonParseState::InParamValue
                            && !self.state.in_quotes
                        {
                            // End of string parameter value
                            self.emit_parameter()?;
                            self.state.json_state = JsonParseState::ExpectingParamName;
                        }
                    } else {
                        // Escaped quote is part of the value/name
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                ':' => {
                    if !self.state.in_quotes
                        && self.state.json_state == JsonParseState::ExpectingColon
                    {
                        // Transition from colon to value
                        self.state.json_state = JsonParseState::ExpectingParamValue;
                    } else if self.state.in_quotes {
                        // Literal ':' in quoted string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                ',' => {
                    if !self.state.in_quotes {
                        // End of a value followed by next parameter
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.emit_parameter()?;
                            self.state.json_state = JsonParseState::ExpectingParamName;
                        }
                    } else {
                        // Literal ',' in quoted string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                '\\' => {
                    // Handle escape character
                    if self.state.in_quotes {
                        if self.state.escaped {
                            // Double escape becomes a literal backslash
                            if self.state.json_state == JsonParseState::InParamValue {
                                self.state.current_param_value.push(c);
                            } else if self.state.json_state == JsonParseState::InParamName {
                                if let Some(name) = &mut self.state.current_param_name {
                                    name.push(c);
                                }
                            }
                            self.state.escaped = false;
                        } else {
                            self.state.escaped = true;
                        }
                    } else {
                        // Backslash outside quotes is just a character
                        self.state.escaped = false;
                    }
                }
                // Handle numeric literals and booleans
                '0'..='9' | '-' | 't' | 'f' | 'n' => {
                    if !self.state.in_quotes
                        && self.state.json_state == JsonParseState::ExpectingParamValue
                    {
                        // Start of a non-string value (number/boolean/null)
                        self.state.json_state = JsonParseState::InParamValue;
                        self.state.current_param_value = c.to_string();

                        // Keep consuming until we hit comma or closing brace
                        let mut value = String::new();
                        let mut in_literal = true;
                        while in_literal {
                            match chars.peek() {
                                Some(next) if ![',', '}'].contains(next) => {
                                    value.push(*next);
                                    chars.next(); // Consume the peeked character
                                }
                                _ => in_literal = false,
                            }
                        }

                        if !value.is_empty() {
                            self.state.current_param_value.push_str(&value);
                        }
                    } else if self.state.in_quotes {
                        // Number inside quotes is part of the string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                // Handle whitespace
                ' ' | '\n' | '\r' | '\t' => {
                    if self.state.in_quotes {
                        // Whitespace in quotes is preserved
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    // Otherwise whitespace is ignored
                    self.state.escaped = false;
                }
                // All other characters
                _ => {
                    if self.state.in_quotes {
                        // Any character in quotes is part of the string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue {
                        // Part of a non-string value
                        self.state.current_param_value.push(c);
                    }
                    self.state.escaped = false;
                }
            }
        }

        // If we didn't finish processing some data, store it in buffer
        if self.state.in_quotes
            || self.state.json_state == JsonParseState::InParamValue
            || self.state.json_depth > 0
        {
            // For debugging: save the current state for the next chunk
            // Uncomment to see the processing state at chunk boundaries
            /*
            println!("Saving JSON state: depth={}, state={:?}, in_quotes={}, param_name={:?}, param_value={}",
                     self.state.json_depth,
                     self.state.json_state,
                     self.state.in_quotes,
                     self.state.current_param_name,
                     self.state.current_param_value);
            */
        }

        Ok(())
    }

    /// Emit a parameter to the UI
    fn emit_parameter(&mut self) -> Result<(), UIError> {
        if let Some(param_name) = &self.state.current_param_name {
            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                name: param_name.clone(),
                value: self.state.current_param_value.clone(),
                tool_id: self.state.tool_id.clone(),
            })?;

            // Reset state for next parameter
            self.state.current_param_name = None;
            self.state.current_param_value.clear();
        }
        Ok(())
    }
}
