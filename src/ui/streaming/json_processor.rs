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
    /// Track the nesting level of JSON objects (for top-level parsing)
    json_depth: i32,
    /// Track if we're inside a quoted string
    in_quotes: bool,
    /// Track if the previous character was an escape character
    escaped: bool,
    /// Buffer for incomplete chunks
    buffer: String,
    /// Track nesting level within arrays and objects in parameter values
    param_value_nesting: i32,
    /// Track special characters in the parameter value for proper batching
    collecting_special_value: bool,
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
            param_value_nesting: 0,
            collecting_special_value: false,
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
                        self.state.param_value_nesting = 0;
                        self.state.collecting_special_value = false;

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
                        } else if self.state.json_state == JsonParseState::ExpectingParamValue {
                            // Start of object as parameter value
                            self.state.json_state = JsonParseState::InParamValue;
                            self.state.collecting_special_value = true;
                            self.state.param_value_nesting += 1;
                            self.state.current_param_value.push(c);
                            
                            // For objects and arrays, we'll collect and emit the entire value
                            if !self.collect_complex_value('{', '}', &mut chars)? {
                                // If we couldn't complete the object collection, store the buffer
                                self.state.buffer = text[text.len() - chars.count()..].to_string();
                                return Ok(());
                            }
                        } else if self.state.json_state == JsonParseState::InParamValue {
                            // Nested object in a parameter value
                            self.state.param_value_nesting += 1;
                            self.state.current_param_value.push(c);
                            // Already collecting a complex value, continue
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue
                        || self.state.json_state == JsonParseState::InParamName
                    {
                        // Literal '{' inside quotes
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
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
                                self.state.current_param_value.push(c);
                                self.emit_parameter()?;
                                self.finalize_parameter()?;
                                self.state.json_state = JsonParseState::None;
                                self.state.collecting_special_value = false;
                            } else if self.state.json_depth >= 1
                                && self.state.json_state == JsonParseState::InParamValue
                            {
                                // Nested object close in a parameter value
                                if self.state.param_value_nesting > 0 {
                                    self.state.param_value_nesting -= 1;
                                }
                                self.state.current_param_value.push(c);
                                
                                // If we've closed all nested structures, emit and possibly finalize
                                if self.state.param_value_nesting == 0 {
                                    self.emit_parameter()?;
                                    self.state.collecting_special_value = false;
                                    
                                    // If this was the end of a complex top-level param value
                                    if !chars.peek().is_some_and(|&next| next == ',') {
                                        self.finalize_parameter()?;
                                        self.state.json_state = JsonParseState::ExpectingParamName;
                                    }
                                }
                            }
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue
                        || self.state.json_state == JsonParseState::InParamName
                    {
                        // Literal '}' inside quotes
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                '[' => {
                    if !self.state.in_quotes {
                        if self.state.json_state == JsonParseState::ExpectingParamValue {
                            // Start of an array as a parameter value
                            self.state.json_state = JsonParseState::InParamValue;
                            self.state.collecting_special_value = true;
                            self.state.param_value_nesting += 1;
                            self.state.current_param_value.push(c);
                            
                            // For objects and arrays, we'll collect and emit the entire value
                            if !self.collect_complex_value('[', ']', &mut chars)? {
                                // If we couldn't complete the array collection, store the buffer
                                self.state.buffer = text[text.len() - chars.count()..].to_string();
                                return Ok(());
                            }
                        } else if self.state.json_state == JsonParseState::InParamValue {
                            // Nested array inside a parameter value
                            self.state.param_value_nesting += 1;
                            self.state.current_param_value.push(c);
                            // Already collecting a complex value, continue
                        }
                    } else if self.state.in_quotes {
                        // Literal '[' in a quoted string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    }
                    self.state.escaped = false;
                }
                ']' => {
                    if !self.state.in_quotes {
                        if self.state.json_state == JsonParseState::InParamValue {
                            // End of an array in a parameter value
                            if self.state.param_value_nesting > 0 {
                                self.state.param_value_nesting -= 1;
                            }
                            self.state.current_param_value.push(c);
                            
                            // If we've closed all nested structures, emit and possibly finalize
                            if self.state.param_value_nesting == 0 {
                                self.emit_parameter()?;
                                self.state.collecting_special_value = false;
                                
                                // Check if there's a comma after this array
                                if chars.peek().is_some_and(|&next| next == ',') {
                                    // We'll continue to the next parameter
                                } else {
                                    // End of all parameters
                                    self.finalize_parameter()?;
                                    self.state.json_state = JsonParseState::ExpectingParamName;
                                }
                            }
                        }
                    } else if self.state.in_quotes {
                        // Literal ']' in a quoted string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
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
                            self.finalize_parameter()?;
                            self.state.json_state = JsonParseState::ExpectingParamName;
                        }
                    } else {
                        // Escaped quote is part of the value/name
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
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
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
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
                        // Handle commas based on nesting level
                        if self.state.json_state == JsonParseState::InParamValue {
                            if self.state.param_value_nesting > 0 {
                                // Comma inside a nested structure (array or object)
                                self.state.current_param_value.push(c);
                            } else {
                                // Comma between top-level parameters
                                self.emit_parameter()?;
                                self.finalize_parameter()?;
                                self.state.json_state = JsonParseState::ExpectingParamName;
                                self.state.collecting_special_value = false;
                            }
                        }
                    } else {
                        // Literal ',' in quoted string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
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
                                if !self.state.collecting_special_value {
                                    self.emit_parameter()?;
                                }
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
                        self.state.current_param_value.push(c);
                        
                        // For primitive values, we'll collect until we hit a delimiter
                        self.collect_primitive_value(&mut chars)?;
                        
                        // After collecting, emit the parameter
                        self.emit_parameter()?;
                        
                        // Check if the next character is a comma
                        if chars.peek().is_some_and(|&next| next == ',') {
                            // We'll continue to the next parameter, but we can finalize this one
                            self.finalize_parameter()?;
                            self.state.json_state = JsonParseState::ExpectingParamName;
                        } else if chars.peek().is_some_and(|&next| next == '}') {
                            // End of the object, finalize this parameter
                            self.finalize_parameter()?;
                            self.state.json_state = JsonParseState::ExpectingParamName;
                        }
                    } else if self.state.in_quotes {
                        // Number inside quotes is part of the string
                        if self.state.json_state == JsonParseState::InParamValue {
                            self.state.current_param_value.push(c);
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue {
                        // Part of a non-string value
                        self.state.current_param_value.push(c);
                        if !self.state.collecting_special_value {
                            self.emit_parameter()?;
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
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue 
                        && self.state.param_value_nesting > 0 {
                        // Whitespace in nested structures (arrays/objects) is preserved
                        self.state.current_param_value.push(c);
                        if !self.state.collecting_special_value {
                            self.emit_parameter()?;
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
                            if !self.state.collecting_special_value {
                                self.emit_parameter()?;
                            }
                        } else if self.state.json_state == JsonParseState::InParamName {
                            if let Some(name) = &mut self.state.current_param_name {
                                name.push(c);
                            }
                        }
                    } else if self.state.json_state == JsonParseState::InParamValue {
                        // Part of a non-string value
                        self.state.current_param_value.push(c);
                        if !self.state.collecting_special_value {
                            self.emit_parameter()?;
                        }
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
    
    /// Helper method to collect a complex value (object or array) as a single parameter value
    fn collect_complex_value(
        &mut self, 
        open_char: char, 
        close_char: char, 
        chars: &mut std::iter::Peekable<std::str::Chars>
    ) -> Result<bool, UIError> {
        let mut nesting = 1; // Start at 1 because we've already seen the opening character
        let mut in_string = false;
        let mut escaped = false;
        
        while let Some(&next) = chars.peek() {
            // Handle string quoting separately
            if next == '"' && !escaped {
                in_string = !in_string;
            } else if !in_string {
                // Only count nesting when not inside a string
                if next == open_char {
                    nesting += 1;
                } else if next == close_char {
                    nesting -= 1;
                    if nesting == 0 {
                        // We've found the matching closing character
                        self.state.current_param_value.push(next);
                        chars.next(); // Consume the character
                        self.emit_parameter()?;
                        
                        // Reset collecting flag if we're back at the top level
                        if self.state.param_value_nesting == 1 {
                            self.state.collecting_special_value = false;
                            self.state.param_value_nesting = 0;
                            
                            // Check if there's a comma after this array/object
                            if chars.peek().is_some_and(|&c| c == ',') {
                                // More parameters follow, prepare to process them
                                self.finalize_parameter()?;
                                self.state.json_state = JsonParseState::ExpectingParamName;
                            } else {
                                // End of object
                                self.finalize_parameter()?;
                                self.state.json_state = JsonParseState::ExpectingParamName;
                            }
                        }
                        return Ok(true);
                    }
                }
            }
            
            // Track escape character for strings
            escaped = next == '\\' && !escaped;
            
            // Add character to parameter value
            self.state.current_param_value.push(next);
            chars.next(); // Consume the character
        }
        
        // If we reach here, the complex value is incomplete
        self.emit_parameter()?;
        Ok(false)
    }
    
    /// Helper method to collect a primitive value (number or boolean)
    fn collect_primitive_value(
        &mut self, 
        chars: &mut std::iter::Peekable<std::str::Chars>
    ) -> Result<(), UIError> {
        while let Some(&next) = chars.peek() {
            match next {
                // Stop at delimiters
                ',' | '}' => break,
                // Stop at whitespace for primitive values
                ' ' | '\n' | '\r' | '\t' => break,
                // Otherwise, add to the value
                _ => {
                    self.state.current_param_value.push(next);
                    chars.next(); // Consume the character
                }
            }
        }
        Ok(())
    }

    /// Emit a parameter to the UI
    fn emit_parameter(&mut self) -> Result<(), UIError> {
        if let Some(param_name) = &self.state.current_param_name {
            if !self.state.current_param_value.is_empty() {
                self.ui.display_fragment(&DisplayFragment::ToolParameter {
                    name: param_name.clone(),
                    value: self.state.current_param_value.clone(),
                    tool_id: self.state.tool_id.clone(),
                })?;
                
                // Clear the current value after emitting, but keep the parameter name
                self.state.current_param_value.clear();
            }
        }
        Ok(())
    }
    
    /// Finalize the current parameter, resetting state for the next parameter
    fn finalize_parameter(&mut self) -> Result<(), UIError> {
        // Reset state for next parameter
        self.state.current_param_name = None;
        self.state.current_param_value.clear();
        self.state.collecting_special_value = false;
        Ok(())
    }
}
