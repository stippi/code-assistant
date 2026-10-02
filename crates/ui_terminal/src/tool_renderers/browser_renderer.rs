//! Compact renderer for the `browser_*` tools.
//!
//! Browser work is many small calls, and some return a lot of text (a page's
//! accessibility tree, its text, logs). Each call renders as its header, a
//! one-line description ("Click ref_4") and the first line of its result —
//! never the full output, which the model already has.

use ratatui::prelude::*;
use ratatui::style::{Color, Style};

use super::{
    ToolRenderer, push_error_history_line, render_error_line, render_tool_header, tool_header_line,
};
use crate::message::ToolUseBlock;
use crate::text_util::truncate_to_width;
use code_assistant_core::tools::impls::browser::describe::{
    BROWSER_TOOL_NAMES, describe_batch_steps, describe_call, target_label,
};
use code_assistant_core::ui::ToolStatus;

pub struct BrowserToolRenderer;

/// One line under the header: what the call does, or what it returned.
#[derive(Debug, PartialEq)]
enum BrowserLine {
    Action(String),
    Result(String),
}

impl ToolRenderer for BrowserToolRenderer {
    fn supported_tools(&self) -> &'static [&'static str] {
        &BROWSER_TOOL_NAMES
    }

    fn render(&self, tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return;
        }
        let mut y = render_tool_header(tool_block, area, buf, area.y);
        let max_len = area.width.saturating_sub(2) as usize;
        for line in browser_lines(tool_block) {
            if y >= area.y + area.height {
                break;
            }
            let (text, style) = styled(&line);
            buf.set_string(area.x + 2, y, truncate_to_width(&text, max_len), style);
            y += 1;
        }
        render_error_line(tool_block, area, buf, y);
    }

    fn calculate_height(&self, tool_block: &ToolUseBlock, _width: u16) -> u16 {
        let mut height = 1 + browser_lines(tool_block).len() as u16;
        if tool_block.status == ToolStatus::Error && tool_block.status_message.is_some() {
            height += 1;
        }
        height
    }

    fn render_history_lines(&self, tool_block: &ToolUseBlock) -> Vec<Line<'static>> {
        let mut lines = vec![tool_header_line(tool_block)];
        for line in browser_lines(tool_block) {
            let (text, style) = styled(&line);
            lines.push(Line::from(vec![Span::raw("  "), Span::styled(text, style)]));
        }
        push_error_history_line(tool_block, &mut lines);
        lines
    }
}

fn styled(line: &BrowserLine) -> (String, Style) {
    match line {
        BrowserLine::Action(text) => (text.clone(), Style::default().fg(Color::Gray)),
        BrowserLine::Result(text) => (format!("→ {text}"), Style::default().fg(Color::DarkGray)),
    }
}

fn browser_lines(tool: &ToolUseBlock) -> Vec<BrowserLine> {
    let param = |name: &str| {
        tool.parameters
            .get(name)
            .map(|p| p.value.clone())
            .filter(|v| !v.is_empty())
    };
    let mut lines = Vec::new();

    if tool.name == "browser_batch" {
        let steps = param("actions")
            .map(|json| describe_batch_steps(&json))
            .unwrap_or_default();
        for (i, step) in steps.iter().enumerate() {
            lines.push(BrowserLine::Action(format!("{}. {step}", i + 1)));
        }
        return lines;
    }

    let mut action = describe_call(&tool.name, &param);
    if let Some(target) = target_label(&param) {
        action.push_str(&format!("  [{target}]"));
    }
    lines.push(BrowserLine::Action(action));

    // The first line of a successful result; errors show via the status line.
    if tool.status == ToolStatus::Success
        && let Some(output) = tool.output.as_deref()
    {
        let mut rest = output.trim().lines().filter(|l| !l.trim().is_empty());
        if let Some(first) = rest.next() {
            let more = rest.count();
            let suffix = if more > 0 {
                format!(" (+{more} lines)")
            } else {
                String::new()
            };
            lines.push(BrowserLine::Result(format!("{}{suffix}", first.trim())));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ParameterValue;
    use indexmap::IndexMap;

    fn make_tool(name: &str, params: &[(&str, &str)], output: Option<&str>) -> ToolUseBlock {
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
            output: output.map(str::to_string),
        }
    }

    #[test]
    fn a_long_result_shows_only_its_first_line() {
        let tool = make_tool(
            "browser_read_page",
            &[("filter", "interactive"), ("profile", "elster")],
            Some("[t1] https://x.test — X\n- button \"Go\" [ref_1]\n- link \"Docs\" [ref_2]"),
        );
        assert_eq!(
            browser_lines(&tool),
            vec![
                BrowserLine::Action("Read interactive elements  [elster]".into()),
                BrowserLine::Result("[t1] https://x.test — X (+2 lines)".into()),
            ]
        );
    }

    #[test]
    fn a_click_shows_its_action_and_result() {
        let tool = make_tool(
            "browser_computer",
            &[("action", "left_click"), ("ref", "ref_4")],
            Some("Clicked ref_4"),
        );
        assert_eq!(
            browser_lines(&tool),
            vec![
                BrowserLine::Action("Click ref_4".into()),
                BrowserLine::Result("Clicked ref_4".into()),
            ]
        );
    }

    #[test]
    fn a_batch_lists_its_steps() {
        let tool = make_tool(
            "browser_batch",
            &[(
                "actions",
                r#"[{"name":"browser_computer","input":{"action":"screenshot"}},{"name":"browser_find","input":{"query":"Save"}}]"#,
            )],
            Some("[1] computer screenshot\n…"),
        );
        assert_eq!(
            browser_lines(&tool),
            vec![
                BrowserLine::Action("1. Screenshot".into()),
                BrowserLine::Action("2. Find \"Save\"".into()),
            ]
        );
    }
}
