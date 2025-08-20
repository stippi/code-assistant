use crate::ui::ToolStatus;
use ratatui::prelude::*;

use super::blocks::{ParameterValue, ToolUseBlock};

/// Custom ratatui widget for rendering tool use blocks
pub struct ToolWidget<'a> {
    tool_block: &'a ToolUseBlock,
}

impl<'a> ToolWidget<'a> {
    #[allow(dead_code)]
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

    /// Render regular parameters as compact inline elements
    #[allow(dead_code)]
    fn render_regular_params(&self, area: Rect, buf: &mut Buffer, params: &[(String, &ParameterValue)]) {
        if params.is_empty() {
            return;
        }

        let mut x = area.x;
        let y = area.y;

        for (i, (name, param)) in params.iter().enumerate() {
            if should_hide_parameter(&self.tool_block.name, name, &param.value) {
                continue;
            }

            // Format: "name: value"
            let param_text = format!("{}: {}", name, param.get_display_value());
            let param_len = param_text.len() as u16;

            // Check if we have space on current line
            if x + param_len > area.x + area.width {
                break; // No more space
            }

            // Render parameter name in bold
            let colon_pos = name.len();
            buf.set_string(x, y, name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
            buf.set_string(x + colon_pos as u16, y, ": ", Style::default().fg(Color::White));
            buf.set_string(x + colon_pos as u16 + 2, y, param.get_display_value(), Style::default().fg(Color::Gray));

            x += param_len + 2; // Add some spacing

            // Add separator if not last param
            if i < params.len() - 1 {
                buf.set_string(x, y, "│", Style::default().fg(Color::DarkGray));
                x += 2;
            }
        }
    }

    /// Render full-width parameters as separate blocks
    #[allow(dead_code)]
    fn render_fullwidth_params(&self, area: Rect, buf: &mut Buffer, params: &[(String, &ParameterValue)]) -> u16 {
        let mut current_y = area.y;

        for (name, param) in params {
            if should_hide_parameter(&self.tool_block.name, name, &param.value) {
                continue;
            }

            if current_y >= area.y + area.height {
                break; // No more space
            }

            // Render parameter name
            buf.set_string(area.x, current_y, name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
            current_y += 1;

            if current_y >= area.y + area.height {
                break;
            }

            // Render parameter value with special formatting
            let rendered_value = render_parameter_value(&self.tool_block.name, name, &param.value);
            let lines: Vec<&str> = rendered_value.lines().collect();

            for line in lines {
                if current_y >= area.y + area.height {
                    break;
                }

                let display_line = if line.len() > area.width as usize {
                    &line[..area.width as usize]
                } else {
                    line
                };

                buf.set_string(area.x, current_y, display_line, Style::default().fg(Color::White));
                current_y += 1;
            }

            current_y += 1; // Add spacing between parameters
        }

        current_y - area.y
    }
}

impl<'a> Widget for ToolWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return; // Not enough space
        }

        // Separate regular and full-width parameters
        let (regular_params, fullwidth_params): (Vec<_>, Vec<_>) = self.tool_block.parameters
            .iter()
            .map(|(k, v)| (k.clone(), v))
            .partition(|(name, _)| !is_full_width_parameter(&self.tool_block.name, name));

        let status_color = self.get_status_color();
        let status_symbol = self.get_status_symbol();

        let mut current_y = area.y;

        // First line: Status symbol + tool name
        buf.set_string(area.x, current_y, status_symbol, Style::default().fg(status_color));
        buf.set_string(area.x + 2, current_y, &self.tool_block.name,
                      Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
        current_y += 1;

        // Regular parameters on separate lines if we have any
        for (name, param) in &regular_params {
            if current_y >= area.y + area.height {
                break;
            }
            if should_hide_parameter(&self.tool_block.name, name, &param.value) {
                continue;
            }



            buf.set_string(area.x, current_y, name, Style::default().fg(Color::Cyan));
            buf.set_string(area.x + name.len() as u16, current_y, ": ", Style::default().fg(Color::White));
            buf.set_string(area.x + name.len() as u16 + 2, current_y, param.get_display_value(), Style::default().fg(Color::Gray));
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
            buf.set_string(area.x, current_y, name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
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

                buf.set_string(area.x + 2, current_y, display_line, Style::default().fg(Color::White));
                current_y += 1;
            }
        }

        // Error message if present
        if let Some(ref message) = self.tool_block.status_message {
            if self.tool_block.status == ToolStatus::Error && current_y < area.y + area.height {
                let error_text = format!("Error: {message}");
                let display_text = if error_text.len() > area.width as usize {
                    &error_text[..area.width as usize]
                } else {
                    &error_text
                };
                buf.set_string(area.x, current_y, display_text, Style::default().fg(Color::Red));
                current_y += 1;
            }
        }

        // Output if present and successful
        if let Some(ref output) = self.tool_block.output {
            if !output.is_empty() && self.tool_block.status == ToolStatus::Success && current_y < area.y + area.height {
                let output_text = format!("→ {}", output.lines().next().unwrap_or(""));
                let display_text = if output_text.len() > area.width as usize {
                    &output_text[..area.width as usize]
                } else {
                    &output_text
                };
                buf.set_string(area.x, current_y, display_text, Style::default().fg(Color::Green));
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
