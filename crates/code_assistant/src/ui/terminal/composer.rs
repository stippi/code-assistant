use ratatui::{
    layout::{Position, Rect},
    widgets::{Block, Borders},
};

use super::custom_terminal;
use super::textarea::TextArea;

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

    pub fn calculate_input_height(&self, textarea: &TextArea, width: u16) -> u16 {
        let lines = textarea.desired_height(width);
        let height_with_border = lines + 1;
        height_with_border.clamp(2, self.max_input_rows + 1)
    }

    pub fn render(&self, f: &mut custom_terminal::Frame, area: Rect, textarea: &TextArea) {
        let input_block = Block::default()
            .borders(Borders::TOP)
            .title("Input (Enter=send, Shift+Enter=newline, Ctrl+C=quit)");

        let inner_area = input_block.inner(area);
        f.render_widget(input_block, area);

        // Render textarea using WidgetRef
        use ratatui::widgets::WidgetRef;
        (&textarea).render_ref(inner_area, f.buffer_mut());

        // Set cursor position
        if let Some((cursor_x, cursor_y)) = textarea.cursor_position(inner_area) {
            f.set_cursor_position(Position::new(cursor_x, cursor_y));
        }
    }
}
