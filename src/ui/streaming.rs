use crate::llm::StreamingChunk;
use crate::ui::{UIError, UserInterface};
use anyhow::Result;
use std::sync::Arc;

/// Fragments for display in UI components
#[derive(Debug, Clone)]
pub enum DisplayFragment {
    /// Regular plain text
    PlainText(String),
    /// Thinking text (shown differently)
    ThinkingText(String),
    /// Tool invocation start
    ToolName { name: String, id: String },
    /// Parameter for a tool
    ToolParameter {
        name: String,
        value: String,
        tool_id: String,
    },
    /// End of a tool invocation
    ToolEnd { id: String },
}

/// State for processing streaming text that may contain tags
struct ProcessorState {
    // Buffer for collecting partial text
    buffer: String,
    // Track if we're inside thinking tags
    in_thinking: bool,
    // Track if we're inside tool tags
    in_tool: bool,
    // Track if we're inside param tags
    in_param: bool,
    // Current active tool name (if any)
    tool_name: String,
    // Current tool ID (if any)
    tool_id: String,
    // Current parameter name (if any)
    param_name: String,
}

impl Default for ProcessorState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            in_thinking: false,
            in_tool: false,
            in_param: false,
            tool_name: String::new(),
            tool_id: String::new(),
            param_name: String::new(),
        }
    }
}

/// Manages the conversion of LLM streaming chunks to display fragments
pub struct StreamProcessor {
    state: ProcessorState,
    ui: Arc<Box<dyn UserInterface>>,
}

// Define tag types we need to process
enum TagType {
    None,
    ThinkingStart,
    ThinkingEnd,
    ToolStart,
    ToolEnd,
    ParamStart,
    ParamEnd,
}

impl StreamProcessor {
    pub fn new(ui: Arc<Box<dyn UserInterface>>) -> Self {
        Self {
            state: ProcessorState::default(),
            ui,
        }
    }

    /// Process a streaming chunk and send display fragments to the UI
    pub fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        match chunk {
            // For native thinking chunks, send directly as ThinkingText
            StreamingChunk::Thinking(text) => self
                .ui
                .display_fragment(&DisplayFragment::ThinkingText(text.clone())),

            // For native JSON input, handle based on tool information
            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                // If this is the first part with tool info, send a ToolName fragment
                if let (Some(name), Some(id)) = (tool_name, tool_id) {
                    if !name.is_empty() && !id.is_empty() {
                        self.ui.display_fragment(&DisplayFragment::ToolName {
                            name: name.clone(),
                            id: id.clone(),
                        })?;
                    }
                }

                // For now, show the JSON as plain text
                // In a more advanced implementation, we could parse the JSON
                // and extract parameter names/values
                self.ui
                    .display_fragment(&DisplayFragment::PlainText(content.clone()))
            }

            // For text chunks, we need to parse for tags
            StreamingChunk::Text(text) => self.process_text_with_tags(text),
        }
    }

    /// Process text that may contain <thinking>, <tool:>, and <param:> tags
    fn process_text_with_tags(&mut self, text: &str) -> Result<(), UIError> {
        // Combine buffer with new text
        let current_text = format!("{}{}", self.state.buffer, text);

        // Check if the end of text could be a partial tag
        // If so, save it to buffer and only process the rest
        let mut processing_text = current_text.clone();
        let mut safe_length = processing_text.len();

        // Check backwards for potential tag starts
        for j in (1..=processing_text.len().min(120)).rev() {
            // Check at most last 120 chars
            // Make sure we're at a valid char boundary
            if !processing_text.is_char_boundary(processing_text.len() - j) {
                continue;
            }

            let suffix = &processing_text[processing_text.len() - j..];
            
            // Special case for newlines at the end that might be followed by a tag in the next chunk
            if suffix.ends_with('\n') && j == 1 {
                // Only hold back the newline if it's the very last character
                safe_length = processing_text.len() - 1;
                self.state.buffer = "\n".to_string();
                break;
            } else if self.is_potential_tag_start(suffix) && suffix != "\n" {
                // We found a potential tag start (not just a newline), buffer this part
                safe_length = processing_text.len() - j;
                self.state.buffer = suffix.to_string();
                break;
            }
        }

        // Only process text up to safe_length, ensuring we end at a char boundary
        if safe_length < processing_text.len() {
            // Ensure safe_length is at a char boundary
            while safe_length > 0 && !processing_text.is_char_boundary(safe_length) {
                safe_length -= 1;
            }
            processing_text = processing_text[..safe_length].to_string();
        } else {
            // No potential tag at end, clear buffer
            self.state.buffer.clear();
        }

        // Process the text for tags
        let mut current_pos = 0;

        // While we have content to process
        while current_pos < processing_text.len() {
            // Look for next tag
            if let Some(tag_pos) = processing_text[current_pos..].find('<') {
                let absolute_tag_pos = current_pos + tag_pos;

                // Output all text before this tag if there is any
                if tag_pos > 0 {
                    let pre_tag_text = &processing_text[current_pos..absolute_tag_pos];

                    // Skip if the text is just whitespace and we're about to process a tag
                    // This prevents creating unnecessary whitespace fragments between tags
                    let is_only_whitespace = pre_tag_text.trim().is_empty();

                    if !is_only_whitespace {
                        // Only trim newlines at tag boundaries, preserving newlines within text
                        let trimmed_text = pre_tag_text.trim_end_matches('\n');
                        // If we're at the start of processing (current_pos == 0), also trim leading newlines
                        // or if the previous char was the end of a tag
                        let is_at_start = current_pos == 0 || 
                            (current_pos > 0 && processing_text.chars().nth(current_pos-1) == Some('>'));
                        
                        let trimmed_text = if is_at_start {
                            trimmed_text.trim_start_matches('\n')
                        } else {
                            trimmed_text
                        };
                        
                        if trimmed_text.is_empty() {
                            // Skip empty text after trimming
                            current_pos = absolute_tag_pos;
                            continue;
                        }
                        
                        if self.state.in_thinking {
                            // In thinking mode, text is displayed as thinking text
                            self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                trimmed_text.to_string(),
                            ))?;
                        } else if self.state.in_param {
                            // In parameter mode, text is collected as a parameter value
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.param_name.clone(),
                                value: trimmed_text.to_string(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                        } else {
                            // All other text (including inside tool tags but not in params)
                            // is displayed as plain text
                            self.ui.display_fragment(&DisplayFragment::PlainText(
                                trimmed_text.to_string(),
                            ))?;
                        }
                    }
                }

                // Determine what kind of tag we're looking at
                let tag_slice = &processing_text[absolute_tag_pos..];
                let (tag_type, tag_len, tag_info) = self.detect_tag(tag_slice);

                // Check if we have a complete tag
                match tag_type {
                    TagType::None => {}
                    _ => {
                        if tag_len > 0 && absolute_tag_pos + tag_len > processing_text.len() {
                            // Incomplete tag found, buffer the rest and stop processing
                            self.state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }
                }

                match tag_type {
                    TagType::ThinkingStart => {
                        // Mark that we're in thinking mode
                        self.state.in_thinking = true;

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ThinkingEnd => {
                        // Exit thinking mode
                        self.state.in_thinking = false;

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ToolStart => {
                        // Extract the tool name from tag_info
                        if let Some(tool_name) = tag_info {
                            // Start a new tool section
                            self.state.in_tool = true;
                            self.state.tool_name = tool_name;
                            // Use a deterministic ID for tests
                            #[cfg(test)]
                            {
                                self.state.tool_id = "ignored".to_string();
                            }
                            #[cfg(not(test))]
                            {
                                self.state.tool_id = format!("tool-{}", rand::random::<u16>());
                            }

                            // Send fragment with tool name
                            self.ui.display_fragment(&DisplayFragment::ToolName {
                                name: self.state.tool_name.clone(),
                                id: self.state.tool_id.clone(),
                            })?;
                        }

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ToolEnd => {
                        // End a tool section
                        let tool_id = self.state.tool_id.clone();
                        self.state.in_tool = false;
                        self.state.tool_name = String::new();
                        self.state.tool_id = String::new();

                        // Send fragment for tool end
                        self.ui
                            .display_fragment(&DisplayFragment::ToolEnd { id: tool_id })?;

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ParamStart => {
                        // Extract parameter name from tag_info
                        if let Some(param_name) = tag_info {
                            self.state.in_param = true;
                            self.state.param_name = param_name;
                        }

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ParamEnd => {
                        // End parameter section
                        self.state.in_param = false;
                        self.state.param_name = String::new();

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::None => {
                        // Not a recognized tag, treat as regular character
                        // Ensure we're processing complete characters by using char iterators
                        if let Some(first_char) = processing_text[absolute_tag_pos..].chars().next()
                        {
                            let char_len = first_char.len_utf8();
                            let single_char = first_char.to_string();

                            if self.state.in_thinking {
                                self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                    single_char,
                                ))?;
                            } else if self.state.in_param {
                                // Handle characters in parameters
                                self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                    name: self.state.param_name.clone(),
                                    value: single_char,
                                    tool_id: self.state.tool_id.clone(),
                                })?;
                            } else {
                                self.ui
                                    .display_fragment(&DisplayFragment::PlainText(single_char))?;
                            }

                            // Move forward by the full character length
                            current_pos = absolute_tag_pos + char_len;
                        } else {
                            // Shouldn't happen, but just in case
                            current_pos = absolute_tag_pos + 1;
                        }
                    }
                }
            } else {
                // No more tags, output the rest of the text
                let remaining = &processing_text[current_pos..];

                // Only process if there's actual content (not just whitespace)
                if !remaining.is_empty() && !remaining.trim().is_empty() {
                    // Only trim trailing newlines at end of processing
                    let trimmed_text = remaining.trim_end_matches('\n');
                    
                    // If we're at the start of processing (current_pos == 0), also trim leading newlines
                    let is_at_start = current_pos == 0 || 
                        (current_pos > 0 && processing_text.chars().nth(current_pos-1) == Some('>'));
                    
                    let trimmed_text = if is_at_start {
                        trimmed_text.trim_start_matches('\n')
                    } else {
                        trimmed_text
                    };
                    
                    if !trimmed_text.is_empty() {
                        if self.state.in_thinking {
                            self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                trimmed_text.to_string(),
                            ))?;
                        } else if self.state.in_param {
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.param_name.clone(),
                                value: trimmed_text.to_string(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                        } else {
                            self.ui
                                .display_fragment(&DisplayFragment::PlainText(trimmed_text.to_string()))?;
                        }
                    }
                }
                current_pos = processing_text.len();
            }
        }

        Ok(())
    }

    /// Detect what kind of tag we're seeing and extract any tag information
    fn detect_tag(&self, text: &str) -> (TagType, usize, Option<String>) {
        if text.starts_with("<thinking>") {
            (TagType::ThinkingStart, 10, None)
        } else if text.starts_with("</thinking>") {
            (TagType::ThinkingEnd, 11, None)
        } else if text.starts_with("<tool:") {
            if let Some(end_pos) = text.find('>') {
                let tool_name = if end_pos > 6 {
                    text[6..end_pos].to_string()
                } else {
                    "unknown".to_string()
                };
                (TagType::ToolStart, end_pos + 1, Some(tool_name))
            } else {
                // Incomplete tool tag
                (TagType::ToolStart, 0, None)
            }
        } else if text.starts_with("</tool:") {
            if let Some(end_pos) = text.find('>') {
                (TagType::ToolEnd, end_pos + 1, None)
            } else {
                // Incomplete tool end tag
                (TagType::ToolEnd, 0, None)
            }
        } else if text.starts_with("<param:") {
            if let Some(end_pos) = text.find('>') {
                let param_name = if end_pos > 7 {
                    text[7..end_pos].to_string()
                } else {
                    "param".to_string()
                };
                (TagType::ParamStart, end_pos + 1, Some(param_name))
            } else {
                // Incomplete param tag
                (TagType::ParamStart, 0, None)
            }
        } else if text.starts_with("</param:") {
            if let Some(end_pos) = text.find('>') {
                (TagType::ParamEnd, end_pos + 1, None)
            } else {
                // Incomplete param end tag
                (TagType::ParamEnd, 0, None)
            }
        } else {
            (TagType::None, 0, None)
        }
    }

    /// Check if a string is a potential beginning of a tag
    fn is_potential_tag_start(&self, text: &str) -> bool {
        // Tag prefixes to check for
        const TAG_PREFIXES: [&str; 6] = [
            "<thinking>",
            "</thinking>",
            "<tool:",
            "</tool:",
            "<param:",
            "</param:",
        ];

        // Check if the text could be the start of any tag
        for prefix in &TAG_PREFIXES {
            // Loop through all possible partial matches
            for i in 1..=prefix.len().min(text.len()) {
                // Check if the end of text matches the beginning of a tag prefix
                if &text[text.len() - i..] == &prefix[..i] {
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
