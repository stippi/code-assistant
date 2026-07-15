use code_assistant_core::ui::ToolStatus;
use ratatui::prelude::*;

use super::message::ToolUseBlock;
use super::tool_renderers::ToolRendererRegistry;

/// Custom ratatui widget for rendering tool use blocks.
///
/// Dispatches to registered `ToolRenderer` plugins when available,
/// falling back to the generic parameter-based rendering for tools
/// without a custom renderer (e.g. `spawn_agent`, `delete_files`).
pub struct ToolWidget<'a> {
    tool_block: &'a ToolUseBlock,
}

impl<'a> ToolWidget<'a> {
    pub fn new(tool_block: &'a ToolUseBlock) -> Self {
        Self { tool_block }
    }

    fn get_status_symbol(&self) -> &'static str {
        "●"
    }

    fn get_status_color(&self) -> Color {
        match self.tool_block.status {
            ToolStatus::Pending => Color::Yellow,
            ToolStatus::Running => Color::Blue,
            ToolStatus::Success => Color::Green,
            ToolStatus::Error => Color::Red,
        }
    }
}

impl<'a> Widget for ToolWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return;
        }

        // Try a registered renderer first.
        if let Some(registry) = ToolRendererRegistry::global() {
            if let Some(renderer) = registry.get(&self.tool_block.name) {
                renderer.render(self.tool_block, area, buf);
                return;
            }
        }

        // ── Fallback: generic rendering ──────────────────────────────────
        self.render_fallback(area, buf);
    }
}

impl<'a> ToolWidget<'a> {
    /// Generic fallback rendering for tools without a custom renderer.
    fn render_fallback(&self, area: Rect, buf: &mut Buffer) {
        let (regular_params, fullwidth_params): (Vec<_>, Vec<_>) = self
            .tool_block
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v))
            .partition(|(name, _)| !is_full_width_parameter(&self.tool_block.name, name));

        let status_color = self.get_status_color();
        let status_symbol = self.get_status_symbol();

        let mut current_y = area.y;

        // Header: status symbol + tool name
        buf.set_string(
            area.x,
            current_y,
            status_symbol,
            Style::default().fg(status_color),
        );
        buf.set_string(
            area.x + 2,
            current_y,
            &self.tool_block.name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
        current_y += 1;

        // Regular parameters
        for (name, param) in &regular_params {
            if current_y >= area.y + area.height {
                break;
            }
            if should_hide_parameter(&self.tool_block.name, name, &param.value) {
                continue;
            }

            buf.set_string(
                area.x + 2,
                current_y,
                name,
                Style::default().fg(Color::Cyan),
            );
            buf.set_string(
                area.x + 2 + name.len() as u16,
                current_y,
                ": ",
                Style::default().fg(Color::White),
            );
            buf.set_string(
                area.x + 2 + name.len() as u16 + 2,
                current_y,
                param.get_display_value(),
                Style::default().fg(Color::Gray),
            );
            current_y += 1;
        }

        // Full-width parameters
        for (name, param) in &fullwidth_params {
            if current_y >= area.y + area.height {
                break;
            }
            if should_hide_parameter(&self.tool_block.name, name, &param.value) {
                continue;
            }

            buf.set_string(
                area.x + 2,
                current_y,
                name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
            current_y += 1;

            for line in param.value.lines() {
                if current_y >= area.y + area.height {
                    break;
                }
                buf.set_string(
                    area.x + 4,
                    current_y,
                    line,
                    Style::default().fg(Color::White),
                );
                current_y += 1;
            }
        }

        // Error status message
        if let Some(ref message) = self.tool_block.status_message {
            if self.tool_block.status == ToolStatus::Error && current_y < area.y + area.height {
                let display_text = if message.len() > area.width as usize {
                    &message[..area.width as usize]
                } else {
                    message
                };
                buf.set_string(
                    area.x + 2,
                    current_y,
                    display_text,
                    Style::default().fg(Color::LightRed),
                );
                current_y += 1;
            }
        }

        // Generic tool output rendered verbatim. Tools with richer output
        // (e.g. spawn_agent's sub-agent activity) register their own
        // `ToolRenderer` and never reach this fallback.
        if let Some(ref output) = self.tool_block.output {
            if !output.is_empty() {
                for line in output.lines() {
                    if current_y >= area.y + area.height {
                        break;
                    }
                    let truncated = if line.len() > (area.width.saturating_sub(4)) as usize {
                        format!("{}...", &line[..(area.width.saturating_sub(7)) as usize])
                    } else {
                        line.to_string()
                    };
                    buf.set_string(
                        area.x + 2,
                        current_y,
                        &truncated,
                        Style::default().fg(Color::Gray),
                    );
                    current_y += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers used by the fallback path and by message.rs height calculation
// ---------------------------------------------------------------------------

/// Check if a parameter should be rendered full-width.
pub(super) fn is_full_width_parameter(tool_name: &str, param_name: &str) -> bool {
    match (tool_name, param_name) {
        (_, "content") if param_name != "message" => true,
        (_, "output") => true,
        (_, "query") => true,
        _ => false,
    }
}

/// Check if a parameter should be hidden from display.
pub(super) fn should_hide_parameter(tool_name: &str, param_name: &str, param_value: &str) -> bool {
    match (tool_name, param_name) {
        (_, "project") => param_value.is_empty() || param_value == "." || param_value == "unknown",
        _ => false,
    }
}
