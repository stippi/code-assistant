use crossterm::{
    cursor::{MoveTo, Show},
    execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType},
};
use std::io::{stdout, Stdout, Write};
use std::sync::{Arc, Mutex};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::input_area::InputArea;

struct Inner {
    stdout: Stdout,
    cols: u16,
    rows: u16,
    content_cursor_col: u16, // Track cursor position within content region
}

/// Enhanced terminal renderer with scroll region management and content streaming
#[derive(Clone)]
pub struct TerminalRenderer {
    inner: Arc<Mutex<Inner>>,
}

impl TerminalRenderer {
    /// Create a new renderer, enabling raw mode
    pub fn new() -> std::io::Result<Arc<Self>> {
        enable_raw_mode()?;
        let (cols, rows) = size()?;
        Ok(Arc::new(Self {
            inner: Arc::new(Mutex::new(Inner {
                stdout: stdout(),
                cols,
                rows,
                content_cursor_col: 0,
            })),
        }))
    }

    /// Apply a scroll region so that only the content area scrolls.
    /// The input area at the bottom remains fixed.
    pub fn set_input_height(&self, input_height: usize) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let content_bottom = inner.rows.saturating_sub(input_height as u16);
        let bottom = if content_bottom >= 1 {
            content_bottom
        } else {
            1
        };
        // ESC[{top};{bottom}r  -> set top/bottom margins (scroll region)
        write!(&mut inner.stdout, "\x1b[1;{bottom}r")?;
        inner.stdout.flush()
    }

    /// Reset scroll region to the full screen
    fn reset_scroll_region(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        write!(&mut inner.stdout, "\x1b[r")?;
        inner.stdout.flush()
    }

    /// Append a chunk of content at the virtual content cursor inside the scroll region.
    pub fn append_content_chunk(&self, chunk: &str, input_area: &InputArea) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();

        // Ensure scroll region is set so only content area scrolls
        let display_height = input_area.get_display_height();
        let content_bottom = inner.rows.saturating_sub(display_height as u16);
        let content_bottom_1based = content_bottom.max(1);
        let content_bottom_y = content_bottom_1based - 1; // convert to 0-based

        // Store values to avoid borrowing issues
        let cursor_col = inner.content_cursor_col;
        let width = inner.cols as usize;

        // Move to current content cursor position on the bottom line of the content region
        execute!(
            &mut inner.stdout,
            MoveTo(cursor_col, content_bottom_y),
            Print(chunk)
        )?;

        // Update virtual content cursor column by simulating wrapping and newlines
        let mut col = cursor_col as usize;
        for ch in chunk.chars() {
            if ch == '\n' {
                col = 0; // newline moves to next line at column 0
            } else {
                // Handle unicode display width (e.g., CJK or emoji width 2)
                let w = UnicodeWidthChar::width(ch).unwrap_or(1);
                col += w;
                if col >= width {
                    col = 0; // terminal wraps to next line
                }
            }
        }
        // Clamp to available width to avoid out-of-bounds MoveTo
        if col >= width {
            col = width.saturating_sub(1);
        }
        inner.content_cursor_col = col as u16;

        inner.stdout.flush()
    }

    /// Write a message to the scrollable region (legacy method for compatibility).
    /// This will move the cursor to the last line of the scroll region then print the text.
    pub fn write_message(&self, text: &str) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        // For simplicity, assume input height of 3 for legacy calls
        let bottom_scroll = inner.rows.saturating_sub(3);
        // Move to the bottom line of the scroll region, column 0 and print
        execute!(
            &mut inner.stdout,
            MoveTo(0, bottom_scroll.saturating_sub(1)),
            Print(text)
        )?;
        inner.stdout.flush()
    }

    /// Redraw the input area with multi-line support
    pub fn redraw_input(&self, prompt: &str, input_area: &InputArea) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();

        let display_height = input_area.get_display_height();
        let start_row = inner.rows.saturating_sub(display_height as u16);
        let display_lines = input_area.get_display_lines();
        let (cursor_row, cursor_col) = input_area.get_display_cursor_pos();

        // Clear all input area lines
        for i in 0..display_height {
            execute!(
                &mut inner.stdout,
                MoveTo(0, start_row + i as u16),
                Clear(ClearType::CurrentLine)
            )?;
        }

        // Render each line with prompt
        for (i, line) in display_lines.iter().enumerate() {
            execute!(
                &mut inner.stdout,
                MoveTo(0, start_row + i as u16),
                SetForegroundColor(Color::Green),
                Print(prompt),
                ResetColor,
                Print(line)
            )?;
        }

        // Position cursor correctly (physical cursor stays in input area)
        let cursor_terminal_row = start_row + cursor_row as u16;
        let cursor_terminal_col = UnicodeWidthStr::width(prompt) as u16 + cursor_col as u16;
        execute!(
            &mut inner.stdout,
            MoveTo(cursor_terminal_col, cursor_terminal_row),
            Show
        )?;

        inner.stdout.flush()
    }

    /// Handle terminal resize: update dimensions and reset scroll region
    pub fn handle_resize(
        &self,
        new_cols: u16,
        new_rows: u16,
        input_area: &InputArea,
    ) -> std::io::Result<()> {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.cols = new_cols;
            inner.rows = new_rows;
            // Clamp content cursor within new width
            if inner.content_cursor_col >= inner.cols.saturating_sub(1) {
                inner.content_cursor_col = inner.cols.saturating_sub(1);
            }
        }

        // Set scroll region for new dimensions
        self.set_input_height(input_area.get_display_height())?;

        Ok(())
    }

    /// Apply overlay: temporarily reduce content region and render overlay lines
    pub fn apply_overlay(
        &self,
        overlay_height: u16,
        overlay_lines: &[String],
        input_area: &InputArea,
    ) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();

        let input_height = input_area.get_display_height() as u16;
        let content_bottom = inner.rows.saturating_sub(input_height + overlay_height);
        let bottom = if content_bottom >= 1 {
            content_bottom
        } else {
            1
        };

        // Set smaller scroll region
        write!(&mut inner.stdout, "\x1b[1;{bottom}r")?;

        // Render overlay lines just above the input area
        let overlay_start_row = inner.rows.saturating_sub(input_height + overlay_height);
        for (i, line) in overlay_lines.iter().enumerate() {
            execute!(
                &mut inner.stdout,
                MoveTo(0, overlay_start_row + i as u16),
                Clear(ClearType::CurrentLine),
                Print(line)
            )?;
        }

        inner.stdout.flush()
    }

    /// Clear overlay: restore full content region
    pub fn clear_overlay(
        &self,
        overlay_height: u16,
        input_area: &InputArea,
    ) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();

        let input_height = input_area.get_display_height() as u16;

        // Clear overlay lines
        let overlay_start_row = inner.rows.saturating_sub(input_height + overlay_height);
        for i in 0..overlay_height {
            execute!(
                &mut inner.stdout,
                MoveTo(0, overlay_start_row + i),
                Clear(ClearType::CurrentLine)
            )?;
        }

        // Restore full content region
        let content_bottom = inner.rows.saturating_sub(input_height);
        let bottom = if content_bottom >= 1 {
            content_bottom
        } else {
            1
        };
        write!(&mut inner.stdout, "\x1b[1;{bottom}r")?;

        inner.stdout.flush()
    }

    /// Reset scroll region and disable raw mode. Call on exit.
    pub fn teardown(&self) -> std::io::Result<()> {
        self.reset_scroll_region()?;
        disable_raw_mode()
    }

    /// Get current terminal dimensions
    pub fn get_size(&self) -> (u16, u16) {
        let inner = self.inner.lock().unwrap();
        (inner.cols, inner.rows)
    }
}
