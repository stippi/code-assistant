use super::DisplayFragment;
use super::StreamProcessorTrait;
use crate::llm::StreamingChunk;
use crate::ui::{UIError, UserInterface};
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

            // For plain text chunks, display as-is
            StreamingChunk::Text(text) => self
                .ui
                .display_fragment(&DisplayFragment::PlainText(text.clone())),
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
}
