/// Manages the input area at the bottom of the terminal
pub struct InputArea {
    content: String,
    cursor_pos: usize, // Character position, not byte position
    terminal_width: u16,
    max_lines: usize,
    scroll_offset: usize, // For virtual scrolling when content exceeds max_lines
}

impl InputArea {
    pub fn new(terminal_width: u16) -> Self {
        Self {
            content: String::new(),
            cursor_pos: 0,
            terminal_width,
            max_lines: 5,
            scroll_offset: 0,
        }
    }

    pub fn update_terminal_width(&mut self, width: u16) {
        self.terminal_width = width;
        // Adjust scroll offset if needed after width change
        self.adjust_scroll_offset();
    }

    pub fn insert_char(&mut self, c: char) {
        let byte_pos = self.char_pos_to_byte_pos(self.cursor_pos);
        self.content.insert(byte_pos, c);
        self.cursor_pos += 1;
        self.adjust_scroll_offset();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn delete_char(&mut self) {
        let char_count = self.content.chars().count();
        if self.cursor_pos < char_count {
            let byte_pos = self.char_pos_to_byte_pos(self.cursor_pos);
            self.content.remove(byte_pos);
            self.adjust_scroll_offset();
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            let byte_pos = self.char_pos_to_byte_pos(self.cursor_pos);
            self.content.remove(byte_pos);
            self.adjust_scroll_offset();
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.adjust_scroll_offset();
        }
    }

    pub fn move_cursor_right(&mut self) {
        let char_count = self.content.chars().count();
        if self.cursor_pos < char_count {
            self.cursor_pos += 1;
            self.adjust_scroll_offset();
        }
    }

    pub fn move_cursor_up(&mut self) {
        let lines = self.get_wrapped_lines();
        let (current_line, current_col) = self.get_cursor_line_col(&lines);

        if current_line > 0 {
            let target_line = current_line - 1;
            let target_col = current_col.min(lines[target_line].chars().count());
            self.cursor_pos = self.line_col_to_cursor_pos(&lines, target_line, target_col);
            self.adjust_scroll_offset();
        }
    }

    pub fn move_cursor_down(&mut self) {
        let lines = self.get_wrapped_lines();
        let (current_line, current_col) = self.get_cursor_line_col(&lines);

        if current_line < lines.len() - 1 {
            let target_line = current_line + 1;
            let target_col = current_col.min(lines[target_line].chars().count());
            self.cursor_pos = self.line_col_to_cursor_pos(&lines, target_line, target_col);
            self.adjust_scroll_offset();
        }
    }

    pub fn move_cursor_to_start(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_to_end(&mut self) {
        self.cursor_pos = self.content.chars().count();
        self.adjust_scroll_offset();
    }

    pub fn clear(&mut self) {
        self.content.clear();
        self.cursor_pos = 0;
        self.scroll_offset = 0;
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn cursor_position(&self) -> usize {
        self.cursor_pos
    }

    /// Get the height needed for the input area (up to max_lines)
    pub fn get_display_height(&self) -> usize {
        let total_lines = self.get_wrapped_lines().len();
        total_lines.min(self.max_lines)
    }

    /// Get the lines to display (considering scroll offset)
    pub fn get_display_lines(&self) -> Vec<String> {
        let lines = self.get_wrapped_lines();
        let total_lines = lines.len();

        if total_lines <= self.max_lines {
            lines
        } else {
            let start = self.scroll_offset;
            let end = (start + self.max_lines).min(total_lines);
            lines[start..end].to_vec()
        }
    }

    /// Get cursor position within the display area (row, col)
    pub fn get_display_cursor_pos(&self) -> (usize, usize) {
        let lines = self.get_wrapped_lines();
        let (line_idx, col) = self.get_cursor_line_col(&lines);

        // Adjust for scroll offset
        let display_line = if line_idx >= self.scroll_offset {
            line_idx - self.scroll_offset
        } else {
            0
        };

        (display_line, col)
    }

    /// Convert character position to byte position for String operations
    fn char_pos_to_byte_pos(&self, char_pos: usize) -> usize {
        self.content
            .char_indices()
            .nth(char_pos)
            .map(|(byte_pos, _)| byte_pos)
            .unwrap_or(self.content.len())
    }

    /// Break content into wrapped lines based on terminal width
    fn get_wrapped_lines(&self) -> Vec<String> {
        if self.content.is_empty() {
            return vec![String::new()];
        }

        let mut lines = Vec::new();
        let content_width = (self.terminal_width as usize).saturating_sub(2); // Account for "> "

        for paragraph in self.content.split('\n') {
            if paragraph.is_empty() {
                lines.push(String::new());
                continue;
            }

            let mut current_line = String::new();
            for ch in paragraph.chars() {
                if current_line.chars().count() >= content_width {
                    lines.push(current_line);
                    current_line = String::new();
                }
                current_line.push(ch);
            }
            lines.push(current_line);
        }

        if lines.is_empty() {
            lines.push(String::new());
        }

        lines
    }

    /// Get which line and column the cursor is on
    fn get_cursor_line_col(&self, _lines: &[String]) -> (usize, usize) {
        // Convert cursor position to line/col by walking through the original content
        let chars: Vec<char> = self.content.chars().collect();
        let mut current_line = 0;
        let mut current_col = 0;

        for (i, &ch) in chars.iter().enumerate() {
            if i == self.cursor_pos {
                return (current_line, current_col);
            }

            if ch == '\n' {
                current_line += 1;
                current_col = 0;
            } else {
                current_col += 1;
                // Handle line wrapping
                let content_width = (self.terminal_width as usize).saturating_sub(2);
                if current_col >= content_width {
                    current_line += 1;
                    current_col = 0;
                }
            }
        }

        // Cursor is at the end
        (current_line, current_col)
    }

    /// Convert line and column position back to cursor position
    fn line_col_to_cursor_pos(&self, _lines: &[String], target_line: usize, target_col: usize) -> usize {
        let chars: Vec<char> = self.content.chars().collect();
        let mut current_line = 0;
        let mut current_col = 0;

        for (i, &ch) in chars.iter().enumerate() {
            if current_line == target_line && current_col == target_col {
                return i;
            }

            if current_line > target_line {
                return i;
            }

            if ch == '\n' {
                current_line += 1;
                current_col = 0;
            } else {
                current_col += 1;
                // Handle line wrapping
                let content_width = (self.terminal_width as usize).saturating_sub(2);
                if current_col >= content_width {
                    current_line += 1;
                    current_col = 0;
                }
            }
        }

        // Return end position if target is beyond content
        chars.len()
    }

    /// Adjust scroll offset to keep cursor visible
    fn adjust_scroll_offset(&mut self) {
        let lines = self.get_wrapped_lines();
        let total_lines = lines.len();
        let (cursor_line, _) = self.get_cursor_line_col(&lines);

        if total_lines <= self.max_lines {
            self.scroll_offset = 0;
            return;
        }

        // If cursor is above visible area, scroll up
        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        }

        // If cursor is below visible area, scroll down
        if cursor_line >= self.scroll_offset + self.max_lines {
            self.scroll_offset = cursor_line - self.max_lines + 1;
        }

        // Ensure scroll offset doesn't go beyond bounds
        self.scroll_offset = self.scroll_offset.min(total_lines.saturating_sub(self.max_lines));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_area_basic_operations() {
        let mut input = InputArea::new(80);

        // Test initial state
        assert_eq!(input.content(), "");
        assert_eq!(input.cursor_position(), 0);

        // Test inserting characters
        input.insert_char('h');
        input.insert_char('e');
        input.insert_char('l');
        input.insert_char('l');
        input.insert_char('o');

        assert_eq!(input.content(), "hello");
        assert_eq!(input.cursor_position(), 5);

        // Test cursor movement
        input.move_cursor_left();
        input.move_cursor_left();
        assert_eq!(input.cursor_position(), 3);

        // Test inserting at cursor position
        input.insert_char('X');
        assert_eq!(input.content(), "helXlo");
        assert_eq!(input.cursor_position(), 4);

        // Test backspace
        input.backspace();
        assert_eq!(input.content(), "hello");
        assert_eq!(input.cursor_position(), 3);

        // Test delete
        input.delete_char();
        assert_eq!(input.content(), "helo");
        assert_eq!(input.cursor_position(), 3);

        // Test clear
        input.clear();
        assert_eq!(input.content(), "");
        assert_eq!(input.cursor_position(), 0);
    }

    #[test]
    fn test_cursor_boundaries() {
        let mut input = InputArea::new(80);

        // Test moving left at start
        input.move_cursor_left();
        assert_eq!(input.cursor_position(), 0);

        // Add some content
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');

        // Test moving right past end
        input.move_cursor_right();
        assert_eq!(input.cursor_position(), 3); // Should not go past end

        // Test home and end
        input.move_cursor_to_start();
        assert_eq!(input.cursor_position(), 0);

        input.move_cursor_to_end();
        assert_eq!(input.cursor_position(), 3);
    }

    #[test]
    fn test_unicode_handling() {
        let mut input = InputArea::new(80);

        // Test with Unicode characters
        input.insert_char('h');
        input.insert_char('ö');
        input.insert_char('l');
        input.insert_char('l');
        input.insert_char('ö');

        assert_eq!(input.content(), "höllö");
        assert_eq!(input.cursor_position(), 5);

        // Test cursor movement with Unicode
        input.move_cursor_left();
        input.move_cursor_left();
        assert_eq!(input.cursor_position(), 3);

        // Test inserting at cursor position with Unicode
        input.insert_char('X');
        assert_eq!(input.content(), "hölXlö");
        assert_eq!(input.cursor_position(), 4);

        // Test backspace with Unicode
        input.backspace();
        assert_eq!(input.content(), "höllö");
        assert_eq!(input.cursor_position(), 3);

        // Test delete with Unicode
        input.delete_char();
        assert_eq!(input.content(), "hölö");
        assert_eq!(input.cursor_position(), 3);
    }

    #[test]
    fn test_multiline_functionality() {
        let mut input = InputArea::new(20);

        // Test newlines
        input.insert_char('H');
        input.insert_char('i');
        input.insert_newline();
        input.insert_char('t');
        input.insert_char('h');
        input.insert_char('e');
        input.insert_char('r');
        input.insert_char('e');

        assert_eq!(input.content(), "Hi\nthere");
        assert_eq!(input.cursor_position(), 8);

        // Test line wrapping
        let lines = input.get_wrapped_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Hi");
        assert_eq!(lines[1], "there");

        // Test display height
        assert_eq!(input.get_display_height(), 2);

        // Test cursor position after up movement
        let (line, col) = input.get_cursor_line_col(&lines);
        assert_eq!(line, 1); // Should be on second line
        assert_eq!(col, 5); // At end of "there"

        input.move_cursor_up();
        let new_pos = input.cursor_position();
        assert_eq!(new_pos, 2); // Should be at end of first line "Hi"
    }
}
