use indexmap::IndexMap;
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use tui_markdown as md;

use super::tool_renderers::ToolRendererRegistry;
use super::tool_widget::{is_full_width_parameter, should_hide_parameter, ToolWidget};
use crate::ui::ToolStatus;

/// A complete message containing multiple blocks
#[derive(Debug, Clone)]
pub struct LiveMessage {
    pub blocks: Vec<MessageBlock>,
    pub finalized: bool,
    /// When true, the committed stream lines for this message were progressively
    /// sent to scrollback during streaming. Only the final tail needs to be sent
    /// on finalization — the bulk of the content is already in scrollback.
    pub streamed_to_scrollback: bool,
}

impl LiveMessage {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            finalized: false,
            streamed_to_scrollback: false,
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
    UserText(PlainTextBlock),
}

impl MessageBlock {
    /// Check if this block has any content
    pub fn has_content(&self) -> bool {
        match self {
            MessageBlock::PlainText(block) => !block.content.trim().is_empty(),
            MessageBlock::Thinking(block) => !block.content.trim().is_empty(),
            MessageBlock::ToolUse(block) => !block.name.is_empty(),
            MessageBlock::UserText(block) => !block.content.trim().is_empty(),
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
            MessageBlock::UserText(block) => block.content.push_str(content),
        }
    }

    /// Width reserved for the left indent on text/thinking/tool blocks,
    /// aligning content with the user's "› " prefix.
    const INDENT: u16 = 2;

    /// Calculate the height needed to render this block
    pub fn calculate_height(&self, width: u16) -> u16 {
        let inner_width = if width > Self::INDENT {
            width - Self::INDENT
        } else {
            width
        };
        match self {
            MessageBlock::PlainText(block) => {
                if block.content.trim().is_empty() {
                    return 0;
                }
                measure_markdown_height(&block.content, inner_width)
            }
            MessageBlock::Thinking(block) => {
                if block.content.trim().is_empty() {
                    return 0;
                }
                measure_markdown_height(&block.content, inner_width)
            }
            MessageBlock::UserText(block) => {
                if block.content.trim().is_empty() {
                    return 0;
                }
                // Empty line before + content lines + empty line after
                let content_lines = block.content.lines().count().max(1) as u16;
                2 + content_lines // 1 blank before + content + 1 blank after
            }
            MessageBlock::ToolUse(block) => {
                // Try a registered renderer first.
                if let Some(registry) = ToolRendererRegistry::global() {
                    if let Some(renderer) = registry.get(&block.name) {
                        return renderer.calculate_height(block, width);
                    }
                }

                // Fallback: generic height calculation
                let mut height: u16 = 1; // Tool name line

                for (name, param) in &block.parameters {
                    if should_hide_parameter(&block.name, name, &param.value) {
                        continue;
                    }
                    if is_full_width_parameter(&block.name, name) {
                        height += 1; // Parameter name
                        height += param.value.lines().count() as u16;
                    } else {
                        height += 1;
                    }
                }

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

fn measure_markdown_height(content: &str, width: u16) -> u16 {
    if content.trim().is_empty() || width == 0 {
        return 0;
    }

    let base_lines = content.lines().count().max(1) as u16;
    let rough_wrap = (content.chars().count() as u16 / width.max(1)).saturating_add(base_lines);
    let max_height = rough_wrap.saturating_add(16).clamp(16, 2048);

    let text = md::from_str(content);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let mut tmp = ratatui::buffer::Buffer::empty(Rect::new(0, 0, width, max_height));
    paragraph.render(Rect::new(0, 0, width, max_height), &mut tmp);

    for y in (0..max_height).rev() {
        let mut row_empty = true;
        for x in 0..width {
            let Some(cell) = tmp.cell((x, y)) else {
                continue;
            };
            if !cell.symbol().is_empty() && cell.symbol() != " " {
                row_empty = false;
                break;
            }
        }
        if !row_empty {
            return y + 1;
        }
    }

    0
}

impl Widget for MessageBlock {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let indent = if area.width > Self::INDENT {
            Self::INDENT
        } else {
            0
        };
        let inner = Rect {
            x: area.x + indent,
            y: area.y,
            width: area.width.saturating_sub(indent),
            height: area.height,
        };
        match self {
            MessageBlock::PlainText(block) => {
                if !block.content.trim().is_empty() {
                    let text = md::from_str(&block.content);
                    let paragraph = ratatui::widgets::Paragraph::new(text)
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    paragraph.render(inner, buf);
                }
            }
            MessageBlock::Thinking(block) => {
                if !block.content.trim().is_empty() {
                    let text = md::from_str(&block.content);
                    let paragraph = ratatui::widgets::Paragraph::new(text)
                        .style(
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM)
                                .add_modifier(Modifier::ITALIC),
                        )
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    paragraph.render(inner, buf);
                }
            }
            MessageBlock::UserText(block) => {
                if !block.content.trim().is_empty() {
                    let mut lines = Vec::new();
                    lines.push(Line::from(""));
                    for (i, line) in block.content.lines().enumerate() {
                        let prefix = if i == 0 {
                            Span::styled(
                                "› ",
                                Style::default()
                                    .add_modifier(Modifier::BOLD)
                                    .add_modifier(Modifier::DIM),
                            )
                        } else {
                            Span::raw("  ")
                        };
                        lines.push(Line::from(vec![prefix, Span::raw(line.to_string())]));
                    }
                    lines.push(Line::from(""));
                    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
                    paragraph.render(area, buf);
                }
            }
            MessageBlock::ToolUse(block) => {
                // ToolWidget renders its own "● name" layout starting at area.x,
                // so it uses the full area (dot at col 0, text at col 2).
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
    pub parameters: IndexMap<String, ParameterValue>,
    pub status: ToolStatus,
    pub status_message: Option<String>,
    pub output: Option<String>,
}

impl ToolUseBlock {
    pub fn new(name: String, id: String) -> Self {
        Self {
            name,
            id,
            parameters: IndexMap::new(),
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
