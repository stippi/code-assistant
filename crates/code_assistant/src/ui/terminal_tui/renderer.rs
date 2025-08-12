use crossterm::{
    cursor::{MoveTo, Show},
    execute,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType},
};
use std::io::{stdout, Stdout, Write};
use std::sync::{Arc, Mutex};

struct Inner {
    stdout: Stdout,
    cols: u16,
    rows: u16,
    input_height: u16,
}

/// Minimal terminal renderer that reserves a fixed input region at the bottom
/// using the terminal scroll region and prints messages to the scrollable area
/// so native terminal scrollback works.
#[derive(Clone)]
pub struct TerminalRenderer {
    inner: Arc<Mutex<Inner>>,
}

impl TerminalRenderer {
    /// Create a new renderer, enabling raw mode and setting the scroll region.
    pub fn new(input_height: u16) -> std::io::Result<Arc<Self>> {
        enable_raw_mode()?;
        let (cols, rows) = size()?;
        let mut out = stdout();
        // Reserve bottom input_height rows for the input area using DECSTBM
        Self::set_scroll_region_internal(&mut out, rows.saturating_sub(input_height))?;
        Ok(Arc::new(Self {
            inner: Arc::new(Mutex::new(Inner {
                stdout: out,
                cols,
                rows,
                input_height,
            })),
        }))
    }

    /// Write a message to the scrollable region.
    /// This will move the cursor to the last line of the scroll region then print the text.
    pub fn write_message(&self, text: &str) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let bottom_scroll = inner.rows.saturating_sub(inner.input_height);
        // Move to the bottom line of the scroll region, column 0 and print
        execute!(
            &mut inner.stdout,
            MoveTo(0, bottom_scroll.saturating_sub(1)),
            Print(text)
        )?;
        inner.stdout.flush()
    }

    /// Redraw the bottom input area with the given prompt and content.
    /// Content is rendered as a single wrapped paragraph across input_height lines for simplicity.
    pub fn redraw_input(&self, prompt: &str, content: &str, cursor_col: u16) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let start_row = inner.rows.saturating_sub(inner.input_height);

        // Clear input area
        for i in 0..inner.input_height {
            execute!(&mut inner.stdout, MoveTo(0, start_row + i), Clear(ClearType::CurrentLine))?;
        }

        // Render prompt and content on the first line (simple variant)
        let mut line = String::new();
        line.push_str(prompt);
        line.push_str(content);
        // Trim to width for now; future: wrap across lines if needed
        let display = if (line.len() as u16) > inner.cols {
            let max = inner.cols as usize;
            line.chars().take(max).collect::<String>()
        } else {
            line
        };

        execute!(&mut inner.stdout, MoveTo(0, start_row), Print(display))?;
        // Reposition cursor
        let cursor_x = prompt.len() as u16 + cursor_col;
        let cursor_x = cursor_x.min(inner.cols.saturating_sub(1));
        execute!(&mut inner.stdout, MoveTo(cursor_x, start_row), Show)?;
        inner.stdout.flush()
    }

    /// Handle terminal resize: reset scroll region and redraw input.
    pub fn handle_resize(&self, new_cols: u16, new_rows: u16, prompt: &str, content: &str, cursor_col: u16) -> std::io::Result<()> {
        let (cols, rows, input_height);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.cols = new_cols;
            inner.rows = new_rows;
            cols = inner.cols;
            rows = inner.rows;
            input_height = inner.input_height;
            // Reset and set new scroll region for updated dimensions
            let rows = inner.rows;
            let input_height = inner.input_height;
            Self::reset_scroll_region_internal(&mut inner.stdout)?;
            drop(inner);
            // After releasing, re-lock to set region with immutable borrow of values
            let mut inner2 = self.inner.lock().unwrap();
            Self::set_scroll_region_internal(&mut inner2.stdout, rows.saturating_sub(input_height))?;
        }
        // Redraw input after releasing lock
        self.redraw_input(prompt, content, cursor_col)
    }

    /// Reset scroll region and disable raw mode. Call on exit.
    pub fn teardown(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        Self::reset_scroll_region_internal(&mut inner.stdout)?;
        inner.stdout.flush()?;
        disable_raw_mode()
    }

    fn set_scroll_region_internal(out: &mut Stdout, bottom: u16) -> std::io::Result<()> {
        // ESC[{top};{bottom}r with top=1
        if bottom >= 1 {
            write!(out, "\x1b[1;{}r", bottom)?;
        }
        Ok(())
    }

    fn reset_scroll_region_internal(out: &mut Stdout) -> std::io::Result<()> {
        // ESC[r]
        write!(out, "\x1b[r")?;
        Ok(())
    }
}
