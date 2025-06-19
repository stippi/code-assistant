use super::{DisplayFragment, StreamProcessorTrait};
use crate::ui::{UIError, UserInterface};
use anyhow::Result;
use llm::{ContentBlock, Message, MessageContent, StreamingChunk};
use std::sync::Arc;

/// State for processing streaming text that may contain tags
#[derive(Default)]
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
    // Track if we're at the beginning of a block (thinking/param/tool)
    // Used to determine when to trim leading newlines
    at_block_start: bool,
    // Counter for tools processed in this request
    tool_counter: u64,
}

/// Manages the conversion of LLM streaming chunks to display fragments using XML-style tags
pub struct XmlStreamProcessor {
    state: ProcessorState,
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
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

// Implement the common StreamProcessorTrait
impl StreamProcessorTrait for XmlStreamProcessor {
    fn new(ui: Arc<Box<dyn UserInterface>>, request_id: u64) -> Self {
        Self {
            state: ProcessorState::default(),
            ui,
            request_id,
        }
    }

    /// Process a streaming chunk and send display fragments to the UI
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
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

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        match &message.content {
            MessageContent::Text(text) => {
                // Process text for XML tags, using request_id for consistent tool ID generation
                fragments.extend(self.extract_fragments_from_text(text, message.request_id)?);
            }
            MessageContent::Structured(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Thinking { thinking, .. } => {
                            fragments.push(DisplayFragment::ThinkingText(thinking.clone()));
                        }
                        ContentBlock::Text { text } => {
                            // Process text for XML tags, using request_id for consistent tool ID generation
                            fragments.extend(
                                self.extract_fragments_from_text(text, message.request_id)?,
                            );
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            // Convert JSON ToolUse to XML-style fragments
                            fragments.push(DisplayFragment::ToolName {
                                name: name.clone(),
                                id: id.clone(),
                            });

                            // Parse JSON input into XML-style tool parameters
                            if let Some(obj) = input.as_object() {
                                for (key, value) in obj {
                                    let value_str = if value.is_string() {
                                        value.as_str().unwrap_or("").to_string()
                                    } else {
                                        value.to_string()
                                    };

                                    fragments.push(DisplayFragment::ToolParameter {
                                        name: key.clone(),
                                        value: value_str,
                                        tool_id: id.clone(),
                                    });
                                }
                            }

                            fragments.push(DisplayFragment::ToolEnd { id: id.clone() });
                        }
                        ContentBlock::ToolResult { .. } => {
                            // Tool results are typically not part of assistant messages
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

impl XmlStreamProcessor {
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
                        let mut processed_text = pre_tag_text.to_string();

                        // Only trim one newline at the end if needed
                        if processed_text.ends_with('\n') {
                            processed_text.pop();
                        }

                        // Only trim one newline at the start if we're at a block start
                        if self.state.at_block_start && processed_text.starts_with('\n') {
                            processed_text = processed_text[1..].to_string();
                        }

                        // We are no longer at the start of a block after processing content
                        self.state.at_block_start = false;

                        if processed_text.is_empty() {
                            // Skip empty text after trimming
                            current_pos = absolute_tag_pos;
                            continue;
                        }

                        if self.state.in_thinking {
                            // In thinking mode, text is displayed as thinking text
                            self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                processed_text.to_string(),
                            ))?;
                        } else if self.state.in_param {
                            // In parameter mode, text is collected as a parameter value
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.param_name.clone(),
                                value: processed_text.to_string(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                        } else {
                            // All other text (including inside tool tags but not in params)
                            // is displayed as plain text
                            self.ui.display_fragment(&DisplayFragment::PlainText(
                                processed_text.to_string(),
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
                        // Set that we're at the start of a thinking block
                        self.state.at_block_start = true;

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ThinkingEnd => {
                        // Exit thinking mode
                        self.state.in_thinking = false;
                        // Set to true for next block to ensure newline trimming
                        self.state.at_block_start = true;

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ToolStart => {
                        // Extract the tool name from tag_info
                        if let Some(tool_name) = tag_info {
                            // Start a new tool section
                            self.state.in_tool = true;
                            self.state.tool_name = tool_name;

                            // For XML tools, generate ID based on request ID and tool counter
                            self.state.tool_counter += 1;
                            self.state.tool_id =
                                format!("tool-{}-{}", self.request_id, self.state.tool_counter);

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

                        // Set at_block_start to true for next block
                        self.state.at_block_start = true;

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
                            // Set that we're at the start of a parameter block
                            self.state.at_block_start = true;
                        }

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ParamEnd => {
                        // End parameter section
                        self.state.in_param = false;
                        self.state.param_name = String::new();
                        // Reset block start flag
                        self.state.at_block_start = false;

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
                    let mut processed_text = remaining.to_string();

                    // Only trim one newline at the end if needed
                    if processed_text.ends_with('\n') {
                        processed_text.pop();
                    }

                    // Only trim one newline at the start if we're at a block start
                    if self.state.at_block_start && processed_text.starts_with('\n') {
                        processed_text = processed_text[1..].to_string();
                    }

                    // We are no longer at the start of a block after processing content
                    self.state.at_block_start = false;

                    if !processed_text.is_empty() {
                        if self.state.in_thinking {
                            self.ui.display_fragment(&DisplayFragment::ThinkingText(
                                processed_text.to_string(),
                            ))?;
                        } else if self.state.in_param {
                            self.ui.display_fragment(&DisplayFragment::ToolParameter {
                                name: self.state.param_name.clone(),
                                value: processed_text.to_string(),
                                tool_id: self.state.tool_id.clone(),
                            })?;
                        } else {
                            self.ui.display_fragment(&DisplayFragment::PlainText(
                                processed_text.to_string(),
                            ))?;
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

    /// Extract fragments from text without sending to UI (used for session loading)
    fn extract_fragments_from_text(
        &mut self,
        text: &str,
        request_id: Option<u64>,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        // Local state for processing this text (don't modify self.state)
        let mut local_in_thinking = false;
        let mut local_in_tool = false;
        let mut local_in_param = false;
        let mut local_tool_name = String::new();
        let mut local_tool_id = String::new();
        let mut local_param_name = String::new();
        let mut tool_counter = 0u64;

        let mut current_pos = 0;

        while current_pos < text.len() {
            if let Some(tag_pos) = text[current_pos..].find('<') {
                let absolute_tag_pos = current_pos + tag_pos;

                // Process text before tag
                if tag_pos > 0 {
                    let pre_tag_text = &text[current_pos..absolute_tag_pos];
                    let processed_text = pre_tag_text.trim().to_string();

                    if !processed_text.is_empty() {
                        if local_in_thinking {
                            fragments.push(DisplayFragment::ThinkingText(processed_text));
                        } else if local_in_param {
                            fragments.push(DisplayFragment::ToolParameter {
                                name: local_param_name.clone(),
                                value: processed_text,
                                tool_id: local_tool_id.clone(),
                            });
                        } else {
                            fragments.push(DisplayFragment::PlainText(processed_text));
                        }
                    }
                }

                // Detect tag type
                let tag_slice = &text[absolute_tag_pos..];
                let (tag_type, tag_len, tag_info) = self.detect_tag(tag_slice);

                if tag_len > 0 {
                    match tag_type {
                        TagType::ThinkingStart => {
                            local_in_thinking = true;
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        TagType::ThinkingEnd => {
                            local_in_thinking = false;
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        TagType::ToolStart => {
                            if let Some(tool_name) = tag_info {
                                local_in_tool = true;
                                local_tool_name = tool_name;
                                tool_counter += 1;

                                // Generate consistent tool ID using request_id (same as live streaming)
                                local_tool_id = if let Some(req_id) = request_id {
                                    format!("tool-{}-{}", req_id, tool_counter)
                                } else {
                                    // Fallback for messages without request_id
                                    String::new()
                                };

                                fragments.push(DisplayFragment::ToolName {
                                    name: local_tool_name.clone(),
                                    id: local_tool_id.clone(),
                                });
                            }
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        TagType::ToolEnd => {
                            if local_in_tool {
                                fragments.push(DisplayFragment::ToolEnd {
                                    id: local_tool_id.clone(),
                                });
                                local_in_tool = false;
                                local_tool_name.clear();
                                local_tool_id.clear();
                            }
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        TagType::ParamStart => {
                            if let Some(param_name) = tag_info {
                                local_in_param = true;
                                local_param_name = param_name;
                            }
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        TagType::ParamEnd => {
                            local_in_param = false;
                            local_param_name.clear();
                            current_pos = absolute_tag_pos + tag_len;
                        }
                        TagType::None => {
                            // Not a recognized tag, process as regular character
                            let char_len = tag_slice.chars().next().map_or(1, |c| c.len_utf8());
                            let char_text = &text[absolute_tag_pos..absolute_tag_pos + char_len];

                            if local_in_thinking {
                                fragments
                                    .push(DisplayFragment::ThinkingText(char_text.to_string()));
                            } else if local_in_param {
                                fragments.push(DisplayFragment::ToolParameter {
                                    name: local_param_name.clone(),
                                    value: char_text.to_string(),
                                    tool_id: local_tool_id.clone(),
                                });
                            } else {
                                fragments.push(DisplayFragment::PlainText(char_text.to_string()));
                            }
                            current_pos = absolute_tag_pos + char_len;
                        }
                    }
                } else {
                    // Incomplete tag, process as regular character
                    let char_len = tag_slice.chars().next().map_or(1, |c| c.len_utf8());
                    let char_text = &text[absolute_tag_pos..absolute_tag_pos + char_len];

                    if local_in_thinking {
                        fragments.push(DisplayFragment::ThinkingText(char_text.to_string()));
                    } else if local_in_param {
                        fragments.push(DisplayFragment::ToolParameter {
                            name: local_param_name.clone(),
                            value: char_text.to_string(),
                            tool_id: local_tool_id.clone(),
                        });
                    } else {
                        fragments.push(DisplayFragment::PlainText(char_text.to_string()));
                    }
                    current_pos = absolute_tag_pos + char_len;
                }
            } else {
                // No more tags, process remaining text
                let remaining = &text[current_pos..];
                if !remaining.is_empty() {
                    let processed_text = remaining.trim().to_string();

                    if !processed_text.is_empty() {
                        if local_in_thinking {
                            fragments.push(DisplayFragment::ThinkingText(processed_text));
                        } else if local_in_param {
                            fragments.push(DisplayFragment::ToolParameter {
                                name: local_param_name.clone(),
                                value: processed_text,
                                tool_id: local_tool_id.clone(),
                            });
                        } else {
                            fragments.push(DisplayFragment::PlainText(processed_text));
                        }
                    }
                }
                current_pos = text.len();
            }
        }

        Ok(fragments)
    }
}
