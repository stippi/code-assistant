//! Data types for the various block kinds that appear inside a message.
//!
//! These are pure data structs with minimal logic (constructors, accessors,
//! formatting helpers). They are used by both `MessageContainer` (which mutates
//! them during streaming) and `BlockView` (which renders them).

use code_assistant_core::ui::ToolStatus;
use std::sync::Arc;

/// Regular text block
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub content: String,
}

/// Summary shown after context compaction
#[derive(Debug, Clone)]
pub struct CompactionSummaryBlock {
    pub summary: String,
    pub is_expanded: bool,
}

/// Thinking text block with collapsible content
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub content: String,
    pub is_collapsed: bool,
    pub is_completed: bool,
    pub start_time: std::time::Instant,
    pub end_time: std::time::Instant,
    /// Pre-computed duration in seconds from persisted ContentBlock timestamps.
    /// When set (e.g. after session restore), this takes precedence over Instant-based measurement.
    pub duration_seconds: Option<f64>,
    // OpenAI reasoning fields
    pub reasoning_summary_items: Vec<llm::ReasoningSummaryItem>,
    pub current_generating_title: Option<String>,
    pub current_generating_content: Option<String>,
}

/// Image block with media type and base64 data
#[derive(Debug, Clone)]
pub struct ImageBlock {
    pub media_type: String,
    /// Parsed image ready for rendering, if parsing was successful
    pub image: Option<Arc<gpui::Image>>,
}

/// Tool use block with name and parameters
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    pub name: String,
    pub id: String,
    pub parameters: Vec<ParameterBlock>,
    pub status: ToolStatus,
    pub status_message: Option<String>,
    pub output: Option<String>,
    /// Styled terminal output with ANSI color information preserved.
    /// Used by terminal card renderer for colored static output.
    pub styled_output: Option<Vec<terminal::StyledLine>>,
    pub state: super::ToolBlockState, // Only collapsed/expanded, no generating
    /// Execution duration in seconds, computed from persisted ContentBlock timestamps.
    /// Stable across session restores (unlike Instant-based measurement).
    pub duration_seconds: Option<f64>,
    /// Image data from tools that produce visual output (e.g. view_images).
    /// Stored as (media_type, base64_data) pairs; rendered when the tool block is expanded.
    pub images: Vec<(String, String)>,
}

/// Parameter for a tool
#[derive(Debug, Clone)]
pub struct ParameterBlock {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// ThinkingBlock implementation
// ---------------------------------------------------------------------------

impl ThinkingBlock {
    pub fn new(content: String) -> Self {
        let now = std::time::Instant::now();
        Self {
            content,
            is_collapsed: true,  // Default is collapsed
            is_completed: false, // Default is not completed
            start_time: now,
            end_time: now, // Initially same as start_time
            duration_seconds: None,
            reasoning_summary_items: Vec::new(),
            current_generating_title: None,
            current_generating_content: None,
        }
    }

    pub fn formatted_duration(&self) -> String {
        // Prefer pre-computed duration from persisted timestamps (survives session restore)
        let secs = if let Some(dur) = self.duration_seconds {
            dur as u64
        } else if self.is_completed {
            self.end_time.duration_since(self.start_time).as_secs()
        } else {
            self.start_time.elapsed().as_secs()
        };

        if secs < 60 {
            format!("{secs}s")
        } else {
            let minutes = secs / 60;
            let seconds = secs % 60;
            format!("{minutes}m{seconds}s")
        }
    }

    /// Start a new reasoning summary item, finalizing the previous one if present
    pub fn start_reasoning_summary_item(&mut self) {
        if let Some(content) = &self.current_generating_content {
            if !content.is_empty() {
                self.reasoning_summary_items
                    .push(llm::ReasoningSummaryItem::SummaryText {
                        text: content.clone(),
                    });
            }
        }

        self.current_generating_content = Some(String::new());
        self.current_generating_title = None;
    }

    /// Append delta text to the current reasoning summary item
    pub fn append_reasoning_summary_delta(&mut self, delta: String) {
        if self.current_generating_content.is_none() {
            self.current_generating_content = Some(String::new());
        }

        if let Some(content) = &mut self.current_generating_content {
            content.push_str(&delta);
            self.current_generating_title = Self::parse_title_from_content(content);
        }
    }

    /// Complete reasoning and finalize any remaining items
    pub fn complete_reasoning(&mut self) {
        // Finalize current item if any
        if let Some(content) = &self.current_generating_content {
            if !content.is_empty() {
                self.reasoning_summary_items
                    .push(llm::ReasoningSummaryItem::SummaryText {
                        text: content.clone(),
                    });
            }
        }

        // Clear current state
        self.current_generating_title = None;
        self.current_generating_content = None;

        // Ensure we have content to display - if we have reasoning items but no fallback content,
        // populate the fallback content with the joined reasoning content
        if !self.reasoning_summary_items.is_empty() && self.content.is_empty() {
            self.content = self
                .reasoning_summary_items
                .iter()
                .map(|item| match item {
                    llm::ReasoningSummaryItem::SummaryText { text } => text.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n\n");
        }
    }

    /// Get display title based on generating state
    pub fn get_display_title(&self, is_generating: bool) -> String {
        if is_generating {
            // While generating, show current summary title or "Thinking..."
            self.current_generating_title
                .as_deref()
                .unwrap_or("Thinking...")
                .to_string()
        } else {
            // When completed, show duration
            format!("Thought for {}", self.formatted_duration())
        }
    }

    /// Get expanded content based on generating state
    pub fn get_expanded_content(&self, is_generating: bool) -> String {
        let result = if is_generating {
            // While generating, show current item content
            let content = self
                .current_generating_content
                .as_deref()
                .unwrap_or(&self.content)
                .to_string();
            content
        } else if self.is_reasoning_block() {
            // When completed with reasoning, show all summary items as raw content
            let reasoning_content = self
                .reasoning_summary_items
                .iter()
                .map(|item| match item {
                    llm::ReasoningSummaryItem::SummaryText { text } => text.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            // Fallback: if reasoning_summary_items is empty but we had content,
            // there might have been a timing issue during completion
            if reasoning_content.is_empty() && !self.content.is_empty() {
                self.content.clone()
            } else {
                reasoning_content
            }
        } else {
            // Traditional thinking block
            self.content.clone()
        };

        result
    }

    /// Check if this is a reasoning block (has reasoning summary items)
    pub fn is_reasoning_block(&self) -> bool {
        !self.reasoning_summary_items.is_empty() || self.current_generating_content.is_some()
    }

    /// Parse title from reasoning content in OpenAI format "**title**\n\ncontent"
    fn parse_title_from_content(content: &str) -> Option<String> {
        // Look for markdown bold pattern: **title** followed by newlines
        if let Some(start) = content.find("**") {
            if let Some(end) = content[start + 2..].find("**") {
                let title_end = start + 2 + end;
                let title = content[start + 2..title_end].trim();
                if !title.is_empty() {
                    return Some(title.to_string());
                }
            }
        }

        // Fallback: use the first line or first few words
        let first_line = content.lines().next().unwrap_or(content);
        let words: Vec<&str> = first_line.split_whitespace().take(5).collect();
        if !words.is_empty() {
            Some(words.join(" "))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// BlockData enum
// ---------------------------------------------------------------------------

/// Different types of blocks that can appear in a message
#[derive(Debug, Clone)]
pub enum BlockData {
    TextBlock(TextBlock),
    ThinkingBlock(ThinkingBlock),
    ToolUse(ToolUseBlock),
    ImageBlock(ImageBlock),
    CompactionSummary(CompactionSummaryBlock),
}

impl BlockData {
    pub(super) fn as_text_mut(&mut self) -> Option<&mut TextBlock> {
        match self {
            BlockData::TextBlock(b) => Some(b),
            _ => None,
        }
    }

    pub(super) fn as_thinking_mut(&mut self) -> Option<&mut ThinkingBlock> {
        match self {
            BlockData::ThinkingBlock(b) => Some(b),
            _ => None,
        }
    }

    pub(super) fn as_tool(&self) -> Option<&ToolUseBlock> {
        match self {
            BlockData::ToolUse(b) => Some(b),
            _ => None,
        }
    }

    pub(super) fn as_tool_mut(&mut self) -> Option<&mut ToolUseBlock> {
        match self {
            BlockData::ToolUse(b) => Some(b),
            _ => None,
        }
    }

    pub(super) fn as_compaction_mut(&mut self) -> Option<&mut CompactionSummaryBlock> {
        match self {
            BlockData::CompactionSummary(b) => Some(b),
            _ => None,
        }
    }
}
