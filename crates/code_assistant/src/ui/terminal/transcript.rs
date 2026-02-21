use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use tui_markdown as md;

use super::message::{LiveMessage, MessageBlock};
use crate::ui::ToolStatus;

/// Convert a `Line<'_>` to `Line<'static>` by owning all span content.
fn line_to_static(line: Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content.to_string(), span.style))
            .collect::<Vec<_>>(),
    )
}

pub struct TranscriptState {
    committed_messages: Vec<LiveMessage>,
    committed_rendered_count: usize,
    active_message: Option<LiveMessage>,
}

impl TranscriptState {
    pub fn new() -> Self {
        Self {
            committed_messages: Vec::new(),
            committed_rendered_count: 0,
            active_message: None,
        }
    }

    pub fn active_message(&self) -> Option<&LiveMessage> {
        self.active_message.as_ref()
    }

    pub fn active_message_mut(&mut self) -> Option<&mut LiveMessage> {
        self.active_message.as_mut()
    }

    pub fn start_active_message(&mut self) {
        self.finalize_active_if_content();
        self.active_message = Some(LiveMessage::new());
    }

    pub fn finalize_active_if_content(&mut self) {
        if let Some(mut current_message) = self.active_message.take() {
            current_message.finalized = true;
            if current_message.has_content() {
                self.committed_messages.push(current_message);
            }
        }
    }

    pub fn push_committed_message(&mut self, mut message: LiveMessage) {
        message.finalized = true;
        self.committed_messages.push(message);
    }

    pub fn clear(&mut self) {
        self.committed_messages.clear();
        self.committed_rendered_count = 0;
        self.active_message = None;
    }

    #[cfg(test)]
    pub fn committed_messages(&self) -> &[LiveMessage] {
        &self.committed_messages
    }

    #[cfg(test)]
    pub fn committed_messages_mut(&mut self) -> &mut Vec<LiveMessage> {
        &mut self.committed_messages
    }

    pub fn unrendered_committed_messages(&self) -> &[LiveMessage] {
        &self.committed_messages[self.committed_rendered_count..]
    }

    pub fn mark_committed_as_rendered(&mut self) {
        self.committed_rendered_count = self.committed_messages.len();
    }

    pub fn as_history_lines(message: &LiveMessage) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for block in &message.blocks {
            match block {
                MessageBlock::PlainText(text) => {
                    if text.content.is_empty() {
                        continue;
                    }
                    let rendered = md::from_str(&text.content);
                    for line in rendered.lines {
                        lines.push(line_to_static(line));
                    }
                }
                MessageBlock::Thinking(thinking) => {
                    if thinking.content.trim().is_empty() {
                        continue;
                    }
                    // Render through markdown for inline formatting (code, etc.)
                    // but apply Yellow+Italic as base style to all spans.
                    // Don't wrap in *...* — multi-line italic doesn't work in markdown.
                    let rendered = md::from_str(&thinking.content);
                    for line in rendered.lines {
                        let styled_spans: Vec<Span<'static>> = line
                            .spans
                            .into_iter()
                            .map(|span| {
                                let mut style = span.style;
                                style = style.fg(Color::Yellow).add_modifier(Modifier::ITALIC);
                                Span::styled(span.content.to_string(), style)
                            })
                            .collect();
                        lines.push(Line::from(styled_spans));
                    }
                }
                MessageBlock::UserText(text) => {
                    if text.content.is_empty() {
                        continue;
                    }
                    lines.push(Line::from(""));
                    for (i, line) in text.content.lines().enumerate() {
                        let prefix = if i == 0 {
                            Span::styled(
                                "› ",
                                Style::default()
                                    .add_modifier(Modifier::BOLD)
                                    .add_modifier(Modifier::DIM),
                            )
                        } else {
                            Span::raw("  ")
                        };
                        lines.push(Line::from(vec![prefix, Span::raw(line.to_string())]));
                    }
                    lines.push(Line::from(""));
                }
                MessageBlock::ToolUse(tool) => {
                    lines.push(Line::styled(
                        format!("tool: {}", tool.name),
                        Style::default().fg(Color::Cyan),
                    ));
                    for (param_name, param_value) in &tool.parameters {
                        for line in param_value.value.lines() {
                            lines.push(Line::from(format!("  {param_name}: {line}")));
                        }
                    }
                    if let Some(status_message) = &tool.status_message {
                        lines.push(Line::styled(
                            format!("  status: {status_message}"),
                            Style::default().fg(match tool.status {
                                ToolStatus::Pending => Color::Gray,
                                ToolStatus::Running => Color::Blue,
                                ToolStatus::Success => Color::Green,
                                ToolStatus::Error => Color::Red,
                            }),
                        ));
                    }
                    if let Some(output) = &tool.output {
                        for line in output.lines() {
                            lines.push(Line::from(format!("  {line}")));
                        }
                    }
                }
            }
        }

        lines
    }
}
