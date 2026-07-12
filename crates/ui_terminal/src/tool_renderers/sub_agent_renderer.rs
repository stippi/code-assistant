//! Renderer for the `spawn_agent` (sub-agent) tool.
//!
//! Mirrors the GPUI `sub_agent_card`: a "Sub-agent" header with an
//! instructions summary, the sub-agent's live tool-call list with per-call
//! status colors, and a cancelled/error/response footer. Parses the tool's
//! output as [`SubAgentOutput`] JSON streamed by the sub-agent runner.
//!
//! A single [`sub_agent_lines`] builder feeds all three trait methods so the
//! live viewport, the height calculation, and the scrollback history stay in
//! lockstep (the generic fallback used to under-count the height).

use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;

use super::{status_color, ToolRenderer};
use crate::message::ToolUseBlock;
use code_assistant_core::agent::sub_agent::{SubAgentOutput, SubAgentToolStatus};
use code_assistant_core::ui::ToolStatus;

/// Maximum length of the instructions summary shown in the header.
const INSTRUCTIONS_SUMMARY_LEN: usize = 60;

/// Renderer for the `spawn_agent` tool.
pub struct SubAgentToolRenderer;

impl ToolRenderer for SubAgentToolRenderer {
    fn supported_tools(&self) -> &'static [&'static str] {
        &["spawn_agent"]
    }

    fn render(&self, tool_block: &ToolUseBlock, area: Rect, buf: &mut Buffer) {
        if area.height < 1 {
            return;
        }
        // Non-wrapping paragraph: one row per line, clipped horizontally, so
        // the rendered height matches `calculate_height` exactly.
        Paragraph::new(sub_agent_lines(tool_block)).render(area, buf);
    }

    fn calculate_height(&self, tool_block: &ToolUseBlock, _width: u16) -> u16 {
        sub_agent_lines(tool_block).len() as u16
    }

    fn render_history_lines(&self, tool_block: &ToolUseBlock) -> Vec<Line<'static>> {
        sub_agent_lines(tool_block)
    }
}

/// Truncate `text` to at most `max` chars, appending `…` when cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    } else {
        text.to_string()
    }
}

/// Build the styled lines for a sub-agent tool block. Shared by the live
/// viewport, the height calculation, and the scrollback history.
fn sub_agent_lines(tool_block: &ToolUseBlock) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Header: ● Sub-agent[: <instructions summary>] ──────────────────────
    let header_color = status_color(&tool_block.status);
    let mut header = vec![
        Span::styled("● ", Style::default().fg(header_color)),
        Span::styled(
            "Sub-agent",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(instructions) = tool_block.parameters.get("instructions") {
        let summary = truncate(instructions.value.trim(), INSTRUCTIONS_SUMMARY_LEN);
        if !summary.is_empty() {
            header.push(Span::styled(
                format!(": {summary}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    lines.push(Line::from(header));

    // ── Body: parsed sub-agent output ──────────────────────────────────────
    if let Some(output) = tool_block.output.as_ref().filter(|o| !o.is_empty()) {
        if let Some(sub) = SubAgentOutput::from_json(output) {
            for call in &sub.tools {
                let color = match call.status {
                    SubAgentToolStatus::Running => Color::Blue,
                    SubAgentToolStatus::Success => Color::Green,
                    SubAgentToolStatus::Error => Color::Red,
                };
                let text = call
                    .title
                    .as_ref()
                    .filter(|t| !t.is_empty())
                    .cloned()
                    .or_else(|| call.message.as_ref().filter(|m| !m.is_empty()).cloned())
                    .unwrap_or_else(|| call.name.replace('_', " "));
                lines.push(Line::from(vec![
                    Span::styled("  ● ", Style::default().fg(color)),
                    Span::styled(text, Style::default().fg(Color::Gray)),
                ]));
            }

            if sub.cancelled == Some(true) {
                lines.push(Line::styled(
                    "  Sub-agent cancelled",
                    Style::default().fg(Color::Yellow),
                ));
            }
            if let Some(error) = &sub.error {
                lines.push(Line::styled(
                    format!("  Error: {error}"),
                    Style::default().fg(Color::Red),
                ));
            }
            // Final response from the sub-agent (shown when it completed).
            if let Some(response) = sub.response.as_ref().filter(|r| !r.trim().is_empty()) {
                for line in response.lines() {
                    lines.push(Line::styled(
                        format!("  {line}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        } else {
            // Output present but not JSON — show it verbatim.
            for line in output.lines() {
                lines.push(Line::from(format!("  {line}")));
            }
        }
    }

    // ── Tool-level error message ───────────────────────────────────────────
    if tool_block.status == ToolStatus::Error {
        if let Some(message) = &tool_block.status_message {
            lines.push(Line::styled(
                format!("  {message}"),
                Style::default().fg(Color::LightRed),
            ));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ParameterValue;
    use code_assistant_core::agent::sub_agent::{SubAgentActivity, SubAgentToolCall};
    use indexmap::IndexMap;

    fn make_tool(params: &[(&str, &str)], output: Option<String>) -> ToolUseBlock {
        let mut parameters = IndexMap::new();
        for (k, v) in params {
            parameters.insert(k.to_string(), ParameterValue::new(v.to_string()));
        }
        ToolUseBlock {
            name: "spawn_agent".to_string(),
            id: "test-id".to_string(),
            parameters,
            status: ToolStatus::Success,
            status_message: None,
            output,
        }
    }

    fn output_with(
        tools: Vec<SubAgentToolCall>,
        cancelled: Option<bool>,
        error: Option<String>,
        response: Option<String>,
    ) -> String {
        SubAgentOutput {
            tools,
            activity: Some(SubAgentActivity::Completed),
            cancelled,
            error,
            response,
            usage: None,
        }
        .to_json()
    }

    fn call(name: &str, status: SubAgentToolStatus, title: Option<&str>) -> SubAgentToolCall {
        SubAgentToolCall {
            name: name.to_string(),
            status,
            title: title.map(|t| t.to_string()),
            message: None,
            parameters: Default::default(),
        }
    }

    #[test]
    fn test_supports_spawn_agent() {
        assert!(SubAgentToolRenderer
            .supported_tools()
            .contains(&"spawn_agent"));
    }

    #[test]
    fn test_header_only_when_no_output() {
        let tool = make_tool(&[("instructions", "Investigate the failing test")], None);
        let lines = sub_agent_lines(&tool);
        assert_eq!(lines.len(), 1);
        // Header carries the instructions summary.
        let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("Sub-agent"));
        assert!(rendered.contains("Investigate the failing test"));
    }

    #[test]
    fn test_long_instructions_truncated() {
        let long = "a".repeat(200);
        let tool = make_tool(&[("instructions", &long)], None);
        let lines = sub_agent_lines(&tool);
        let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.ends_with('…'));
        assert!(rendered.chars().count() < long.len());
    }

    #[test]
    fn test_tool_calls_listed_and_height_matches() {
        let output = output_with(
            vec![
                call(
                    "read_files",
                    SubAgentToolStatus::Success,
                    Some("Read main.rs"),
                ),
                call("execute_command", SubAgentToolStatus::Running, None),
            ],
            None,
            None,
            None,
        );
        let tool = make_tool(&[("instructions", "Do work")], Some(output));
        let lines = sub_agent_lines(&tool);
        // header + 2 tool calls
        assert_eq!(lines.len(), 3);
        // Height calc must match the rendered line count (the fallback bug).
        assert_eq!(SubAgentToolRenderer.calculate_height(&tool, 80), 3);

        let first_call: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_call.contains("Read main.rs"));
        // Missing title falls back to a humanized tool name.
        let second_call: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(second_call.contains("execute command"));
    }

    #[test]
    fn test_cancelled_and_error_and_response_footer() {
        let output = output_with(
            vec![call(
                "read_files",
                SubAgentToolStatus::Success,
                Some("Read"),
            )],
            Some(true),
            Some("boom".to_string()),
            Some("Here is the answer\nsecond line".to_string()),
        );
        let tool = make_tool(&[], Some(output));
        let lines = sub_agent_lines(&tool);
        let joined: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(joined.iter().any(|l| l.contains("Sub-agent cancelled")));
        assert!(joined.iter().any(|l| l.contains("Error: boom")));
        assert!(joined.iter().any(|l| l.contains("Here is the answer")));
        assert!(joined.iter().any(|l| l.contains("second line")));
    }

    #[test]
    fn test_non_json_output_shown_verbatim() {
        let tool = make_tool(&[], Some("plain streaming text".to_string()));
        let lines = sub_agent_lines(&tool);
        let joined: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(joined.iter().any(|l| l.contains("plain streaming text")));
    }
}
