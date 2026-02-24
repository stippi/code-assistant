//! Diff renderer for write tools (edit, replace_in_file, write_file).
//!
//! Shows the file path and a coloured diff with line numbers, inspired by the
//! codex CLI diff rendering.

use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};
use similar::{ChangeTag, TextDiff};

use super::{
    push_error_history_line, render_error_line, render_tool_header, tool_header_line, ToolRenderer,
};
use crate::ui::terminal::message::ToolUseBlock;
use crate::ui::terminal::terminal_color;
use crate::ui::ToolStatus;

/// Renderer for write/edit tools: edit, write_file, replace_in_file.
pub struct DiffToolRenderer;

impl ToolRenderer for DiffToolRenderer {
    fn supported_tools(&self) -> &'static [&'static str] {
        &["edit", "write_file", "replace_in_file"]
    }

    fn render(&self, tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return;
        }

        let mut y = render_tool_header(tool_block, area, buf, area.y);

        // File path line
        y = render_file_path(tool_block, area, buf, y);

        // Diff body
        let diff_lines = generate_tool_diff_lines(tool_block);
        let bg = terminal_color::tool_content_bg();
        y = render_diff_to_buffer(&diff_lines, area, buf, area.x + 2, y, bg);

        render_error_line(tool_block, area, buf, y);
    }

    fn calculate_height(&self, tool_block: &ToolUseBlock, _width: u16) -> u16 {
        let mut height: u16 = 1; // header

        // File path
        if get_file_path(tool_block).is_some() {
            height += 1;
        }

        // Diff lines
        height += generate_tool_diff_lines(tool_block).len() as u16;

        if tool_block.status == ToolStatus::Error && tool_block.status_message.is_some() {
            height += 1;
        }
        height
    }

    fn render_history_lines(&self, tool_block: &ToolUseBlock) -> Vec<Line<'static>> {
        let mut lines = vec![tool_header_line(tool_block)];

        // File path
        if let Some(path) = get_file_path(tool_block) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(path, Style::default().fg(Color::Gray)),
            ]));
        }

        // Diff
        let diff_lines = generate_tool_diff_lines(tool_block);
        render_diff_to_history_lines(&diff_lines, &mut lines);

        push_error_history_line(tool_block, &mut lines);
        lines
    }
}

// ---------------------------------------------------------------------------
// DiffLine: shared representation
// ---------------------------------------------------------------------------

pub enum DiffLine {
    Context { line_num: usize, text: String },
    Insert { line_num: usize, text: String },
    Delete { line_num: usize, text: String },
    HunkSeparator,
}

// ---------------------------------------------------------------------------
// Diff generation per tool
// ---------------------------------------------------------------------------

/// Produce the appropriate diff lines for a tool block based on its name.
fn generate_tool_diff_lines(tool_block: &ToolUseBlock) -> Vec<DiffLine> {
    match tool_block.name.as_str() {
        "edit" => {
            let old = tool_block
                .parameters
                .get("old_text")
                .map(|p| p.value.as_str())
                .unwrap_or("");
            let new = tool_block
                .parameters
                .get("new_text")
                .map(|p| p.value.as_str())
                .unwrap_or("");
            if old.is_empty() && new.is_empty() {
                return Vec::new();
            }
            generate_diff_lines(old, new)
        }
        "replace_in_file" => {
            let diff = tool_block
                .parameters
                .get("diff")
                .map(|p| p.value.as_str())
                .unwrap_or("");
            if diff.is_empty() {
                return Vec::new();
            }
            generate_search_replace_diff_lines(diff)
        }
        "write_file" => {
            let content = tool_block
                .parameters
                .get("content")
                .map(|p| p.value.as_str())
                .unwrap_or("");
            if content.is_empty() {
                return Vec::new();
            }
            generate_write_file_diff_lines(content)
        }
        _ => Vec::new(),
    }
}

/// Generate diff lines from old/new text using the `similar` crate.
pub fn generate_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(old_text, new_text);

    let mut lines = Vec::new();
    let mut old_ln: usize = 1;
    let mut new_ln: usize = 1;

    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches('\n').to_string();
        match change.tag() {
            ChangeTag::Equal => {
                lines.push(DiffLine::Context {
                    line_num: new_ln,
                    text,
                });
                old_ln += 1;
                new_ln += 1;
            }
            ChangeTag::Delete => {
                lines.push(DiffLine::Delete {
                    line_num: old_ln,
                    text,
                });
                old_ln += 1;
            }
            ChangeTag::Insert => {
                lines.push(DiffLine::Insert {
                    line_num: new_ln,
                    text,
                });
                new_ln += 1;
            }
        }
    }
    lines
}

/// Parse the `<<<<<<< SEARCH` / `=======` / `>>>>>>> REPLACE` format used by
/// `replace_in_file` and emit diff lines.
pub fn generate_search_replace_diff_lines(diff_param: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut block_idx: usize = 0;

    let mut in_search = false;
    let mut in_replace = false;
    let mut search_lines: Vec<String> = Vec::new();
    let mut replace_lines: Vec<String> = Vec::new();

    for raw in diff_param.lines() {
        if raw.starts_with("<<<<<<< SEARCH") {
            if block_idx > 0 {
                lines.push(DiffLine::HunkSeparator);
            }
            in_search = true;
            in_replace = false;
            search_lines.clear();
            replace_lines.clear();
            continue;
        }
        if raw == "=======" && in_search {
            in_search = false;
            in_replace = true;
            continue;
        }
        if raw.starts_with(">>>>>>> REPLACE") && in_replace {
            in_replace = false;
            block_idx += 1;
            // Emit search lines as deletions
            for (i, s) in search_lines.iter().enumerate() {
                lines.push(DiffLine::Delete {
                    line_num: i + 1,
                    text: s.clone(),
                });
            }
            // Emit replace lines as insertions
            for (i, r) in replace_lines.iter().enumerate() {
                lines.push(DiffLine::Insert {
                    line_num: i + 1,
                    text: r.clone(),
                });
            }
            continue;
        }
        if in_search {
            search_lines.push(raw.to_string());
        } else if in_replace {
            replace_lines.push(raw.to_string());
        }
    }
    lines
}

/// For write_file: all lines are insertions.
pub fn generate_write_file_diff_lines(content: &str) -> Vec<DiffLine> {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| DiffLine::Insert {
            line_num: i + 1,
            text: line.to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

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

fn get_file_path(tool_block: &ToolUseBlock) -> Option<String> {
    tool_block
        .parameters
        .get("file_path")
        .or_else(|| tool_block.parameters.get("path"))
        .map(|p| p.value.clone())
        .filter(|v| !v.is_empty())
}

fn render_file_path(tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer, y: u16) -> u16 {
    if y >= area.y + area.height {
        return y;
    }
    if let Some(path) = get_file_path(tool_block) {
        buf.set_string(area.x + 2, y, &path, Style::default().fg(Color::Gray));
        y + 1
    } else {
        y
    }
}

fn line_number_width(max_line: usize) -> usize {
    if max_line == 0 {
        1
    } else {
        max_line.to_string().len()
    }
}

fn max_line_number(diff_lines: &[DiffLine]) -> usize {
    diff_lines
        .iter()
        .filter_map(|l| match l {
            DiffLine::Context { line_num, .. }
            | DiffLine::Insert { line_num, .. }
            | DiffLine::Delete { line_num, .. } => Some(*line_num),
            DiffLine::HunkSeparator => None,
        })
        .max()
        .unwrap_or(0)
}

/// Render diff lines into a ratatui Buffer with line numbers and background.
pub fn render_diff_to_buffer(
    diff_lines: &[DiffLine],
    area: Rect,
    buf: &mut Buffer,
    x: u16,
    mut y: u16,
    bg: Color,
) -> u16 {
    let max_ln = max_line_number(diff_lines);
    let gw = line_number_width(max_ln);

    for diff_line in diff_lines {
        if y >= area.y + area.height {
            break;
        }

        // Fill the entire row with the background color
        let row_width = area.width.saturating_sub(x - area.x);
        let bg_style = Style::default().bg(bg);
        buf.set_string(x, y, " ".repeat(row_width as usize), bg_style);

        match diff_line {
            DiffLine::HunkSeparator => {
                let spacer = format!("{:width$} ", "", width = gw);
                buf.set_string(
                    x,
                    y,
                    &spacer,
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                );
                buf.set_string(
                    x + spacer.len() as u16,
                    y,
                    "⋮",
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                );
            }
            DiffLine::Context { line_num, text } => {
                let gutter = format!("{:>width$} ", line_num, width = gw);
                buf.set_string(
                    x,
                    y,
                    &gutter,
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                );
                let content = format!(" {}", expand_tabs(text));
                buf.set_string(
                    x + gutter.len() as u16,
                    y,
                    &content,
                    Style::default().fg(Color::Gray).bg(bg),
                );
            }
            DiffLine::Insert { line_num, text } => {
                let gutter = format!("{:>width$} ", line_num, width = gw);
                buf.set_string(
                    x,
                    y,
                    &gutter,
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                );
                let content = format!("+{}", expand_tabs(text));
                buf.set_string(
                    x + gutter.len() as u16,
                    y,
                    &content,
                    Style::default().fg(Color::Green).bg(bg),
                );
            }
            DiffLine::Delete { line_num, text } => {
                let gutter = format!("{:>width$} ", line_num, width = gw);
                buf.set_string(
                    x,
                    y,
                    &gutter,
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                );
                let content = format!("-{}", expand_tabs(text));
                buf.set_string(
                    x + gutter.len() as u16,
                    y,
                    &content,
                    Style::default().fg(Color::Red).bg(bg),
                );
            }
        }
        y += 1;
    }
    y
}

/// Produce styled Lines for scrollback history.
pub fn render_diff_to_history_lines(diff_lines: &[DiffLine], lines: &mut Vec<Line<'static>>) {
    let max_ln = max_line_number(diff_lines);
    let gw = line_number_width(max_ln);
    let bg = terminal_color::tool_content_bg();
    let bg_style = Style::default().bg(bg);

    for diff_line in diff_lines {
        let line = match diff_line {
            DiffLine::HunkSeparator => Line::from(vec![
                Span::styled(
                    format!("  {:width$} ", "", width = gw),
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                ),
                Span::styled("⋮", Style::default().add_modifier(Modifier::DIM).bg(bg)),
            ]),
            DiffLine::Context { line_num, text } => Line::from(vec![
                Span::styled(
                    format!("  {:>width$} ", line_num, width = gw),
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                ),
                Span::styled(
                    format!(" {}", expand_tabs(text)),
                    Style::default().fg(Color::Gray).bg(bg),
                ),
            ]),
            DiffLine::Insert { line_num, text } => Line::from(vec![
                Span::styled(
                    format!("  {:>width$} ", line_num, width = gw),
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                ),
                Span::styled(
                    format!("+{}", expand_tabs(text)),
                    Style::default().fg(Color::Green).bg(bg),
                ),
            ]),
            DiffLine::Delete { line_num, text } => Line::from(vec![
                Span::styled(
                    format!("  {:>width$} ", line_num, width = gw),
                    Style::default().add_modifier(Modifier::DIM).bg(bg),
                ),
                Span::styled(
                    format!("-{}", expand_tabs(text)),
                    Style::default().fg(Color::Red).bg(bg),
                ),
            ]),
        };
        // Setting bg on the Line style causes history_insert to fill the
        // entire terminal row with the background colour (via ClearType::UntilNewLine).
        lines.push(line.style(bg_style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::terminal::message::ParameterValue;
    use indexmap::IndexMap;

    fn make_tool(name: &str, params: &[(&str, &str)]) -> ToolUseBlock {
        let mut parameters = IndexMap::new();
        for (k, v) in params {
            parameters.insert(k.to_string(), ParameterValue::new(v.to_string()));
        }
        ToolUseBlock {
            name: name.to_string(),
            id: "test-id".to_string(),
            parameters,
            status: ToolStatus::Success,
            status_message: None,
            output: None,
        }
    }

    #[test]
    fn test_edit_diff_lines() {
        let lines = generate_diff_lines("hello\nworld\n", "hello\nearth\n");
        assert_eq!(lines.len(), 3); // context + delete + insert
        match &lines[0] {
            DiffLine::Context { line_num, text } => {
                assert_eq!(*line_num, 1);
                assert_eq!(text, "hello");
            }
            _ => panic!("expected Context"),
        }
        match &lines[1] {
            DiffLine::Delete { text, .. } => assert_eq!(text, "world"),
            _ => panic!("expected Delete"),
        }
        match &lines[2] {
            DiffLine::Insert { text, .. } => assert_eq!(text, "earth"),
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_search_replace_diff_lines() {
        let diff = "<<<<<<< SEARCH\nold line 1\nold line 2\n=======\nnew line 1\n>>>>>>> REPLACE";
        let lines = generate_search_replace_diff_lines(diff);
        assert_eq!(lines.len(), 3);
        match &lines[0] {
            DiffLine::Delete { text, .. } => assert_eq!(text, "old line 1"),
            _ => panic!("expected Delete"),
        }
        match &lines[1] {
            DiffLine::Delete { text, .. } => assert_eq!(text, "old line 2"),
            _ => panic!("expected Delete"),
        }
        match &lines[2] {
            DiffLine::Insert { text, .. } => assert_eq!(text, "new line 1"),
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_search_replace_multiple_blocks() {
        let diff = "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n<<<<<<< SEARCH\nc\n=======\nd\n>>>>>>> REPLACE";
        let lines = generate_search_replace_diff_lines(diff);
        // block1: Delete(a), Insert(b), HunkSeparator, block2: Delete(c), Insert(d)
        assert_eq!(lines.len(), 5);
        matches!(&lines[2], DiffLine::HunkSeparator);
    }

    #[test]
    fn test_write_file_diff_lines() {
        let lines = generate_write_file_diff_lines("fn main() {\n    println!(\"hello\");\n}");
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            match line {
                DiffLine::Insert { line_num, .. } => assert_eq!(*line_num, i + 1),
                _ => panic!("expected Insert"),
            }
        }
    }

    #[test]
    fn test_height_edit() {
        let renderer = DiffToolRenderer;
        let tool = make_tool(
            "edit",
            &[
                ("file_path", "src/main.rs"),
                ("old_text", "hello\nworld\n"),
                ("new_text", "hello\nearth\n"),
            ],
        );
        // 1 header + 1 file path + 3 diff lines = 5
        assert_eq!(renderer.calculate_height(&tool, 80), 5);
    }

    #[test]
    fn test_height_write_file() {
        let renderer = DiffToolRenderer;
        let tool = make_tool(
            "write_file",
            &[("file_path", "new.rs"), ("content", "line1\nline2")],
        );
        // 1 header + 1 file path + 2 insert lines = 4
        assert_eq!(renderer.calculate_height(&tool, 80), 4);
    }
}
