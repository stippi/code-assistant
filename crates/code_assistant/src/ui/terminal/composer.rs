use ratatui::{
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Widget, WidgetRef},
};

use super::custom_terminal;
use super::terminal_color;
use super::textarea::TextArea;

/// Width reserved for the "› " prefix to the left of the textarea.
const PREFIX_COLS: u16 = 2;

/// Return the composer background color, auto-detected from the terminal.
fn composer_bg() -> Color {
    terminal_color::composer_bg()
}

pub struct Composer {
    max_input_rows: u16,
}

impl Composer {
    pub fn new(max_input_rows: u16) -> Self {
        Self { max_input_rows }
    }

    #[cfg(test)]
    pub fn max_input_rows(&self) -> u16 {
        self.max_input_rows
    }

    /// Calculate total height:
    ///   1 (top padding) + textarea lines + 1 (bottom padding) + 1 (footer hints).
    pub fn calculate_input_height(&self, textarea: &TextArea, width: u16) -> u16 {
        let textarea_width = width.saturating_sub(PREFIX_COLS + 1); // prefix + 1 right margin
        let lines = textarea.desired_height(textarea_width);
        let total = lines + 3; // 1 top + textarea + 1 bottom padding + 1 footer
        total.clamp(4, self.max_input_rows + 3)
    }

    pub fn render(&self, f: &mut custom_terminal::Frame, area: Rect, textarea: &TextArea) {
        // Layout:
        //   Row 0:          empty (top padding, bg)
        //   Row 1..N:       › textarea content (bg)
        //   Row N+1:        empty (bottom padding, bg)
        //   Row N+2 (last): footer hints (no bg, dimmed)
        if area.height < 4 || area.width < PREFIX_COLS + 2 {
            return;
        }

        let bg_style = Style::default().bg(composer_bg());
        let footer_y = area.y + area.height - 1;
        let bg_height = area.height - 1; // everything except the footer row
        let textarea_height = bg_height.saturating_sub(2); // minus top and bottom padding

        // Fill the background area (excludes footer row)
        let bg_rect = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: bg_height,
        };
        Block::default()
            .style(bg_style)
            .render(bg_rect, f.buffer_mut());

        // Textarea area: inset from left by PREFIX_COLS, from right by 1
        let textarea_rect = Rect {
            x: area.x + PREFIX_COLS,
            y: area.y + 1,
            width: area.width.saturating_sub(PREFIX_COLS + 1),
            height: textarea_height,
        };

        // Render "› " prefix on the first textarea row
        let prompt = Span::styled(
            "›",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(composer_bg()),
        );
        f.buffer_mut()
            .set_span(area.x, area.y + 1, &prompt, PREFIX_COLS);

        // Render textarea
        (&textarea).render_ref(textarea_rect, f.buffer_mut());

        // Apply background to textarea cells (textarea renders with default bg)
        for row in 0..textarea_rect.height {
            for col in 0..textarea_rect.width {
                if let Some(cell) = f
                    .buffer_mut()
                    .cell_mut((textarea_rect.x + col, textarea_rect.y + row))
                {
                    if cell.bg == Color::Reset {
                        cell.set_style(Style::default().bg(composer_bg()));
                    }
                }
            }
        }

        // Render footer hints below the background area (dimmed, no bg)
        let action_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);
        let mapping_style = Style::default().fg(Color::Gray).add_modifier(Modifier::DIM);
        let footer_line = Line::from(vec![
            Span::styled("  Enter", action_style),
            Span::styled(" send  ", mapping_style),
            Span::styled("Shift+Enter", action_style),
            Span::styled(" newline  ", mapping_style),
            Span::styled("Esc", action_style),
            Span::styled(" dismiss  ", mapping_style),
            Span::styled("/help", action_style),
            Span::styled(" commands", mapping_style),
        ]);
        let footer_rect = Rect {
            x: area.x,
            y: footer_y,
            width: area.width,
            height: 1,
        };
        footer_line.render(footer_rect, f.buffer_mut());

        // Set cursor position (relative to textarea_rect)
        if let Some((cursor_x, cursor_y)) = textarea.cursor_position(textarea_rect) {
            f.set_cursor_position(Position::new(cursor_x, cursor_y));
        }
    }
}
