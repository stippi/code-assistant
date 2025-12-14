use ratatui::prelude::*;
use std::collections::HashMap;
use tui_markdown as md;

use super::tool_widget::ToolWidget;
use crate::ui::ToolStatus;

/// A complete message containing multiple blocks
#[derive(Debug, Clone)]
pub struct LiveMessage {
    pub blocks: Vec<MessageBlock>,
    pub finalized: bool,
}

impl LiveMessage {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            finalized: false,
        }
    }

    /// Add a new block to this message
    pub fn add_block(&mut self, block: MessageBlock) {
        self.blocks.push(block);
    }

    /// Get the last block if it matches the expected type
    pub fn get_last_block_mut(&mut self) -> Option<&mut MessageBlock> {
        self.blocks.last_mut()
    }

    /// Get a mutable reference to a tool block by ID
    pub fn get_tool_block_mut(&mut self, tool_id: &str) -> Option<&mut ToolUseBlock> {
        for block in &mut self.blocks {
            if let MessageBlock::ToolUse(tool_block) = block {
                if tool_block.id == tool_id {
                    return Some(tool_block);
                }
            }
        }
        None
    }

    /// Check if this message has any content
    pub fn has_content(&self) -> bool {
        !self.blocks.is_empty() && self.blocks.iter().any(|block| block.has_content())
    }
}

/// Different types of blocks within a message
#[derive(Debug, Clone)]
pub enum MessageBlock {
    PlainText(PlainTextBlock),
    Thinking(ThinkingBlock),
    ToolUse(ToolUseBlock),
}

impl MessageBlock {
    /// Check if this block has any content
    pub fn has_content(&self) -> bool {
        match self {
            MessageBlock::PlainText(block) => !block.content.trim().is_empty(),
            MessageBlock::Thinking(block) => !block.content.trim().is_empty(),
            MessageBlock::ToolUse(block) => !block.name.is_empty(),
        }
    }

    /// Append content to the block (only for text-based blocks)
    pub fn append_content(&mut self, content: &str) {
        match self {
            MessageBlock::PlainText(block) => block.content.push_str(content),
            MessageBlock::Thinking(block) => block.content.push_str(content),
            MessageBlock::ToolUse(_) => {
                // Tool use blocks don't support general content appending
                // Parameter updates are handled separately
            }
        }
    }

    /// Calculate the height needed to render this block
    pub fn calculate_height(&self, width: u16) -> u16 {
        match self {
            MessageBlock::PlainText(block) => {
                if block.content.trim().is_empty() {
                    return 0;
                }
                // Account for text wrapping
                let mut total_lines = 0u16;
                for line in block.content.lines() {
                    if line.is_empty() {
                        total_lines += 1;
                    } else {
                        let wrapped_lines = (line.len() as u16 + width - 1) / width.max(1);
                        total_lines += wrapped_lines.max(1);
                    }
                }
                total_lines.max(1)
            }
            MessageBlock::Thinking(block) => {
                if block.content.trim().is_empty() {
                    return 0;
                }
                let formatted = format!("*{}*", block.content);
                // Account for text wrapping
                let mut total_lines = 0u16;
                for line in formatted.lines() {
                    if line.is_empty() {
                        total_lines += 1;
                    } else {
                        let wrapped_lines = (line.len() as u16 + width - 1) / width.max(1);
                        total_lines += wrapped_lines.max(1);
                    }
                }
                total_lines.max(1)
            }
            MessageBlock::ToolUse(block) => {
                let mut height = 1; // Tool name line

                // Check if we should show combined diff for completed edit tools
                let should_show_combined_diff = block.name == "edit"
                    && matches!(block.status, ToolStatus::Success | ToolStatus::Error)
                    && block.parameters.contains_key("old_text")
                    && block.parameters.contains_key("new_text");

                // Count parameter lines
                for (name, param) in &block.parameters {
                    if should_hide_parameter(&block.name, name, &param.value) {
                        continue;
                    }

                    // Skip old_text and new_text if we're showing combined diff
                    if should_show_combined_diff && (name == "old_text" || name == "new_text") {
                        continue;
                    }

                    if is_full_width_parameter(&block.name, name) {
                        height += 1; // Parameter name
                        height += param.value.lines().count() as u16; // Show all lines for full-width parameters
                    } else {
                        height += 1; // Regular parameter line
                    }
                }

                // Add height for combined diff if applicable
                if should_show_combined_diff {
                    if let (Some(old_param), Some(new_param)) = (
                        block.parameters.get("old_text"),
                        block.parameters.get("new_text"),
                    ) {
                        height += 1; // "diff" parameter name
                                     // Estimate diff height - this is approximate but should be close enough
                        let old_lines = old_param.value.lines().count();
                        let new_lines = new_param.value.lines().count();
                        height += (old_lines + new_lines) as u16; // Conservative estimate
                    }
                }

                // Status message
                if block.status_message.is_some() && block.status == ToolStatus::Error {
                    height += 1;
                }

                // Output (used by spawn_agent for streaming sub-agent activity)
                if let Some(ref output) = block.output {
                    if !output.is_empty() {
                        height += output.lines().count() as u16;
                    }
                }

                height
            }
        }
    }
}

impl Widget for MessageBlock {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self {
            MessageBlock::PlainText(block) => {
                if !block.content.trim().is_empty() {
                    let text = md::from_str(&block.content);
                    let paragraph = ratatui::widgets::Paragraph::new(text)
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    paragraph.render(area, buf);
                }
            }
            MessageBlock::Thinking(block) => {
                if !block.content.trim().is_empty() {
                    let formatted = format!("*{}*", block.content);
                    let text = md::from_str(&formatted);
                    let paragraph = ratatui::widgets::Paragraph::new(text)
                        .style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::ITALIC),
                        )
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    paragraph.render(area, buf);
                }
            }
            MessageBlock::ToolUse(block) => {
                let tool_widget = ToolWidget::new(&block);
                tool_widget.render(area, buf);
            }
        }
    }
}

/// Plain text block for regular assistant responses
#[derive(Debug, Clone)]
pub struct PlainTextBlock {
    pub content: String,
}

impl PlainTextBlock {
    pub fn new() -> Self {
        Self {
            content: String::new(),
        }
    }
}

/// Thinking block for assistant reasoning
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub content: String,
    pub start_time: std::time::Instant,
}

impl ThinkingBlock {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            start_time: std::time::Instant::now(),
        }
    }

    #[allow(dead_code)]
    pub fn formatted_duration(&self) -> String {
        let duration = self.start_time.elapsed();
        if duration.as_secs() < 60 {
            format!("{}s", duration.as_secs())
        } else {
            let minutes = duration.as_secs() / 60;
            let seconds = duration.as_secs() % 60;
            format!("{minutes}m{seconds}s")
        }
    }
}

/// Tool use block with parameters
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    pub name: String,
    pub id: String,
    pub parameters: HashMap<String, ParameterValue>,
    pub status: ToolStatus,
    pub status_message: Option<String>,
    pub output: Option<String>,
}

impl ToolUseBlock {
    pub fn new(name: String, id: String) -> Self {
        Self {
            name,
            id,
            parameters: HashMap::new(),
            status: ToolStatus::Pending,
            status_message: None,
            output: None,
        }
    }

    /// Add or update a parameter value
    pub fn add_or_update_parameter(&mut self, name: String, value: String) {
        match self.parameters.get_mut(&name) {
            Some(param) => param.append_value(&value),
            None => {
                self.parameters.insert(name, ParameterValue::new(value));
            }
        }
    }
}

/// Parameter value that can be streamed
#[derive(Debug, Clone)]
pub struct ParameterValue {
    pub value: String,
}

impl ParameterValue {
    pub fn new(value: String) -> Self {
        Self { value }
    }

    pub fn append_value(&mut self, content: &str) {
        self.value.push_str(content);
    }

    pub fn get_display_value(&self) -> String {
        // Truncate long values for regular parameters
        if self.value.len() > 100 {
            format!("{}...", &self.value[..97])
        } else {
            self.value.clone()
        }
    }
}

/// Check if a parameter should be rendered full-width
fn is_full_width_parameter(tool_name: &str, param_name: &str) -> bool {
    match (tool_name, param_name) {
        // Diff-style parameters
        ("replace_in_file", "diff") => true,
        ("edit", "old_text") => true,
        ("edit", "new_text") => true,
        // Content parameters
        ("write_file", "content") => true,
        // Large text parameters
        (_, "content") if param_name != "message" => true, // Exclude short message content
        (_, "output") => true,
        (_, "query") => true,
        _ => false,
    }
}

/// Check if a parameter should be hidden
fn should_hide_parameter(tool_name: &str, param_name: &str, param_value: &str) -> bool {
    match (tool_name, param_name) {
        (_, "project") => {
            // Hide project parameter if it's empty or matches common defaults
            param_value.is_empty() || param_value == "." || param_value == "unknown"
        }
        _ => false,
    }
}
