use crate::ui::ToolStatus;
use gpui::{div, px, Element, ParentElement, SharedString, Styled};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::warn;

/// A unique key for tool name
pub type ToolKey = String;

/// Trait for tool output renderers that can provide custom rendering for tool output
pub trait ToolOutputRenderer: Send + Sync {
    /// List of tool names this renderer supports
    fn supported_tools(&self) -> Vec<String>;

    /// Render the tool output as a UI element
    /// Returns None if the default rendering should be used
    fn render(
        &self,
        tool_name: &str,
        output: &str,
        status: &ToolStatus,
        theme: &gpui_component::theme::Theme,
    ) -> Option<gpui::AnyElement>;
}

/// Registry for tool output renderers
pub struct ToolOutputRendererRegistry {
    renderers: HashMap<ToolKey, Arc<Box<dyn ToolOutputRenderer>>>,
}

// Global registry singleton using OnceLock (thread-safe)
static GLOBAL_REGISTRY: OnceLock<Mutex<Option<Arc<ToolOutputRendererRegistry>>>> = OnceLock::new();

impl ToolOutputRendererRegistry {
    /// Set the global registry
    pub fn set_global(registry: Arc<ToolOutputRendererRegistry>) {
        let global_mutex = GLOBAL_REGISTRY.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = global_mutex.lock() {
            *guard = Some(registry);
        } else {
            warn!("Failed to acquire lock for setting global tool output registry");
        }
    }

    /// Get a reference to the global registry
    pub fn global() -> Option<Arc<ToolOutputRendererRegistry>> {
        if let Some(global_mutex) = GLOBAL_REGISTRY.get() {
            if let Ok(guard) = global_mutex.lock() {
                return guard.clone();
            }
        }
        None
    }

    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            renderers: HashMap::new(),
        }
    }

    /// Register a new renderer for its supported tools
    pub fn register_renderer(&mut self, renderer: Box<dyn ToolOutputRenderer>) {
        let renderer_arc = Arc::new(renderer);
        for tool_name in renderer_arc.supported_tools() {
            if self.renderers.contains_key(&tool_name) {
                warn!(
                    "Overriding existing output renderer for tool: {}",
                    tool_name
                );
            }
            self.renderers.insert(tool_name, renderer_arc.clone());
        }
    }

    /// Check if a custom renderer exists for a tool
    pub fn has_renderer(&self, tool_name: &str) -> bool {
        self.renderers.contains_key(tool_name)
    }

    /// Render tool output using the appropriate renderer
    /// Returns None if no custom renderer is registered (use default rendering)
    pub fn render_output(
        &self,
        tool_name: &str,
        output: &str,
        status: &ToolStatus,
        theme: &gpui_component::theme::Theme,
    ) -> Option<gpui::AnyElement> {
        self.renderers
            .get(tool_name)
            .and_then(|renderer| renderer.render(tool_name, output, status, theme))
    }
}

impl Default for ToolOutputRendererRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed sub-agent activity line
#[derive(Debug, Clone)]
pub enum SubAgentActivityLine {
    /// Tool call: "Calling tool `tool_name`"
    ToolCall { tool_name: String },
    /// Tool status: "Tool status: Success" or "Tool status: Running — message"
    ToolStatus {
        status: SubAgentToolStatus,
        message: Option<String>,
    },
    /// LLM streaming cancelled
    Cancelled,
    /// LLM error
    Error { message: String },
    /// Unknown/unparsed line
    Other { text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubAgentToolStatus {
    Pending,
    Running,
    Success,
    Error,
}

impl SubAgentActivityLine {
    /// Parse a single line from sub-agent output
    fn parse(line: &str) -> Self {
        let line = line.trim();

        // Remove leading "- " if present
        let line = line.strip_prefix("- ").unwrap_or(line);

        // Check for tool call
        if let Some(rest) = line.strip_prefix("Calling tool `") {
            if let Some(tool_name) = rest.strip_suffix('`') {
                return Self::ToolCall {
                    tool_name: tool_name.to_string(),
                };
            }
        }

        // Check for tool status
        if let Some(rest) = line.strip_prefix("Tool status: ") {
            // Format: "Status" or "Status — message"
            let (status_str, message) = if let Some(idx) = rest.find(" — ") {
                (&rest[..idx], Some(rest[idx + " — ".len()..].to_string()))
            } else {
                (rest, None)
            };

            let status = match status_str {
                "Pending" => SubAgentToolStatus::Pending,
                "Running" => SubAgentToolStatus::Running,
                "Success" => SubAgentToolStatus::Success,
                "Error" => SubAgentToolStatus::Error,
                _ => SubAgentToolStatus::Running, // Default to running for unknown
            };

            return Self::ToolStatus { status, message };
        }

        // Check for LLM events
        if line == "LLM streaming cancelled" {
            return Self::Cancelled;
        }

        if let Some(rest) = line.strip_prefix("LLM error: ") {
            return Self::Error {
                message: rest.to_string(),
            };
        }

        // Fallback
        Self::Other {
            text: line.to_string(),
        }
    }
}

/// Parsed sub-agent output with activity lines
#[derive(Debug, Clone)]
pub struct ParsedSubAgentOutput {
    pub activities: Vec<SubAgentActivityLine>,
}

impl ParsedSubAgentOutput {
    /// Parse the full sub-agent output
    pub fn parse(output: &str) -> Self {
        let mut activities = Vec::new();

        for line in output.lines() {
            let line = line.trim();

            // Skip the header
            if line == "### Sub-agent activity" || line.is_empty() {
                continue;
            }

            activities.push(SubAgentActivityLine::parse(line));
        }

        Self { activities }
    }

    /// Get the most recent tool call that is still "active" (last call without a success/error)
    pub fn get_active_tool(&self) -> Option<&str> {
        let mut last_tool: Option<&str> = None;

        for activity in &self.activities {
            match activity {
                SubAgentActivityLine::ToolCall { tool_name } => {
                    last_tool = Some(tool_name.as_str());
                }
                SubAgentActivityLine::ToolStatus { status, .. }
                    if *status == SubAgentToolStatus::Success
                        || *status == SubAgentToolStatus::Error =>
                {
                    last_tool = None; // Tool completed
                }
                _ => {}
            }
        }

        last_tool
    }

    /// Get a summary of completed tools for compact display
    pub fn get_tool_summary(&self) -> Vec<CompactToolInfo> {
        let mut tools: Vec<CompactToolInfo> = Vec::new();
        let mut current_tool: Option<String> = None;

        for activity in &self.activities {
            match activity {
                SubAgentActivityLine::ToolCall { tool_name } => {
                    current_tool = Some(tool_name.clone());
                }
                SubAgentActivityLine::ToolStatus { status, message } => {
                    if let Some(tool_name) = current_tool.take() {
                        let tool_status = match status {
                            SubAgentToolStatus::Success => CompactToolStatus::Success,
                            SubAgentToolStatus::Error => CompactToolStatus::Error,
                            SubAgentToolStatus::Running => CompactToolStatus::Running,
                            SubAgentToolStatus::Pending => CompactToolStatus::Pending,
                        };
                        tools.push(CompactToolInfo {
                            name: tool_name,
                            status: tool_status,
                            message: message.clone(),
                        });
                    }
                }
                _ => {}
            }
        }

        // If there's still a current tool without status, it's running
        if let Some(tool_name) = current_tool {
            tools.push(CompactToolInfo {
                name: tool_name,
                status: CompactToolStatus::Running,
                message: None,
            });
        }

        tools
    }
}

#[derive(Debug, Clone)]
pub struct CompactToolInfo {
    pub name: String,
    pub status: CompactToolStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompactToolStatus {
    Pending,
    Running,
    Success,
    Error,
}

/// Renderer for spawn_agent tool output
/// Displays sub-agent tool calls in a compact, Zed-like style
pub struct SpawnAgentOutputRenderer;

impl SpawnAgentOutputRenderer {
    /// Get the icon for a tool name (matching file_icons.rs logic)
    fn get_tool_icon(tool_name: &str) -> Option<SharedString> {
        use super::file_icons;
        file_icons::get().get_tool_icon(tool_name)
    }

    /// Get a display title for a tool (like Zed's ACP mode)
    fn get_tool_title(tool_name: &str) -> String {
        match tool_name {
            "read_files" => "Reading".to_string(),
            "search_files" => "Searching".to_string(),
            "list_files" => "Listing".to_string(),
            "glob_files" => "Finding files".to_string(),
            "write_file" => "Writing".to_string(),
            "edit" => "Editing".to_string(),
            "replace_in_file" => "Replacing".to_string(),
            "delete_files" => "Deleting".to_string(),
            "execute_command" => "Executing".to_string(),
            "web_fetch" => "Fetching".to_string(),
            "web_search" => "Searching web".to_string(),
            "perplexity_ask" => "Asking Perplexity".to_string(),
            _ => tool_name.replace('_', " "),
        }
    }

    /// Render a single compact tool line
    fn render_tool_line(
        tool: &CompactToolInfo,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        use super::file_icons;

        let icon = Self::get_tool_icon(&tool.name);
        let title = Self::get_tool_title(&tool.name);

        // Status-based colors
        let (icon_color, text_color) = match tool.status {
            CompactToolStatus::Running => (theme.info, theme.muted_foreground),
            CompactToolStatus::Success => (theme.success, theme.muted_foreground),
            CompactToolStatus::Error => (theme.danger, theme.danger),
            CompactToolStatus::Pending => (theme.muted_foreground, theme.muted_foreground),
        };

        // Build the display text
        let display_text = if let Some(msg) = &tool.message {
            if msg.is_empty() {
                title
            } else {
                format!("{} {}", title, msg)
            }
        } else {
            title
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .py(px(2.))
            .children(vec![
                // Icon
                file_icons::render_icon_container(&icon, 14.0, icon_color, "🔧").into_any(),
                // Title text
                div()
                    .text_size(px(13.))
                    .text_color(text_color)
                    .child(display_text)
                    .into_any(),
            ])
            .into_any()
    }
}

impl ToolOutputRenderer for SpawnAgentOutputRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["spawn_agent".to_string()]
    }

    fn render(
        &self,
        _tool_name: &str,
        output: &str,
        _status: &ToolStatus,
        theme: &gpui_component::theme::Theme,
    ) -> Option<gpui::AnyElement> {
        if output.is_empty() {
            return None;
        }

        let parsed = ParsedSubAgentOutput::parse(output);
        let tools = parsed.get_tool_summary();

        if tools.is_empty() {
            return None;
        }

        // Render compact list of tool calls
        let tool_elements: Vec<gpui::AnyElement> = tools
            .iter()
            .map(|tool| Self::render_tool_line(tool, theme))
            .collect();

        Some(
            div()
                .flex()
                .flex_col()
                .gap_0()
                .mt_1()
                .children(tool_elements)
                .into_any(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sub_agent_output() {
        let output = r#"### Sub-agent activity

- Calling tool `read_files`
- Tool status: Success
- Calling tool `search_files`
- Tool status: Running — Searching for pattern
"#;

        let parsed = ParsedSubAgentOutput::parse(output);
        assert_eq!(parsed.activities.len(), 4);

        // First activity: tool call
        match &parsed.activities[0] {
            SubAgentActivityLine::ToolCall { tool_name } => {
                assert_eq!(tool_name, "read_files");
            }
            _ => panic!("Expected ToolCall"),
        }

        // Second activity: status
        match &parsed.activities[1] {
            SubAgentActivityLine::ToolStatus { status, message } => {
                assert_eq!(*status, SubAgentToolStatus::Success);
                assert!(message.is_none());
            }
            _ => panic!("Expected ToolStatus"),
        }

        // Fourth activity: status with message
        match &parsed.activities[3] {
            SubAgentActivityLine::ToolStatus { status, message } => {
                assert_eq!(*status, SubAgentToolStatus::Running);
                assert_eq!(message.as_deref(), Some("Searching for pattern"));
            }
            _ => panic!("Expected ToolStatus"),
        }
    }

    #[test]
    fn test_get_tool_summary() {
        let output = r#"### Sub-agent activity

- Calling tool `read_files`
- Tool status: Success
- Calling tool `search_files`
"#;

        let parsed = ParsedSubAgentOutput::parse(output);
        let tools = parsed.get_tool_summary();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "read_files");
        assert_eq!(tools[0].status, CompactToolStatus::Success);
        assert_eq!(tools[1].name, "search_files");
        assert_eq!(tools[1].status, CompactToolStatus::Running);
    }

    #[test]
    fn test_get_active_tool() {
        let output = r#"### Sub-agent activity

- Calling tool `read_files`
- Tool status: Success
- Calling tool `search_files`
"#;

        let parsed = ParsedSubAgentOutput::parse(output);
        assert_eq!(parsed.get_active_tool(), Some("search_files"));
    }

    #[test]
    fn test_parse_error_and_cancel() {
        let output = r#"### Sub-agent activity

- Calling tool `read_files`
- LLM streaming cancelled
- LLM error: Connection failed
"#;

        let parsed = ParsedSubAgentOutput::parse(output);
        assert_eq!(parsed.activities.len(), 3);

        match &parsed.activities[1] {
            SubAgentActivityLine::Cancelled => {}
            _ => panic!("Expected Cancelled"),
        }

        match &parsed.activities[2] {
            SubAgentActivityLine::Error { message } => {
                assert_eq!(message, "Connection failed");
            }
            _ => panic!("Expected Error"),
        }
    }
}
