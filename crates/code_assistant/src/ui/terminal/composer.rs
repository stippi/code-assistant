use ratatui::{
    layout::{Position, Rect},
    prelude::Frame,
    widgets::{Block, Borders},
};
use tui_textarea::TextArea;

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

    pub fn calculate_input_height(&self, textarea: &TextArea) -> u16 {
        let lines = textarea.lines().len() as u16;
        let height_with_border = lines + 1;
        height_with_border.clamp(2, self.max_input_rows + 1)
    }

    pub fn render(&self, f: &mut Frame, area: Rect, textarea: &TextArea) {
        let input_block = Block::default()
            .borders(Borders::TOP)
            .title("Input (Enter=send, Shift+Enter=newline, Ctrl+C=quit)");

        let inner_area = input_block.inner(area);
        f.render_widget(input_block, area);
        f.render_widget(textarea, inner_area);

        let cursor_pos = textarea.cursor();
        let cursor_x = inner_area.x + cursor_pos.1 as u16;
        let cursor_y = inner_area.y + cursor_pos.0 as u16;
        f.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}
