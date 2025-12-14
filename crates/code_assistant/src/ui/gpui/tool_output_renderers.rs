use crate::agent::sub_agent::{SubAgentOutput, SubAgentToolStatus};
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
    #[allow(dead_code)]
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

    /// Render a single compact tool line from structured data
    fn render_tool_line(
        tool: &crate::agent::sub_agent::SubAgentToolCall,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        use super::file_icons;

        let icon = Self::get_tool_icon(&tool.name);
        let title = Self::get_tool_title(&tool.name);

        // Status-based colors
        let (icon_color, text_color) = match tool.status {
            SubAgentToolStatus::Running => (theme.info, theme.muted_foreground),
            SubAgentToolStatus::Success => (theme.success, theme.muted_foreground),
            SubAgentToolStatus::Error => (theme.danger, theme.danger),
        };

        // Build the display text
        let display_text = if let Some(msg) = &tool.message {
            if msg.is_empty() {
                title
            } else {
                format!("{title} — {msg}")
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

    /// Render error/cancelled status if present
    fn render_status_line(
        output: &SubAgentOutput,
        theme: &gpui_component::theme::Theme,
    ) -> Option<gpui::AnyElement> {
        if output.cancelled == Some(true) {
            return Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.warning)
                            .child("Sub-agent cancelled"),
                    )
                    .into_any(),
            );
        }

        if let Some(error) = &output.error {
            return Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.danger)
                            .child(format!("Error: {error}")),
                    )
                    .into_any(),
            );
        }

        None
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

        // Parse JSON output
        let parsed = match SubAgentOutput::from_json(output) {
            Some(p) => p,
            None => {
                // If not valid JSON, return None to use default text rendering
                // This handles backwards compatibility with any old markdown format
                return None;
            }
        };

        if parsed.tools.is_empty() && parsed.cancelled.is_none() && parsed.error.is_none() {
            return None;
        }

        // Render compact list of tool calls
        let mut elements: Vec<gpui::AnyElement> = parsed
            .tools
            .iter()
            .map(|tool| Self::render_tool_line(tool, theme))
            .collect();

        // Add status line if present
        if let Some(status_line) = Self::render_status_line(&parsed, theme) {
            elements.push(status_line);
        }

        Some(
            div()
                .flex()
                .flex_col()
                .gap_0()
                .mt_1()
                .children(elements)
                .into_any(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_output() {
        let json = r#"{"tools":[{"name":"read_files","status":"success"},{"name":"search_files","status":"running","message":"Searching..."}]}"#;

        let parsed = SubAgentOutput::from_json(json).unwrap();
        assert_eq!(parsed.tools.len(), 2);
        assert_eq!(parsed.tools[0].name, "read_files");
        assert_eq!(parsed.tools[0].status, SubAgentToolStatus::Success);
        assert_eq!(parsed.tools[1].name, "search_files");
        assert_eq!(parsed.tools[1].status, SubAgentToolStatus::Running);
        assert_eq!(parsed.tools[1].message.as_deref(), Some("Searching..."));
    }

    #[test]
    fn test_parse_json_with_cancelled() {
        let json = r#"{"tools":[{"name":"read_files","status":"success"}],"cancelled":true}"#;

        let parsed = SubAgentOutput::from_json(json).unwrap();
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.cancelled, Some(true));
    }

    #[test]
    fn test_parse_json_with_error() {
        let json = r#"{"tools":[],"error":"Connection failed"}"#;

        let parsed = SubAgentOutput::from_json(json).unwrap();
        assert_eq!(parsed.error.as_deref(), Some("Connection failed"));
    }

    #[test]
    fn test_roundtrip() {
        let mut output = SubAgentOutput::new();
        output
            .tools
            .push(crate::agent::sub_agent::SubAgentToolCall {
                name: "read_files".to_string(),
                status: SubAgentToolStatus::Success,
                message: None,
            });
        output
            .tools
            .push(crate::agent::sub_agent::SubAgentToolCall {
                name: "search_files".to_string(),
                status: SubAgentToolStatus::Running,
                message: Some("Searching for pattern".to_string()),
            });

        let json = output.to_json();
        let parsed = SubAgentOutput::from_json(&json).unwrap();

        assert_eq!(parsed.tools.len(), 2);
        assert_eq!(parsed.tools[0].name, "read_files");
        assert_eq!(
            parsed.tools[1].message.as_deref(),
            Some("Searching for pattern")
        );
    }

    #[test]
    fn test_invalid_json_returns_none() {
        let invalid = "### Sub-agent activity\n- Calling tool read_files";
        assert!(SubAgentOutput::from_json(invalid).is_none());
    }
}
