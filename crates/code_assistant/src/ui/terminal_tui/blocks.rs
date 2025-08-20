use std::collections::HashMap;

/// Different types of live blocks that can be streamed
#[derive(Debug, Clone)]
pub enum LiveBlockType {
    PlainText(PlainTextBlock),
    Thinking(ThinkingBlock),
    ToolUse(ToolUseBlock),
}

impl LiveBlockType {
    /// Get the markdown content for rendering
    pub fn get_markdown_content(&self) -> String {
        match self {
            LiveBlockType::PlainText(block) => block.content.clone(),
            LiveBlockType::Thinking(block) => {
                format!("**🧠 Thinking...**\n\n*{}*", block.content)
            }
            LiveBlockType::ToolUse(block) => block.render_as_markdown(),
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
            id,
            parameters: HashMap::new(),
            status: crate::ui::ToolStatus::Pending,
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

    /// Render the tool use block as markdown
    pub fn render_as_markdown(&self) -> String {
        let mut content = String::new();

        // Tool header with status
        let status_emoji = match self.status {
            crate::ui::ToolStatus::Pending => "🔄",
            crate::ui::ToolStatus::Running => "⚙️",
            crate::ui::ToolStatus::Success => "✅",
            crate::ui::ToolStatus::Error => "❌",
        };

        content.push_str(&format!("\n{} **{}**\n\n", status_emoji, self.name));

        // Render parameters
        if !self.parameters.is_empty() {
            // Separate regular and full-width parameters
            let (regular_params, fullwidth_params): (Vec<_>, Vec<_>) = self.parameters
                .iter()
                .partition(|(name, _)| !is_full_width_parameter(&self.name, name));

            // Regular parameters in a compact format
            if !regular_params.is_empty() {
                for (name, param) in regular_params {
                    if should_hide_parameter(&self.name, name, &param.value) {
                        continue;
                    }
                    content.push_str(&format!("- **{}:** `{}`\n", name, param.get_display_value()));
                }
                content.push('\n');
            }

            // Full-width parameters with special rendering
            for (name, param) in fullwidth_params {
                if should_hide_parameter(&self.name, name, &param.value) {
                    continue;
                }
                content.push_str(&render_parameter_markdown(&self.name, name, &param.value));
                content.push('\n');
            }
        }

        // Status message for errors
        if let Some(ref message) = self.status_message {
            if self.status == crate::ui::ToolStatus::Error {
                content.push_str(&format!("**Error:** {}\n\n", message));
            }
        }

        // Output
        if let Some(ref output) = self.output {
            if !output.is_empty() {
                content.push_str("**Output:**\n```\n");
                content.push_str(output);
                content.push_str("\n```\n\n");
            }
        }

        content
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
        ("read_files", _) => true, // All read_files parameters are full-width
        // Large text parameters
        (_, "content") => true,
        (_, "output") => true,
        (_, "query") => true,
        _ => false,
    }
}

/// Check if a parameter should be hidden (e.g., project parameter matching current project)
fn should_hide_parameter(tool_name: &str, param_name: &str, param_value: &str) -> bool {
    // For now, simple logic - could be expanded later
    match (tool_name, param_name) {
        (_, "project") => {
            // Hide project parameter if it's empty or matches a common default
            param_value.is_empty() || param_value == "." || param_value == "unknown"
        }
        _ => false,
    }
}

/// Render a parameter as markdown with special formatting
fn render_parameter_markdown(tool_name: &str, param_name: &str, param_value: &str) -> String {
    match (tool_name, param_name) {
        // Diff parameters get special rendering
        ("replace_in_file", "diff") => {
            format!("**Diff:**\n```diff\n{}\n```\n", param_value)
        }
        ("edit", "old_text") => {
            format!("**Old text:**\n```\n{}\n```\n", param_value)
        }
        ("edit", "new_text") => {
            format!("**New text:**\n```\n{}\n```\n", param_value)
        }
        // File content
        ("write_file", "content") => {
            format!("**Content:**\n```\n{}\n```\n", param_value)
        }
        // Default full-width rendering
        _ => {
            format!("**{}:**\n```\n{}\n```\n", param_name, param_value)
        }
    }
}
