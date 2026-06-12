// Tui orchestration layer.
// Adapted from codex-rs (https://github.com/openai/codex) under the Apache License 2.0.
//
// Wraps the custom terminal and provides atomic draw cycles via SynchronizedUpdate.
// All viewport management, history insertion, and rendering happen inside a single
// synchronized update block to prevent flicker.

use std::io;
use std::io::stdout;
use std::io::Stdout;
use std::panic;

use crossterm::event::EnableBracketedPaste;
use crossterm::SynchronizedUpdate;
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::disable_raw_mode;
use ratatui::crossterm::terminal::enable_raw_mode;
use ratatui::layout::Offset;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::text::Line;

use super::custom_terminal;
use super::custom_terminal::Terminal as CustomTerminal;

/// Type alias for the terminal type used in this application.
pub type Terminal = CustomTerminal<CrosstermBackend<Stdout>>;

/// Initialize the terminal (inline viewport; history stays in normal scrollback).
pub fn init() -> io::Result<Tui> {
    // Query the terminal background color before entering raw mode.
    // Uses OSC 11 to detect the actual bg color for composer overlay blending.
    super::terminal_color::init();

    // Initialize tool renderer registry for custom tool block display.
    super::tool_renderers::init_registry();

    enable_raw_mode()?;
    let _ = execute!(stdout(), EnableBracketedPaste);

    set_panic_hook();

    let backend = CrosstermBackend::new(stdout());
    let terminal = CustomTerminal::with_options(backend)?;
    Ok(Tui::new(terminal))
}

/// Restore terminal state.
pub fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    Ok(())
}

fn set_panic_hook() {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = restore();
        hook(panic_info);
    }));
}

/// The Tui struct orchestrates all terminal operations. Its `draw()` method wraps
/// viewport management, history insertion, and widget rendering in a single
/// `SynchronizedUpdate` block for flicker-free output.
pub struct Tui {
    pub terminal: Terminal,
    pending_history_lines: Vec<Line<'static>>,
}

impl Tui {
    pub fn new(terminal: Terminal) -> Self {
        Self {
            terminal,
            pending_history_lines: vec![],
        }
    }

    /// Buffer history lines for insertion in the next `draw()` call.
    /// Lines are not written to the terminal immediately -- they are inserted
    /// atomically together with the viewport rendering inside `draw()`.
    pub fn insert_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.pending_history_lines.extend(lines);
    }

    /// Draw a frame to the terminal. All operations happen inside a single
    /// `SynchronizedUpdate` block:
    /// 1. Handle terminal resize via cursor position heuristic
    /// 2. Expand viewport if needed (scroll content above viewport up)
    /// 3. Insert any pending history lines into scrollback above viewport
    /// 4. Render the frame via double-buffered diff rendering
    pub fn draw(
        &mut self,
        height: u16,
        draw_fn: impl FnOnce(&mut custom_terminal::Frame),
    ) -> io::Result<()> {
        // Precompute viewport adjustments before entering the synchronized update,
        // to avoid racing with the event reader on cursor position queries.
        let mut pending_viewport_area = self.pending_viewport_area()?;

        stdout().sync_update(|_| {
            let terminal = &mut self.terminal;
            if let Some(new_area) = pending_viewport_area.take() {
                terminal.set_viewport_area(new_area);
                terminal.clear()?;
            }

            let size = terminal.size()?;

            let mut area = terminal.viewport_area;
            area.height = height.min(size.height);
            area.width = size.width;
            // If the viewport has expanded past the bottom of the screen,
            // scroll everything above the viewport up to make room.
            if area.bottom() > size.height {
                terminal
                    .backend_mut()
                    .scroll_region_up(0..area.top(), area.bottom() - size.height)?;
                area.y = size.height - area.height;
            }
            if area != terminal.viewport_area {
                terminal.clear()?;
                terminal.set_viewport_area(area);
            }

            if !self.pending_history_lines.is_empty() {
                super::history_insert::insert_history_lines(
                    terminal,
                    std::mem::take(&mut self.pending_history_lines),
                )?;
            }

            terminal.draw(|frame| {
                draw_fn(frame);
            })
        })?
    }

    /// Detect terminal resize by comparing current screen size with last known size.
    /// If the cursor moved (e.g., terminal reflowed text), adjust the viewport offset.
    fn pending_viewport_area(&mut self) -> io::Result<Option<Rect>> {
        let terminal = &mut self.terminal;
        let screen_size = terminal.size()?;
        let last_known_screen_size = terminal.last_known_screen_size;
        if screen_size != last_known_screen_size {
            if let Ok(cursor_pos) = terminal.get_cursor_position() {
                let last_known_cursor_pos = terminal.last_known_cursor_pos;
                if cursor_pos.y != last_known_cursor_pos.y {
                    let offset = Offset {
                        x: 0,
                        y: cursor_pos.y as i32 - last_known_cursor_pos.y as i32,
                    };
                    return Ok(Some(terminal.viewport_area.offset(offset)));
                }
            }
        }
        Ok(None)
    }

    /// Get the current terminal screen size.
    pub fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }
}
