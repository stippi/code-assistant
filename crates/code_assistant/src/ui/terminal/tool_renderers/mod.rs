//! Plugin-based tool renderer system for the terminal UI.
//!
//! Each tool (or group of tools) can register a custom renderer that controls
//! how the tool block appears in both the live viewport and scrollback history.

pub mod command_renderer;
pub mod compact_renderer;
pub mod diff_renderer;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};

use super::message::ToolUseBlock;
use crate::ui::ToolStatus;

/// Trait for custom tool block renderers.
///
/// Implementations handle rendering for one or more tool names, covering
/// both the live viewport (ratatui Buffer) and scrollback history (Line items).
pub trait ToolRenderer: Send + Sync {
    /// Which tool names this renderer handles.
    fn supported_tools(&self) -> &'static [&'static str];

    /// Render the tool block into a ratatui Buffer (live viewport).
    fn render(&self, tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer);

    /// Calculate the height (in rows) needed for this tool block.
    fn calculate_height(&self, tool_block: &ToolUseBlock, width: u16) -> u16;

    /// Produce styled Lines for scrollback history.
    fn render_history_lines(&self, tool_block: &ToolUseBlock) -> Vec<Line<'static>>;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

static GLOBAL_REGISTRY: OnceLock<Arc<ToolRendererRegistry>> = OnceLock::new();

pub struct ToolRendererRegistry {
    renderers: HashMap<String, Arc<dyn ToolRenderer>>,
}

impl ToolRendererRegistry {
    pub fn new() -> Self {
        Self {
            renderers: HashMap::new(),
        }
    }

    /// Register a renderer for all tools it declares via `supported_tools()`.
    pub fn register(&mut self, renderer: Arc<dyn ToolRenderer>) {
        for &tool_name in renderer.supported_tools() {
            self.renderers
                .insert(tool_name.to_string(), renderer.clone());
        }
    }

    /// Look up a renderer by tool name.
    pub fn get(&self, tool_name: &str) -> Option<Arc<dyn ToolRenderer>> {
        self.renderers.get(tool_name).cloned()
    }

    /// Install the global singleton.
    pub fn set_global(registry: ToolRendererRegistry) {
        let _ = GLOBAL_REGISTRY.set(Arc::new(registry));
    }

    /// Retrieve the global singleton.
    pub fn global() -> Option<&'static Arc<ToolRendererRegistry>> {
        GLOBAL_REGISTRY.get()
    }
}

// ---------------------------------------------------------------------------
// Shared helpers used by multiple renderers
// ---------------------------------------------------------------------------

/// Return ` [project]` if a meaningful project parameter is present, else empty.
pub fn get_project_suffix(tool_block: &ToolUseBlock) -> String {
    if let Some(project_param) = tool_block.parameters.get("project") {
        let val = &project_param.value;
        if !val.is_empty() && val != "." && val != "unknown" {
            return format!(" [{}]", val);
        }
    }
    String::new()
}

/// Status symbol for a tool block.
pub fn status_symbol(_status: &ToolStatus) -> &'static str {
    "●"
}

/// Status color for a tool block.
pub fn status_color(status: &ToolStatus) -> Color {
    match status {
        ToolStatus::Pending => Color::Yellow,
        ToolStatus::Running => Color::Blue,
        ToolStatus::Success => Color::Green,
        ToolStatus::Error => Color::Red,
    }
}

/// Render the standard `● tool_name [project]` header line into a Buffer.
/// Returns the y position of the next row.
pub fn render_tool_header(tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer, y: u16) -> u16 {
    let color = status_color(&tool_block.status);
    let symbol = status_symbol(&tool_block.status);
    let project = get_project_suffix(tool_block);

    buf.set_string(area.x, y, symbol, Style::default().fg(color));
    buf.set_string(
        area.x + 2,
        y,
        &tool_block.name,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    if !project.is_empty() {
        buf.set_string(
            area.x + 2 + tool_block.name.len() as u16,
            y,
            &project,
            Style::default().fg(Color::DarkGray),
        );
    }
    y + 1
}

/// Produce a styled `● tool_name [project]` Line for scrollback history.
pub fn tool_header_line(tool_block: &ToolUseBlock) -> Line<'static> {
    let color = status_color(&tool_block.status);
    let project = get_project_suffix(tool_block);

    let mut spans = vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::styled(
            tool_block.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !project.is_empty() {
        spans.push(Span::styled(project, Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

/// Render an error status message (if any) into a Buffer. Returns the next y.
pub fn render_error_line(tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer, y: u16) -> u16 {
    if tool_block.status == ToolStatus::Error {
        if let Some(ref message) = tool_block.status_message {
            if y < area.y + area.height {
                let max_len = area.width.saturating_sub(2) as usize;
                let display = if message.len() > max_len {
                    &message[..max_len]
                } else {
                    message.as_str()
                };
                buf.set_string(area.x + 2, y, display, Style::default().fg(Color::LightRed));
                return y + 1;
            }
        }
    }
    y
}

/// Push an error status message Line for scrollback history, if applicable.
pub fn push_error_history_line(tool_block: &ToolUseBlock, lines: &mut Vec<Line<'static>>) {
    if tool_block.status == ToolStatus::Error {
        if let Some(ref message) = tool_block.status_message {
            lines.push(Line::styled(
                format!("  {message}"),
                Style::default().fg(Color::LightRed),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Create and install the global tool renderer registry with all built-in renderers.
pub fn init_registry() {
    let mut registry = ToolRendererRegistry::new();
    registry.register(Arc::new(compact_renderer::CompactToolRenderer));
    registry.register(Arc::new(diff_renderer::DiffToolRenderer));
    registry.register(Arc::new(command_renderer::CommandToolRenderer));
    ToolRendererRegistry::set_global(registry);
}
