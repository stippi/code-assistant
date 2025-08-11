use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers},
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders},
    Frame,
};
use tui_textarea::{Input, TextArea};

pub struct InputComponent {
    textarea: TextArea<'static>,
}

impl InputComponent {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .title("Input (Enter: send, Shift+Enter: new line, Esc: cancel)")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Gray)),
        );
        textarea.set_style(Style::default());
        textarea.set_cursor_style(Style::default().fg(Color::Yellow));

        Self { textarea }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(&self.textarea, area);
    }

    pub fn handle_input(&mut self, input: ratatui::crossterm::event::Event) -> InputResult {
        match input {
            ratatui::crossterm::event::Event::Key(key) => {
                match key.code {
                    KeyCode::Enter => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            // Shift+Enter: insert new line
                            self.textarea.input(Input::from(key));
                            InputResult::None
                        } else {
                            // Enter: send message
                            let content = self.get_content();
                            self.clear();
                            InputResult::SendMessage(content)
                        }
                    }
                    KeyCode::Esc => {
                        InputResult::Cancel
                    }
                    _ => {
                        self.textarea.input(Input::from(key));
                        InputResult::None
                    }
                }
            }
            _ => InputResult::None,
        }
    }

    pub fn get_content(&self) -> String {
        self.textarea.lines().join("\n")
    }

    pub fn clear(&mut self) {
        self.textarea = TextArea::default();
        self.textarea.set_block(
            Block::default()
                .title("Input (Enter: send, Shift+Enter: new line, Esc: cancel)")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Gray)),
        );
        self.textarea.set_style(Style::default());
        self.textarea.set_cursor_style(Style::default().fg(Color::Yellow));
    }

    #[allow(dead_code)]
    pub fn set_content(&mut self, content: &str) {
        self.clear();
        for line in content.lines() {
            if !self.textarea.is_empty() {
                self.textarea.input(Input::from(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press,
                    state: KeyEventState::NONE,
                }));
            }
            for ch in line.chars() {
                self.textarea.input(Input::from(KeyEvent {
                    code: KeyCode::Char(ch),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press,
                    state: KeyEventState::NONE,
                }));
            }
        }
    }
}

pub enum InputResult {
    None,
    SendMessage(String),
    Cancel,
}
