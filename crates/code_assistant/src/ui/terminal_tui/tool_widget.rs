use crate::ui::ToolStatus;
use ratatui::prelude::*;

use super::message::ToolUseBlock;

/// Custom ratatui widget for rendering tool use blocks
pub struct ToolWidget<'a> {
    tool_block: &'a ToolUseBlock,
}

impl<'a> ToolWidget<'a> {
    pub fn new(tool_block: &'a ToolUseBlock) -> Self {
        Self { tool_block }
    }

    /// Get status symbol for the tool
    fn get_status_symbol(&self) -> &'static str {
        match self.tool_block.status {
            ToolStatus::Pending => "●",
            ToolStatus::Running => "●",
            ToolStatus::Success => "●",
            ToolStatus::Error => "●",
        }
    }

    /// Get status color for the tool
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
            return; // Not enough space
        }

        // Separate regular and full-width parameters
        let (regular_params, fullwidth_params): (Vec<_>, Vec<_>) = self
            .tool_block
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v))
            .partition(|(name, _)| !is_full_width_parameter(&self.tool_block.name, name));

        let status_color = self.get_status_color();
        let status_symbol = self.get_status_symbol();

        let mut current_y = area.y;

        // First line: Status symbol + tool name
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

        // Regular parameters on separate lines if we have any
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

            // Parameter name
            buf.set_string(
                area.x + 2,
                current_y,
                name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
            current_y += 1;

            if current_y >= area.y + area.height {
                break;
            }

            // Parameter value with special formatting
            let rendered_value = render_parameter_value(&self.tool_block.name, name, &param.value);
            let lines: Vec<&str> = rendered_value.lines().take(3).collect(); // Limit to 3 lines for compactness

            for line in lines {
                if current_y >= area.y + area.height {
                    break;
                }

                let display_line = if line.len() > area.width as usize {
                    &line[..area.width as usize]
                } else {
                    line
                };

                buf.set_string(
                    area.x + 4,
                    current_y,
                    display_line,
                    Style::default().fg(Color::White),
                );
                current_y += 1;
            }
        }

        // Status message only for errors
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
            }
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

/// Render parameter value with special formatting for different types
fn render_parameter_value(tool_name: &str, param_name: &str, param_value: &str) -> String {
    match (tool_name, param_name) {
        // Diff parameters - show as diff with simple prefix
        ("replace_in_file", "diff") => {
            format!("--- OLD\n+++ NEW\n{param_value}")
        }
        ("edit", "old_text") => {
            format!("- {}", param_value.replace('\n', "\n- "))
        }
        ("edit", "new_text") => {
            format!("+ {}", param_value.replace('\n', "\n+ "))
        }
        // Regular full-width parameters
        _ => param_value.to_string(),
    }
}
