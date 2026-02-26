//! Compact renderer for read/explore tools.
//!
//! Shows only the tool name, project, and key identifiers (paths, patterns,
//! URLs) — never the file contents, search results, or full tool output.

use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};

use super::{
    push_error_history_line, render_error_line, render_tool_header, tool_header_line, ToolRenderer,
};
use crate::ui::terminal::message::ToolUseBlock;
use crate::ui::ToolStatus;

/// Renderer for read/explore tools: read_files, list_files, list_projects,
/// search_files, glob_files, web_search, web_fetch.
pub struct CompactToolRenderer;

impl ToolRenderer for CompactToolRenderer {
    fn supported_tools(&self) -> &'static [&'static str] {
        &[
            "read_files",
            "list_files",
            "list_projects",
            "search_files",
            "glob_files",
            "web_search",
            "web_fetch",
        ]
    }

    fn render(&self, tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return;
        }

        let mut y = render_tool_header(tool_block, area, buf, area.y);

        for line in compact_lines(tool_block) {
            if y >= area.y + area.height {
                break;
            }
            match line {
                CompactLine::Item(text) => {
                    buf.set_string(area.x + 2, y, "- ", Style::default().fg(Color::DarkGray));
                    let max_len = area.width.saturating_sub(4) as usize;
                    let display = if text.len() > max_len {
                        &text[..max_len]
                    } else {
                        text.as_str()
                    };
                    buf.set_string(area.x + 4, y, display, Style::default().fg(Color::Gray));
                }
                CompactLine::KeyValue(key, value) => {
                    let key_len = key.len() as u16;
                    buf.set_string(area.x + 2, y, &key, Style::default().fg(Color::Cyan));
                    buf.set_string(
                        area.x + 2 + key_len,
                        y,
                        ": ",
                        Style::default().fg(Color::White),
                    );
                    let max_len = area.width.saturating_sub(4 + key_len) as usize;
                    let display = if value.len() > max_len {
                        &value[..max_len]
                    } else {
                        value.as_str()
                    };
                    buf.set_string(
                        area.x + 4 + key_len,
                        y,
                        display,
                        Style::default().fg(Color::Gray),
                    );
                }
            }
            y += 1;
        }

        render_error_line(tool_block, area, buf, y);
    }

    fn calculate_height(&self, tool_block: &ToolUseBlock, _width: u16) -> u16 {
        let mut height: u16 = 1; // header line
        height += compact_lines(tool_block).len() as u16;
        if tool_block.status == ToolStatus::Error && tool_block.status_message.is_some() {
            height += 1;
        }
        height
    }

    fn render_history_lines(&self, tool_block: &ToolUseBlock) -> Vec<Line<'static>> {
        let mut lines = vec![tool_header_line(tool_block)];

        for compact in compact_lines(tool_block) {
            match compact {
                CompactLine::Item(text) => {
                    lines.push(Line::from(vec![
                        Span::styled("  - ", Style::default().fg(Color::DarkGray)),
                        Span::styled(text, Style::default().fg(Color::Gray)),
                    ]));
                }
                CompactLine::KeyValue(key, value) => {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            key,
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                        ),
                        Span::styled(": ", Style::default().fg(Color::White)),
                        Span::styled(value, Style::default().fg(Color::Gray)),
                    ]));
                }
            }
        }

        push_error_history_line(tool_block, &mut lines);
        lines
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

enum CompactLine {
    /// A simple list item, e.g. a file path.
    Item(String),
    /// A labelled value, e.g. `pattern: *.rs`.
    KeyValue(String, String),
}

/// Extract the compact display items for a given tool block.
fn compact_lines(tool_block: &ToolUseBlock) -> Vec<CompactLine> {
    let mut out = Vec::new();
    match tool_block.name.as_str() {
        "read_files" => {
            if let Some(paths) = tool_block.parameters.get("paths") {
                for path in paths.value.lines() {
                    let path = path.trim();
                    if !path.is_empty() {
                        out.push(CompactLine::Item(path.to_string()));
                    }
                }
            }
        }
        "list_files" => {
            if let Some(path) = tool_block.parameters.get("path") {
                let val = path.value.trim();
                if !val.is_empty() {
                    out.push(CompactLine::Item(val.to_string()));
                }
            }
        }
        "search_files" => {
            if let Some(pattern) = tool_block.parameters.get("pattern") {
                out.push(CompactLine::KeyValue(
                    "pattern".into(),
                    pattern.value.clone(),
                ));
            }
            // Also accept "regex" (alias used in some configurations)
            if let Some(regex) = tool_block.parameters.get("regex") {
                if !tool_block.parameters.contains_key("pattern") {
                    out.push(CompactLine::KeyValue("regex".into(), regex.value.clone()));
                }
            }
            if let Some(path) = tool_block.parameters.get("path") {
                let val = path.value.trim();
                if !val.is_empty() {
                    out.push(CompactLine::Item(val.to_string()));
                }
            }
        }
        "glob_files" => {
            if let Some(pattern) = tool_block.parameters.get("pattern") {
                out.push(CompactLine::KeyValue(
                    "pattern".into(),
                    pattern.value.clone(),
                ));
            }
        }
        "web_search" => {
            if let Some(query) = tool_block.parameters.get("query") {
                out.push(CompactLine::KeyValue("query".into(), query.value.clone()));
            }
        }
        "web_fetch" => {
            if let Some(url) = tool_block.parameters.get("url") {
                out.push(CompactLine::KeyValue("url".into(), url.value.clone()));
            }
        }
        "list_projects" => {
            // No additional parameters to show
        }
        _ => {}
    }
    out
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
    fn test_read_files_compact() {
        let tool = make_tool("read_files", &[("paths", "src/main.rs\nsrc/lib.rs")]);
        let lines = compact_lines(&tool);
        assert_eq!(lines.len(), 2);
        match &lines[0] {
            CompactLine::Item(p) => assert_eq!(p, "src/main.rs"),
            _ => panic!("expected Item"),
        }
        match &lines[1] {
            CompactLine::Item(p) => assert_eq!(p, "src/lib.rs"),
            _ => panic!("expected Item"),
        }
    }

    #[test]
    fn test_search_files_compact() {
        let tool = make_tool("search_files", &[("pattern", "fn main"), ("path", "src/")]);
        let lines = compact_lines(&tool);
        assert_eq!(lines.len(), 2);
        match &lines[0] {
            CompactLine::KeyValue(k, v) => {
                assert_eq!(k, "pattern");
                assert_eq!(v, "fn main");
            }
            _ => panic!("expected KeyValue"),
        }
    }

    #[test]
    fn test_web_search_compact() {
        let tool = make_tool("web_search", &[("query", "rust ratatui tutorial")]);
        let lines = compact_lines(&tool);
        assert_eq!(lines.len(), 1);
        match &lines[0] {
            CompactLine::KeyValue(k, v) => {
                assert_eq!(k, "query");
                assert_eq!(v, "rust ratatui tutorial");
            }
            _ => panic!("expected KeyValue"),
        }
    }

    #[test]
    fn test_list_projects_empty() {
        let tool = make_tool("list_projects", &[]);
        let lines = compact_lines(&tool);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_height_matches_lines() {
        let renderer = CompactToolRenderer;
        let tool = make_tool(
            "read_files",
            &[("paths", "a.rs\nb.rs\nc.rs"), ("project", "my-proj")],
        );
        // 1 header + 3 items = 4
        assert_eq!(renderer.calculate_height(&tool, 80), 4);
    }

    #[test]
    fn test_height_with_error() {
        let renderer = CompactToolRenderer;
        let mut tool = make_tool("read_files", &[("paths", "a.rs")]);
        tool.status = ToolStatus::Error;
        tool.status_message = Some("File not found".to_string());
        // 1 header + 1 item + 1 error = 3
        assert_eq!(renderer.calculate_height(&tool, 80), 3);
    }
}
