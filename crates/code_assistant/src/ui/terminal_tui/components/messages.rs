use crate::ui::{ui_events::MessageData, gpui::elements::MessageRole};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};

pub struct MessagesComponent {
    scroll_state: ScrollbarState,
    scroll_position: usize,
}

impl MessagesComponent {
    pub fn new() -> Self {
        Self {
            scroll_state: ScrollbarState::default(),
            scroll_position: 0,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, messages: &[MessageData]) {
        // Create the main block
        let block = Block::default()
            .title("Messages")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray));

        let inner_area = block.inner(area);

        // Convert messages to text
        let mut text_lines = Vec::new();

        for message in messages {
            // Add role indicator
            let role_style = match message.role {
                MessageRole::User => Style::default().fg(Color::Blue),
                MessageRole::Assistant => Style::default().fg(Color::Green),
            };

            let role_name = match message.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
            };

            text_lines.push(Line::from(vec![
                Span::styled(format!("▌{role_name}: "), role_style),
            ]));

            // Process fragments
            for fragment in &message.fragments {
                match fragment {
                    crate::ui::DisplayFragment::PlainText(text) => {
                        // Split text into lines and add them
                        for line in text.lines() {
                            text_lines.push(Line::from(line.to_string()));
                        }
                    }
                    crate::ui::DisplayFragment::ThinkingText(text) => {
                        // Render thinking text in italic/dim style
                        for line in text.lines() {
                            text_lines.push(Line::from(vec![
                                Span::styled(
                                    format!("💭 {line}"),
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]));
                        }
                    }
                    crate::ui::DisplayFragment::ToolName { name, id } => {
                        text_lines.push(Line::from(vec![
                            Span::styled(
                                format!("🔧 {name} ({id})"),
                                Style::default().fg(Color::Yellow),
                            ),
                        ]));
                    }
                    crate::ui::DisplayFragment::ToolParameter { name, value, tool_id: _ } => {
                        text_lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {name}: "),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::raw(value),
                        ]));
                    }
                    crate::ui::DisplayFragment::ToolEnd { id: _ } => {
                        text_lines.push(Line::from(vec![
                            Span::styled("  ✓ Tool completed", Style::default().fg(Color::Green)),
                        ]));
                    }
                    crate::ui::DisplayFragment::Image { media_type, data: _ } => {
                        text_lines.push(Line::from(vec![
                            Span::styled(
                                format!("🖼️  Image ({media_type})"),
                                Style::default().fg(Color::Magenta),
                            ),
                        ]));
                    }
                }
            }

            // Add separator between messages
            text_lines.push(Line::from(""));
        }

        // Create paragraph widget
        let text = Text::from(text_lines);
        let paragraph = Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: true })
            .scroll((self.scroll_position as u16, 0));

        frame.render_widget(paragraph, area);

        // Update scrollbar state
        let content_height = messages.len() * 3; // Rough estimate
        let visible_height = inner_area.height as usize;

        self.scroll_state = self.scroll_state
            .content_length(content_height)
            .viewport_content_length(visible_height)
            .position(self.scroll_position);

        // Render scrollbar if needed
        if content_height > visible_height {
            let scrollbar = Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓"));

            frame.render_stateful_widget(
                scrollbar,
                area.inner(ratatui::layout::Margin { vertical: 1, horizontal: 0 }),
                &mut self.scroll_state,
            );
        }
    }

    #[allow(dead_code)]
    pub fn scroll_up(&mut self) {
        self.scroll_position = self.scroll_position.saturating_sub(1);
    }

    #[allow(dead_code)]
    pub fn scroll_down(&mut self, max_scroll: usize) {
        self.scroll_position = (self.scroll_position + 1).min(max_scroll);
    }
}
