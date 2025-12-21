use super::{DisplayFragment, StreamProcessorTrait};
use crate::tools::core::{ToolRegistry, ToolScope};
use crate::tools::tool_use_filter::{SmartToolFilter, ToolUseFilter};
use crate::ui::{UIError, UserInterface};
use anyhow::Result;
use llm::{ContentBlock, Message, MessageContent, ReasoningSummaryItem, StreamingChunk};
use std::sync::Arc;
use tracing::warn;

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
    // Track if the current tool is hidden
    current_tool_hidden: bool,
}

/// Streaming state for managing tool filtering and buffering
#[derive(Debug, PartialEq, Clone)]
enum StreamingState {
    /// Stream everything immediately (before any tools)
    PreFirstTool,
    /// Buffer content between tools until next tool is evaluated
    BufferingAfterTool {
        last_tool_name: String,
        buffered_fragments: Vec<DisplayFragment>,
    },
    /// Stop streaming (tool was denied)
    Blocked,
}

/// Tracks the last type of text fragment emitted (for paragraph breaks after hidden tools)
#[derive(Debug, Clone, Copy, PartialEq)]
enum LastFragmentType {
    None,
    PlainText,
    ThinkingText,
}

/// Manages the conversion of LLM streaming chunks to display fragments using XML-style tags
pub struct XmlStreamProcessor {
    state: ProcessorState,
    ui: Arc<dyn UserInterface>,
    request_id: u64,
    filter: Box<dyn ToolUseFilter>,
    streaming_state: StreamingState,
    /// Tracks the last emitted text fragment type for paragraph breaks after hidden tools
    last_fragment_type: LastFragmentType,
    /// Flag indicating a hidden tool was just suppressed and we need a paragraph break
    /// if the next fragment is the same type as the last one
    needs_paragraph_break_if_same_type: bool,
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
    fn new(ui: Arc<dyn UserInterface>, request_id: u64) -> Self {
        Self {
            state: ProcessorState::default(),
            ui,
            request_id,

            filter: Box::new(SmartToolFilter::new()),
            streaming_state: StreamingState::PreFirstTool,
            last_fragment_type: LastFragmentType::None,
            needs_paragraph_break_if_same_type: false,
        }
    }

    /// Process a streaming chunk and send display fragments to the UI
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        match chunk {
            // For native thinking chunks, send directly as ThinkingText
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

            // For native JSON input, handle based on tool information
            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                // If this is the first part with tool info, send a ToolName fragment
                if let (Some(name), Some(id)) = (tool_name, tool_id) {
                    if !name.is_empty() && !id.is_empty() {
                        // Check if tool is hidden and update state
                        self.state.current_tool_hidden =
                            ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent);

                        self.emit_fragment(DisplayFragment::ToolName {
                            name: name.clone(),
                            id: id.clone(),
                        })?;
                    }
                }

                // For now, show the JSON as plain text
                // In a more advanced implementation, we could parse the JSON
                // and extract parameter names/values
                self.emit_fragment(DisplayFragment::PlainText(content.clone()))
            }

            StreamingChunk::StreamingComplete => {
                // Emit any remaining buffered fragments since no more content is coming
                self.flush_buffered_content()
            }

            // For text chunks, we need to parse for tags
            StreamingChunk::Text(text) => self.process_text_with_tags(text),

            StreamingChunk::ReasoningSummaryStart => {
                self.emit_fragment(DisplayFragment::ReasoningSummaryStart)
            }

            StreamingChunk::ReasoningSummaryDelta(delta) => {
                self.emit_fragment(DisplayFragment::ReasoningSummaryDelta(delta.clone()))
            }

            StreamingChunk::ReasoningComplete => {
                self.emit_fragment(DisplayFragment::ReasoningComplete)
            }
        }
    }

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let mut fragments = Vec::new();

        // For User messages, don't process XML tags - just create a single PlainText fragment
        if message.role == llm::MessageRole::User {
            match &message.content {
                MessageContent::Text(text) => {
                    if !text.trim().is_empty() {
                        fragments.push(DisplayFragment::PlainText(text.clone()));
                    }
                }
                MessageContent::Structured(blocks) => {
                    // For structured user messages, combine all text into a single fragment
                    let mut combined_text = String::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text, .. } => {
                                combined_text.push_str(text);
                            }
                            ContentBlock::ToolResult { content, .. } => {
                                // Include tool result content for user messages
                                combined_text.push_str(content);
                            }
                            _ => {} // Skip other block types for user messages
                        }
                    }
                    if !combined_text.trim().is_empty() {
                        fragments.push(DisplayFragment::PlainText(combined_text));
                    }
                }
            }
        } else {
            // For Assistant messages, process normally with XML tag parsing
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
                            ContentBlock::Text { text, .. } => {
                                // Process text for XML tags, using request_id for consistent tool ID generation
                                fragments.extend(
                                    self.extract_fragments_from_text(text, message.request_id)?,
                                );
                            }
                            ContentBlock::ToolUse {
                                id, name, input, ..
                            } => {
                                // Check if tool is hidden
                                let tool_hidden =
                                    ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent);

                                // Only add fragments if tool is not hidden
                                if !tool_hidden {
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
                            }
                            ContentBlock::ToolResult { .. } => {
                                // Tool results are typically not part of assistant messages
                            }
                            ContentBlock::RedactedThinking { summary, .. } => {
                                // Generate reasoning summary fragments for each item, emitting raw content
                                // exactly as it would come from streaming API
                                for item in summary {
                                    fragments.push(DisplayFragment::ReasoningSummaryStart);
                                    match item {
                                        ReasoningSummaryItem::SummaryText { text } => {
                                            fragments.push(DisplayFragment::ReasoningSummaryDelta(
                                                text.clone(),
                                            ));
                                        }
                                    }
                                }
                                // End with reasoning complete if we had items
                                if !summary.is_empty() {
                                    fragments.push(DisplayFragment::ReasoningComplete);
                                }
                            }
                            ContentBlock::Image {
                                media_type, data, ..
                            } => {
                                // Images in assistant messages - preserve for display
                                fragments.push(DisplayFragment::Image {
                                    media_type: media_type.clone(),
                                    data: data.clone(),
                                });
                            }
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

                        self.emit_text(processed_text.as_str())?;
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

                            // Check if tool is hidden and update state
                            self.state.current_tool_hidden = ToolRegistry::global()
                                .is_tool_hidden(&self.state.tool_name, ToolScope::Agent);

                            // Send fragment with tool name
                            self.emit_fragment(DisplayFragment::ToolName {
                                name: self.state.tool_name.clone(),
                                id: self.state.tool_id.clone(),
                            })?;
                        }

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ToolEnd => {
                        // Only send ToolEnd fragment if we're actually in a tool and have a valid tool ID
                        if self.state.in_tool && !self.state.tool_id.is_empty() {
                            let tool_id = self.state.tool_id.clone();

                            // Send fragment for tool end with the original tool ID
                            self.emit_fragment(DisplayFragment::ToolEnd { id: tool_id })?;
                        }

                        // Always reset tool state when we see any tool end tag
                        self.state.in_tool = false;
                        self.state.tool_name = String::new();
                        self.state.tool_id = String::new();
                        // Also reset parameter state when closing tool
                        self.state.in_param = false;
                        self.state.param_name = String::new();

                        // Set at_block_start to true for next block
                        self.state.at_block_start = true;

                        // Skip past this tag
                        current_pos = absolute_tag_pos + tag_len;
                    }

                    TagType::ParamStart => {
                        // Only process parameter tags if we're inside a tool
                        if self.state.in_tool && !self.state.tool_id.is_empty() {
                            // Extract parameter name from tag_info
                            if let Some(param_name) = tag_info {
                                self.state.in_param = true;
                                self.state.param_name = param_name;
                                // Set that we're at the start of a parameter block
                                self.state.at_block_start = true;
                            }
                            // Skip past this tag
                            current_pos = absolute_tag_pos + tag_len;
                        } else {
                            // Log and treat parameter tags outside of tool context as plain text
                            warn!("Parameter tag found outside of tool context, treating as plain text");
                            // Process as a single character (the '<')
                            let char_len = processing_text[absolute_tag_pos..]
                                .chars()
                                .next()
                                .map_or(1, |c| c.len_utf8());
                            let char_text =
                                &processing_text[absolute_tag_pos..absolute_tag_pos + char_len];

                            self.emit_text(char_text)?;
                            current_pos = absolute_tag_pos + char_len;
                        }
                    }

                    TagType::ParamEnd => {
                        // Only process parameter end tags if we're actually in a parameter
                        if self.state.in_param
                            && self.state.in_tool
                            && !self.state.tool_id.is_empty()
                        {
                            // End parameter section
                            self.state.in_param = false;
                            self.state.param_name = String::new();
                            // Reset block start flag
                            self.state.at_block_start = false;
                            // Skip past this tag
                            current_pos = absolute_tag_pos + tag_len;
                        } else {
                            // Treat as plain text if not in valid parameter context
                            warn!("Parameter end tag found outside of parameter context, treating as plain text");
                            // Process as a single character (the '<')
                            let char_len = processing_text[absolute_tag_pos..]
                                .chars()
                                .next()
                                .map_or(1, |c| c.len_utf8());
                            let char_text =
                                &processing_text[absolute_tag_pos..absolute_tag_pos + char_len];

                            self.emit_text(char_text)?;
                            current_pos = absolute_tag_pos + char_len;
                        }
                    }

                    TagType::None => {
                        // Not a recognized tag, treat as regular character
                        // Ensure we're processing complete characters by using char iterators
                        if let Some(first_char) = processing_text[absolute_tag_pos..].chars().next()
                        {
                            let char_len = first_char.len_utf8();
                            let single_char = first_char.to_string();

                            self.emit_text(single_char.as_str())?;

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
                        self.emit_text(processed_text.as_str())?;
                    }
                }
                current_pos = processing_text.len();
            }
        }

        Ok(())
    }

    fn emit_text(&mut self, text: &str) -> Result<(), UIError> {
        let fragment = if self.state.in_thinking {
            // In thinking mode, text is displayed as thinking text
            DisplayFragment::ThinkingText(text.to_string())
        } else if self.state.in_param {
            // In parameter mode, text is collected as a parameter value
            // Only send if we have a valid tool_id
            if !self.state.tool_id.is_empty() {
                DisplayFragment::ToolParameter {
                    name: self.state.param_name.clone(),
                    value: text.to_string(),
                    tool_id: self.state.tool_id.clone(),
                }
            } else {
                // Log the issue and treat as plain text
                warn!(
                    "Parameter '{}' found outside of tool context, treating as plain text",
                    self.state.param_name
                );
                DisplayFragment::PlainText(text.to_string())
            }
        } else {
            // All other text (including inside tool tags but not in params)
            // is displayed as plain text
            DisplayFragment::PlainText(text.to_string())
        };

        self.emit_fragment(fragment)
    }

    /// Emit fragments through this central function that handles filtering and buffering
    fn emit_fragment(&mut self, fragment: DisplayFragment) -> Result<(), UIError> {
        // Filter out tool-related fragments for hidden tools
        if self.state.current_tool_hidden {
            match &fragment {
                DisplayFragment::ToolName { .. } | DisplayFragment::ToolParameter { .. } => {
                    // Skip tool-related fragments for hidden tools
                    return Ok(());
                }
                DisplayFragment::ToolEnd { .. } => {
                    // Set flag to emit paragraph break lazily if the next fragment
                    // is the same type as the last one
                    self.needs_paragraph_break_if_same_type = true;
                    return Ok(());
                }
                _ => {
                    // Allow non-tool fragments even when current tool is hidden
                }
            }
        }

        self.emit_fragment_inner(fragment)
    }

    /// Inner emit function that handles streaming state and actually sends fragments
    fn emit_fragment_inner(&mut self, fragment: DisplayFragment) -> Result<(), UIError> {
        // Check if we need to emit a paragraph break before this fragment
        // (only if fragment type matches the last one after a hidden tool was suppressed)
        if self.needs_paragraph_break_if_same_type {
            let current_type = match &fragment {
                DisplayFragment::PlainText(_) => Some(LastFragmentType::PlainText),
                DisplayFragment::ThinkingText(_) => Some(LastFragmentType::ThinkingText),
                _ => None,
            };

            if let Some(current) = current_type {
                if current == self.last_fragment_type {
                    // Same type as before the hidden tool - emit paragraph break first
                    let paragraph_break = match current {
                        LastFragmentType::ThinkingText => {
                            DisplayFragment::ThinkingText("\n\n".to_string())
                        }
                        _ => DisplayFragment::PlainText("\n\n".to_string()),
                    };
                    // Emit the paragraph break (bypassing this check by going directly to streaming state)
                    self.emit_fragment_to_streaming_state(paragraph_break)?;
                }
                // Reset flag regardless of whether we emitted (type changed or same)
                self.needs_paragraph_break_if_same_type = false;
            }
        }

        // Track last fragment type for paragraph breaks after hidden tools
        match &fragment {
            DisplayFragment::PlainText(_) => self.last_fragment_type = LastFragmentType::PlainText,
            DisplayFragment::ThinkingText(_) => {
                self.last_fragment_type = LastFragmentType::ThinkingText
            }
            _ => {}
        }

        self.emit_fragment_to_streaming_state(fragment)
    }

    /// Send fragment to streaming state machine (handles buffering logic)
    fn emit_fragment_to_streaming_state(
        &mut self,
        fragment: DisplayFragment,
    ) -> Result<(), UIError> {
        match &self.streaming_state {
            StreamingState::Blocked => {
                // Already blocked, check if this is just whitespace and ignore silently
                if let DisplayFragment::PlainText(text) = &fragment {
                    if text.trim().is_empty() {
                        return Ok(());
                    }
                }
                // Non-whitespace content after blocking - return the blocking error
                return Err(UIError::IOError(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Tool limit reached - no additional text after complete tool block allowed",
                )));
            }
            StreamingState::PreFirstTool => {
                // Before first tool, emit everything immediately
                match &fragment {
                    DisplayFragment::ToolName { name, .. } => {
                        // First tool starting - check if it's allowed (should always be allowed)
                        if !self.filter.allow_tool_at_position(name, 1) {
                            // First tool denied - block streaming
                            self.streaming_state = StreamingState::Blocked;
                            return Err(UIError::IOError(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "Tool limit reached - no additional text after complete tool block allowed",
                            )));
                        }
                        // Emit the tool name fragment
                        self.ui.display_fragment(&fragment)?;
                    }
                    DisplayFragment::ToolEnd { .. } => {
                        // Tool ended - emit fragment and check if we should buffer after this tool
                        self.ui.display_fragment(&fragment)?;

                        // Get the tool name from processor state
                        let tool_name = self.state.tool_name.clone();

                        if self.filter.allow_content_after_tool(&tool_name, 1) {
                            // Transition to buffering state
                            self.streaming_state = StreamingState::BufferingAfterTool {
                                last_tool_name: tool_name,
                                buffered_fragments: Vec::new(),
                            };
                        } else {
                            // No content allowed after this tool - block
                            self.streaming_state = StreamingState::Blocked;
                        }
                    }
                    _ => {
                        // Regular fragment - emit immediately
                        self.ui.display_fragment(&fragment)?;
                    }
                }
            }
            StreamingState::BufferingAfterTool {
                last_tool_name,
                buffered_fragments,
            } => {
                match &fragment {
                    DisplayFragment::ToolName { name, .. } => {
                        // New tool starting - check if it's allowed
                        let tool_count = self.state.tool_counter + 1; // Next tool count
                        if self
                            .filter
                            .allow_tool_at_position(name, tool_count as usize)
                        {
                            // Tool allowed - emit all buffered fragments first
                            let mut buffered = buffered_fragments.clone();
                            for buffered_fragment in buffered.drain(..) {
                                self.ui.display_fragment(&buffered_fragment)?;
                            }
                            // Then emit the tool name fragment
                            self.ui.display_fragment(&fragment)?;
                            // Update state to continue buffering after this tool
                            self.streaming_state = StreamingState::BufferingAfterTool {
                                last_tool_name: name.clone(),
                                buffered_fragments: Vec::new(),
                            };
                        } else {
                            // Tool denied - discard buffered content and block
                            self.streaming_state = StreamingState::Blocked;
                            return Err(UIError::IOError(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "Tool limit reached - no additional text after complete tool block allowed",
                            )));
                        }
                    }
                    DisplayFragment::ToolEnd { .. } => {
                        // Tool ended - emit fragment and check if we should continue buffering
                        self.ui.display_fragment(&fragment)?;

                        let tool_name = last_tool_name.clone();
                        let tool_count = self.state.tool_counter as usize;

                        if self.filter.allow_content_after_tool(&tool_name, tool_count) {
                            // Continue buffering
                            self.streaming_state = StreamingState::BufferingAfterTool {
                                last_tool_name: tool_name,
                                buffered_fragments: Vec::new(),
                            };
                        } else {
                            // No content allowed after this tool - block
                            self.streaming_state = StreamingState::Blocked;
                        }
                    }
                    DisplayFragment::ToolParameter { .. } => {
                        // Tool parameter - emit immediately (we've already decided to allow the tool)
                        self.ui.display_fragment(&fragment)?;
                    }
                    DisplayFragment::PlainText(_)
                    | DisplayFragment::ThinkingText(_)
                    | DisplayFragment::CompactionDivider { .. } => {
                        // Text or thinking - buffer it
                        if let StreamingState::BufferingAfterTool {
                            buffered_fragments, ..
                        } = &mut self.streaming_state
                        {
                            buffered_fragments.push(fragment);
                        }
                    }
                    DisplayFragment::Image { .. } => {
                        // Image - buffer it
                        if let StreamingState::BufferingAfterTool {
                            buffered_fragments, ..
                        } = &mut self.streaming_state
                        {
                            buffered_fragments.push(fragment);
                        }
                    }
                    DisplayFragment::ReasoningSummaryStart
                    | DisplayFragment::ReasoningSummaryDelta(_) => {
                        // Reasoning summary - buffer it
                        if let StreamingState::BufferingAfterTool {
                            buffered_fragments, ..
                        } = &mut self.streaming_state
                        {
                            buffered_fragments.push(fragment);
                        }
                    }
                    DisplayFragment::ToolOutput { .. } | DisplayFragment::ToolTerminal { .. } => {
                        // Tool output - emit immediately (we've already decided to allow the tool)
                        self.ui.display_fragment(&fragment)?;
                    }
                    DisplayFragment::ReasoningComplete => {
                        // Reasoning complete - buffer it
                        if let StreamingState::BufferingAfterTool {
                            buffered_fragments, ..
                        } = &mut self.streaming_state
                        {
                            buffered_fragments.push(fragment);
                        }
                    }
                }
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
                                    format!("tool-{req_id}-{tool_counter}")
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
                            // Only add ToolEnd fragment if we're in a tool and have a valid tool ID
                            if local_in_tool && !local_tool_id.is_empty() {
                                fragments.push(DisplayFragment::ToolEnd {
                                    id: local_tool_id.clone(),
                                });
                            }
                            // Always reset tool state when we see any tool end tag
                            local_in_tool = false;
                            local_tool_name.clear();
                            local_tool_id.clear();
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

    /// Flush any buffered content when streaming completes
    fn flush_buffered_content(&mut self) -> Result<(), UIError> {
        if let StreamingState::BufferingAfterTool {
            buffered_fragments, ..
        } = &mut self.streaming_state
        {
            // Emit all buffered fragments since no more tools are coming
            for fragment in buffered_fragments.drain(..) {
                self.ui.display_fragment(&fragment)?;
            }
        }
        Ok(())
    }
}
