//! Renderer for the execute_command tool.
//!
//! Displays the command line and streaming terminal output on a tinted
//! background so it stands out from surrounding assistant text.

use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};

use super::{
    push_error_history_line, render_error_line, render_tool_header, tool_header_line, ToolRenderer,
};
use crate::ui::terminal::message::ToolUseBlock;
use crate::ui::terminal::terminal_color;
use crate::ui::ToolStatus;

/// Expand tab characters to spaces (4-space tab stops).
fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut col = 0;
    for ch in text.chars() {
        if ch == '\t' {
            let spaces = 4 - (col % 4);
            for _ in 0..spaces {
                result.push(' ');
            }
            col += spaces;
        } else {
            result.push(ch);
            col += 1;
        }
    }
    result
}

/// Renderer for the `execute_command` tool.
pub struct CommandToolRenderer;

impl ToolRenderer for CommandToolRenderer {
    fn supported_tools(&self) -> &'static [&'static str] {
        &["execute_command"]
    }

    fn render(&self, tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return;
        }

        let mut y = render_tool_header(tool_block, area, buf, area.y);

        // Command line
        if let Some(cmd) = tool_block.parameters.get("command_line") {
            if y < area.y + area.height {
                let bg = terminal_color::tool_content_bg();
                let row_width = area.width.saturating_sub(2) as usize;
                buf.set_string(
                    area.x + 2,
                    y,
                    " ".repeat(row_width),
                    Style::default().bg(bg),
                );
                buf.set_string(
                    area.x + 2,
                    y,
                    "$ ",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                        .bg(bg),
                );
                let max_cmd_len = row_width.saturating_sub(2);
                let display = if cmd.value.len() > max_cmd_len {
                    &cmd.value[..max_cmd_len]
                } else {
                    cmd.value.as_str()
                };
                buf.set_string(
                    area.x + 4,
                    y,
                    display,
                    Style::default().fg(Color::White).bg(bg),
                );
                y += 1;
            }
        }

        // Terminal output
        if let Some(ref output) = tool_block.output {
            if !output.is_empty() {
                let bg = terminal_color::tool_content_bg();
                let row_width = area.width.saturating_sub(2) as usize;
                for line in output.lines() {
                    if y >= area.y + area.height {
                        break;
                    }
                    // Fill background across full row width
                    buf.set_string(
                        area.x + 2,
                        y,
                        " ".repeat(row_width),
                        Style::default().bg(bg),
                    );
                    let expanded = expand_tabs(line);
                    let display = if expanded.len() > row_width {
                        &expanded[..row_width]
                    } else {
                        expanded.as_str()
                    };
                    buf.set_string(
                        area.x + 2,
                        y,
                        display,
                        Style::default().fg(Color::Gray).bg(bg),
                    );
                    y += 1;
                }
            }
        }

        render_error_line(tool_block, area, buf, y);
    }

    fn calculate_height(&self, tool_block: &ToolUseBlock, _width: u16) -> u16 {
        let mut height: u16 = 1; // header

        // Command line
        if tool_block.parameters.contains_key("command_line") {
            height += 1;
        }

        // Terminal output
        if let Some(ref output) = tool_block.output {
            if !output.is_empty() {
                height += output.lines().count() as u16;
            }
        }

        if tool_block.status == ToolStatus::Error && tool_block.status_message.is_some() {
            height += 1;
        }
        height
    }

    fn render_history_lines(&self, tool_block: &ToolUseBlock) -> Vec<Line<'static>> {
        let mut lines = vec![tool_header_line(tool_block)];
        let bg = terminal_color::tool_content_bg();
        let bg_style = Style::default().bg(bg);

        // Command line
        if let Some(cmd) = tool_block.parameters.get("command_line") {
            lines.push(
                Line::from(vec![
                    Span::styled(
                        "  $ ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD)
                            .bg(bg),
                    ),
                    Span::styled(cmd.value.clone(), Style::default().fg(Color::White).bg(bg)),
                ])
                .style(bg_style),
            );
        }

        // Terminal output
        if let Some(ref output) = tool_block.output {
            for line in output.lines() {
                lines.push(
                    Line::from(vec![Span::styled(
                        format!("  {}", expand_tabs(line)),
                        Style::default().fg(Color::Gray).bg(bg),
                    )])
                    .style(bg_style),
                );
            }
        }

        push_error_history_line(tool_block, &mut lines);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::terminal::message::ParameterValue;
    use indexmap::IndexMap;

    fn make_tool(params: &[(&str, &str)], output: Option<&str>) -> ToolUseBlock {
        let mut parameters = IndexMap::new();
        for (k, v) in params {
            parameters.insert(k.to_string(), ParameterValue::new(v.to_string()));
        }
        ToolUseBlock {
            name: "execute_command".to_string(),
            id: "test-id".to_string(),
            parameters,
            status: ToolStatus::Success,
            status_message: None,
            output: output.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_height_no_output() {
        let renderer = CommandToolRenderer;
        let tool = make_tool(&[("command_line", "echo hello")], None);
        // 1 header + 1 command = 2
        assert_eq!(renderer.calculate_height(&tool, 80), 2);
    }

    #[test]
    fn test_height_with_output() {
        let renderer = CommandToolRenderer;
        let tool = make_tool(
            &[("command_line", "ls")],
            Some("file1.rs\nfile2.rs\nfile3.rs"),
        );
        // 1 header + 1 command + 3 output lines = 5
        assert_eq!(renderer.calculate_height(&tool, 80), 5);
    }

    #[test]
    fn test_height_with_error() {
        let renderer = CommandToolRenderer;
        let mut tool = make_tool(&[("command_line", "false")], None);
        tool.status = ToolStatus::Error;
        tool.status_message = Some("Exit code 1".to_string());
        // 1 header + 1 command + 1 error = 3
        assert_eq!(renderer.calculate_height(&tool, 80), 3);
    }
}
