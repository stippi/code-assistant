use std::collections::HashMap;

/// Different types of live blocks that can be streamed
#[derive(Debug, Clone)]
pub enum LiveBlockType {
    PlainText(PlainTextBlock),
    Thinking(ThinkingBlock),
    ToolUse(ToolUseBlock),
}

impl LiveBlockType {
    /// Get the markdown content for rendering (only for PlainText and Thinking)
    pub fn get_markdown_content(&self) -> Option<String> {
        match self {
            LiveBlockType::PlainText(block) => Some(block.content.clone()),
            LiveBlockType::Thinking(block) => Some(format!("*{}*", block.content)),
            LiveBlockType::ToolUse(_) => None, // ToolUse blocks use custom widget
        }
    }

    /// Check if this is a tool use block
    pub fn is_tool_use(&self) -> bool {
        matches!(self, LiveBlockType::ToolUse(_))
    }

    /// Get reference to tool use block
    pub fn as_tool_use(&self) -> Option<&ToolUseBlock> {
        match self {
            LiveBlockType::ToolUse(block) => Some(block),
            _ => None,
        }
    }

    /// Append content to the block
    pub fn append_content(&mut self, content: &str) {
        match self {
            LiveBlockType::PlainText(block) => block.content.push_str(content),
            LiveBlockType::Thinking(block) => block.content.push_str(content),
            LiveBlockType::ToolUse(_block) => {
                // For tool use, we don't append to general content
                // Parameter updates are handled separately
            }
        }
    }

    /// Check if this is a tool use block with matching ID
    pub fn is_tool_with_id(&self, tool_id: &str) -> bool {
        match self {
            LiveBlockType::ToolUse(block) => block.id == tool_id,
            _ => false,
        }
    }

    /// Get mutable reference to tool use block if it matches the ID
    pub fn get_tool_mut(&mut self, tool_id: &str) -> Option<&mut ToolUseBlock> {
        match self {
            LiveBlockType::ToolUse(block) if block.id == tool_id => Some(block),
            _ => None,
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
    pub status: crate::ui::ToolStatus,
    pub status_message: Option<String>,
    pub output: Option<String>,
}

impl ToolUseBlock {
    pub fn new(name: String, id: String) -> Self {
        Self {
            name,
            status: crate::ui::ToolStatus::Pending, // Start with Pending (gray - streaming)
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
