//! Terminal crate — a thin integration layer between `alacritty_terminal` and GPUI.
//!
//! This crate provides a `Terminal` GPUI entity that wraps an Alacritty terminal
//! emulator, manages PTY I/O, and produces content snapshots for rendering.
//!
//! # Architecture
//!
//! ```text
//! TerminalBuilder::new()
//!   → tty::new()          // OS pseudo-terminal
//!   → Term::new()         // Alacritty emulator state machine
//!   → EventLoop::new()    // Background I/O thread
//!   → Terminal entity      // GPUI entity that owns everything
//! ```
//!
//! The `Terminal` entity processes events from the Alacritty event loop and
//! maintains a `TerminalContent` snapshot that can be read for rendering.

use std::collections::VecDeque;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event::{Event as AlacTermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as AlacPoint};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config as TermConfig, RenderableCursor, TermMode};
use alacritty_terminal::vte::ansi::{ClearMode, Handler};
use alacritty_terminal::{tty, Term};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use gpui::{px, Bounds, Context, EventEmitter, Pixels, Point, Size, Task};

// Re-export types needed by terminal_view for rendering
pub use alacritty_terminal::grid::Dimensions as AlacDimensions;
pub use alacritty_terminal::index::{Column as AlacColumn, Line as AlacLine, Point as GridPoint};
pub use alacritty_terminal::term::cell::{Cell as AlacCell, Flags as AlacCellFlags};
pub use alacritty_terminal::term::{RenderableCursor as AlacCursor, TermMode as AlacTermMode};
pub use alacritty_terminal::vte::ansi::CursorShape as AlacCursorShape;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_SCROLL_HISTORY_LINES: usize = 10_000;
pub const MAX_SCROLL_HISTORY_LINES: usize = 100_000;

/// Default dimensions used before the first real layout.
/// These are intentionally small so that embedded terminals (inline mode)
/// start with a tiny grid that grows as content arrives, matching Zed's
/// approach (where defaults are 5px line height / 30px height = 6 rows).
const DEBUG_CELL_WIDTH: Pixels = px(5.0);
const DEBUG_LINE_HEIGHT: Pixels = px(5.0);
const DEBUG_TERMINAL_WIDTH: Pixels = px(500.0);
const DEBUG_TERMINAL_HEIGHT: Pixels = px(30.0);

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Events emitted by the `Terminal` entity.
#[derive(Debug, Clone)]
pub enum Event {
    /// The terminal content has changed and should be re-rendered.
    Wakeup,
    /// The terminal title changed.
    TitleChanged(String),
    /// The child process exited with the given status code.
    /// `None` means the process was killed or we couldn't get the code.
    ChildExit(Option<i32>),
    /// Bell character received.
    Bell,
}

// ---------------------------------------------------------------------------
// TerminalBounds — maps pixel dimensions to alacritty grid dimensions
// ---------------------------------------------------------------------------

/// Describes the terminal's pixel bounds and cell metrics.
/// Implements `alacritty_terminal::grid::Dimensions` so it can be used
/// directly with `Term::new()` and `Term::resize()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalBounds {
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub bounds: Bounds<Pixels>,
}

impl TerminalBounds {
    pub fn new(line_height: Pixels, cell_width: Pixels, bounds: Bounds<Pixels>) -> Self {
        Self {
            cell_width,
            line_height,
            bounds,
        }
    }

    pub fn num_lines(&self) -> usize {
        (self.bounds.size.height / self.line_height).floor() as usize
    }

    pub fn num_columns(&self) -> usize {
        (self.bounds.size.width / self.cell_width).floor() as usize
    }

    pub fn height(&self) -> Pixels {
        self.bounds.size.height
    }

    pub fn width(&self) -> Pixels {
        self.bounds.size.width
    }
}

impl Default for TerminalBounds {
    fn default() -> Self {
        Self::new(
            DEBUG_LINE_HEIGHT,
            DEBUG_CELL_WIDTH,
            Bounds {
                origin: Point::default(),
                size: Size {
                    width: DEBUG_TERMINAL_WIDTH,
                    height: DEBUG_TERMINAL_HEIGHT,
                },
            },
        )
    }
}

impl Dimensions for TerminalBounds {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        self.num_lines().max(1)
    }

    fn columns(&self) -> usize {
        self.num_columns().max(1)
    }
}

impl From<TerminalBounds> for WindowSize {
    fn from(val: TerminalBounds) -> Self {
        WindowSize {
            num_lines: val.num_lines().max(1) as u16,
            num_cols: val.num_columns().max(1) as u16,
            cell_width: f32::from(val.cell_width) as u16,
            cell_height: f32::from(val.line_height) as u16,
        }
    }
}

// ---------------------------------------------------------------------------
// Listener — bridges alacritty events to an async channel
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Listener(UnboundedSender<AlacTermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: AlacTermEvent) {
        self.0.unbounded_send(event).ok();
    }
}

// ---------------------------------------------------------------------------
// IndexedCell — a cell at a grid position
// ---------------------------------------------------------------------------

/// A single cell from the terminal grid, together with its position.
#[derive(Debug, Clone)]
pub struct IndexedCell {
    pub point: AlacPoint,
    pub cell: Cell,
}

impl Deref for IndexedCell {
    type Target = Cell;
    fn deref(&self) -> &Cell {
        &self.cell
    }
}

// ---------------------------------------------------------------------------
// TerminalContent — a snapshot of the terminal state for rendering
// ---------------------------------------------------------------------------

/// An immutable snapshot of the terminal grid, suitable for rendering.
pub struct TerminalContent {
    pub cells: Vec<IndexedCell>,
    pub mode: TermMode,
    pub display_offset: usize,
    pub cursor: RenderableCursor,
    pub cursor_char: char,
    pub terminal_bounds: TerminalBounds,
    /// Raw content line count: scrollback + cursor_line + 1.
    /// Always includes the cursor's line, which may be empty after a
    /// trailing newline.  See `Terminal::content_lines()` which trims
    /// that extra line once the process has exited.
    pub raw_content_lines: usize,
    /// Whether the grid cursor sits at column 0 (i.e. on a fresh empty
    /// line after a trailing newline).
    pub cursor_at_line_start: bool,
}

impl Default for TerminalContent {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            mode: TermMode::empty(),
            display_offset: 0,
            cursor: RenderableCursor {
                point: AlacPoint::new(Line(0), Column(0)),
                shape: alacritty_terminal::vte::ansi::CursorShape::Block,
            },
            cursor_char: ' ',
            terminal_bounds: TerminalBounds::default(),
            raw_content_lines: 1,
            cursor_at_line_start: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal events queued for processing during sync()
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
enum InternalEvent {
    Resize(TerminalBounds),
    Scroll(alacritty_terminal::grid::Scroll),
    Clear,
}

// ---------------------------------------------------------------------------
// Terminal — the main GPUI entity
// ---------------------------------------------------------------------------

enum TerminalType {
    Pty { pty_tx: Notifier },
    DisplayOnly,
}

/// A terminal backed by the Alacritty terminal emulator.
///
/// This is a GPUI entity that manages:
/// - The Alacritty `Term` state machine
/// - PTY I/O (or display-only mode)
/// - A content snapshot for rendering
pub struct Terminal {
    terminal_type: TerminalType,
    term: Arc<FairMutex<Term<Listener>>>,
    events: VecDeque<InternalEvent>,
    /// The most recent content snapshot, updated by `sync()`.
    pub last_content: TerminalContent,
    /// When the terminal was created.
    started_at: Instant,
    /// The command being executed (if any).
    command: Option<String>,
    /// Exit status of the child process, set when it exits.
    exit_status: Option<Option<i32>>,
    /// Background task for the event loop subscription.
    _event_loop_task: Option<Task<()>>,
}

impl EventEmitter<Event> for Terminal {}

impl Terminal {
    // -- Accessors --

    /// Total lines in the terminal (scrollback + visible).
    pub fn total_lines(&self) -> usize {
        self.term.lock_unfair().total_lines()
    }

    /// Number of visible screen lines.
    pub fn screen_lines(&self) -> usize {
        self.term.lock_unfair().screen_lines()
    }

    /// Number of lines that actually have content.  Unlike `total_lines()`
    /// this excludes empty grid rows below the cursor, so embedded
    /// terminals don't show trailing blank space.
    ///
    /// While the process is **running**, returns `cursor_line + 1 +
    /// scrollback` which is stable (monotonically increasing) and avoids
    /// frame-to-frame height oscillation.
    ///
    /// Once the process has **exited** and the cursor sits at column 0
    /// (typical after a trailing newline), the empty cursor line is
    /// trimmed so the final card has no blank row at the bottom.
    pub fn content_lines(&self) -> usize {
        let raw = self.last_content.raw_content_lines;
        if self.has_exited() && self.last_content.cursor_at_line_start && raw > 1 {
            raw - 1
        } else {
            raw
        }
    }

    /// When this terminal was created.
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// The command being executed, if any.
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    /// The exit status, if the child has exited.
    /// `Some(None)` = exited but status unknown, `Some(Some(code))` = exited with code.
    pub fn exit_status(&self) -> Option<Option<i32>> {
        self.exit_status
    }

    /// Whether the child process has exited.
    pub fn has_exited(&self) -> bool {
        self.exit_status.is_some()
    }

    /// Get the full terminal text content as a string.
    pub fn get_content_text(&self) -> String {
        let term = self.term.lock_unfair();
        let start = AlacPoint::new(term.topmost_line(), Column(0));
        let end = AlacPoint::new(term.bottommost_line(), term.last_column());
        term.bounds_to_string(start, end)
    }

    // -- Mutations --

    /// Queue a resize event. The actual resize happens during `sync()`.
    pub fn set_size(&mut self, new_bounds: TerminalBounds) {
        if self.last_content.terminal_bounds != new_bounds {
            self.events.push_back(InternalEvent::Resize(new_bounds));
        }
    }

    /// Write bytes to the PTY (no-op in display-only mode).
    pub fn write_to_pty(&self, input: impl Into<std::borrow::Cow<'static, [u8]>>) {
        if let TerminalType::Pty { pty_tx, .. } = &self.terminal_type {
            pty_tx.notify(input.into());
        }
    }

    /// Inject output directly into the terminal emulator (display-only mode).
    /// Also works in PTY mode, but typically used for display-only terminals.
    pub fn write_output(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        // Convert bare LF to CRLF for proper line wrapping without a PTY.
        let mut converted = Vec::with_capacity(bytes.len());
        for &b in bytes {
            if b == b'\n' {
                converted.push(b'\r');
            }
            converted.push(b);
        }

        let mut processor = alacritty_terminal::vte::ansi::Processor::<
            alacritty_terminal::vte::ansi::StdSyncHandler,
        >::new();

        {
            let mut term = self.term.lock();
            processor.advance(&mut *term, &converted);
        }

        cx.emit(Event::Wakeup);
        cx.notify();
    }

    /// Synchronize the terminal state: process queued events and take a fresh
    /// content snapshot. This should be called during layout/prepaint.
    ///
    /// When `scroll_to_top` is true the viewport is scrolled to the topmost
    /// content *under the same lock* before taking the snapshot.  This is
    /// used by inline/embedded terminals so that any scrollback lines
    /// (which are counted in `total_lines` for height sizing) are included
    /// in the rendered cells.  Doing it under a single lock prevents a PTY
    /// write race between resize and snapshot.
    pub fn sync(&mut self, scroll_to_top: bool, cx: &mut Context<Self>) {
        let term = self.term.clone();
        let mut terminal = term.lock_unfair();

        while let Some(event) = self.events.pop_front() {
            self.process_internal_event(&event, &mut terminal, cx);
        }

        if scroll_to_top {
            terminal.scroll_display(alacritty_terminal::grid::Scroll::Top);
        }

        self.last_content = Self::make_content(&terminal, &self.last_content);
    }

    // -- Event processing --

    fn process_event(&mut self, event: AlacTermEvent, cx: &mut Context<Self>) {
        match event {
            AlacTermEvent::Wakeup => {
                cx.emit(Event::Wakeup);
                cx.notify();
            }
            AlacTermEvent::Title(title) => {
                cx.emit(Event::TitleChanged(title));
            }
            AlacTermEvent::Bell => {
                cx.emit(Event::Bell);
            }
            AlacTermEvent::ChildExit(raw_status) => {
                self.exit_status = Some(Some(raw_status));
                cx.emit(Event::ChildExit(Some(raw_status)));
                cx.notify();
            }
            AlacTermEvent::Exit => {
                if self.exit_status.is_none() {
                    self.exit_status = Some(None);
                    cx.emit(Event::ChildExit(None));
                    cx.notify();
                }
            }
            AlacTermEvent::ClipboardStore(_, _data) => {
                // TODO: integrate with system clipboard if needed
            }
            AlacTermEvent::ClipboardLoad(_, _format) => {
                // TODO: integrate with system clipboard if needed
            }
            AlacTermEvent::PtyWrite(out) => {
                self.write_to_pty(out.into_bytes());
            }
            AlacTermEvent::ColorRequest(index, format) => {
                let color = self.term.lock().colors()[index]
                    .unwrap_or(alacritty_terminal::vte::ansi::Rgb { r: 0, g: 0, b: 0 });
                self.write_to_pty(format(color).into_bytes());
            }
            AlacTermEvent::TextAreaSizeRequest(format) => {
                let size: WindowSize = self.last_content.terminal_bounds.into();
                self.write_to_pty(format(size).into_bytes());
            }
            AlacTermEvent::CursorBlinkingChange
            | AlacTermEvent::MouseCursorDirty
            | AlacTermEvent::ResetTitle => {}
        }
    }

    fn process_internal_event(
        &mut self,
        event: &InternalEvent,
        term: &mut Term<Listener>,
        cx: &mut Context<Self>,
    ) {
        match event {
            InternalEvent::Resize(new_bounds) => {
                let mut bounds = *new_bounds;
                // Ensure minimum dimensions of one cell
                bounds.bounds.size.height = bounds.height().max(bounds.line_height);
                bounds.bounds.size.width = bounds.width().max(bounds.cell_width);

                self.last_content.terminal_bounds = bounds;

                if let TerminalType::Pty { pty_tx, .. } = &self.terminal_type {
                    pty_tx.0.send(Msg::Resize(bounds.into())).ok();
                }

                term.resize(bounds);
            }
            InternalEvent::Clear => {
                Handler::clear_screen(term, ClearMode::Saved);
                cx.emit(Event::Wakeup);
            }
            InternalEvent::Scroll(scroll) => {
                term.scroll_display(*scroll);
            }
        }
    }

    fn make_content(term: &Term<Listener>, last_content: &TerminalContent) -> TerminalContent {
        let content = term.renderable_content();
        let cells: Vec<IndexedCell> = content
            .display_iter
            .map(|ic| IndexedCell {
                point: ic.point,
                cell: ic.cell.clone(),
            })
            .collect();

        // Snapshot cursor state for content_lines calculation.
        let cursor = term.grid().cursor.point;
        let cursor_line = cursor.line.0.max(0) as usize;
        let scrollback = (-term.topmost_line().0).max(0) as usize;

        TerminalContent {
            cells,
            mode: content.mode,
            display_offset: content.display_offset,
            cursor: content.cursor,
            cursor_char: term.grid()[content.cursor.point].c,
            terminal_bounds: last_content.terminal_bounds,
            raw_content_lines: scrollback + cursor_line + 1,
            cursor_at_line_start: cursor.column.0 == 0,
        }
    }
}

// ---------------------------------------------------------------------------
// TerminalBuilder — factory for creating Terminal entities
// ---------------------------------------------------------------------------

/// Options for creating a new terminal.
#[derive(Default)]
pub struct TerminalOptions {
    /// The command to execute. If `None`, the default shell is used.
    pub command: Option<String>,
    /// Working directory. If `None`, the current directory is used.
    pub working_dir: Option<PathBuf>,
    /// Environment variables to set.
    pub env: Vec<(String, String)>,
    /// Maximum scrollback history lines.
    pub scroll_history: Option<usize>,
}

/// Builder that creates a `Terminal` entity wired to a PTY.
pub struct TerminalBuilder {
    terminal: Terminal,
    events_rx: UnboundedReceiver<AlacTermEvent>,
}

impl TerminalBuilder {
    /// Create a new terminal backed by a real PTY.
    pub fn new(options: TerminalOptions) -> Result<Self> {
        let (events_tx, events_rx) = mpsc::unbounded();

        let scrolling_history = options
            .scroll_history
            .unwrap_or(DEFAULT_SCROLL_HISTORY_LINES)
            .min(MAX_SCROLL_HISTORY_LINES);

        let config = TermConfig {
            scrolling_history,
            ..TermConfig::default()
        };

        // Build the shell command
        let (shell, shell_text) = if let Some(ref cmd) = options.command {
            let shell = tty::Shell::new(
                default_shell_program(),
                vec![shell_command_flag().to_string(), cmd.clone()],
            );
            (Some(shell), Some(cmd.clone()))
        } else {
            (None, None)
        };

        // Disable pagers so commands don't block
        let mut env: Vec<(String, String)> = options.env.clone();
        env.push(("PAGER".into(), String::new()));
        env.push(("GIT_PAGER".into(), "cat".into()));

        let pty_options = tty::Options {
            shell,
            working_directory: options.working_dir.clone(),
            drain_on_exit: true,
            env: env.into_iter().collect(),
            #[cfg(windows)]
            escape_args: true,
        };

        let default_bounds = TerminalBounds::default();
        let pty =
            tty::new(&pty_options, default_bounds.into(), 0).context("Failed to create PTY")?;

        let term = Term::new(config, &default_bounds, Listener(events_tx.clone()));

        let term = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(
            term.clone(),
            Listener(events_tx),
            pty,
            pty_options.drain_on_exit,
            false,
        )
        .context("Failed to create event loop")?;

        let pty_tx = event_loop.channel();
        let _io_thread = event_loop.spawn();

        let terminal = Terminal {
            terminal_type: TerminalType::Pty {
                pty_tx: Notifier(pty_tx),
            },
            term,
            events: VecDeque::new(),
            last_content: TerminalContent::default(),
            started_at: Instant::now(),
            command: shell_text,
            exit_status: None,
            _event_loop_task: None,
        };

        Ok(Self {
            terminal,
            events_rx,
        })
    }

    /// Create a display-only terminal (no PTY). Output is injected via
    /// `Terminal::write_output()`.
    pub fn new_display_only(scroll_history: Option<usize>) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded();

        let scrolling_history = scroll_history
            .unwrap_or(DEFAULT_SCROLL_HISTORY_LINES)
            .min(MAX_SCROLL_HISTORY_LINES);

        let config = TermConfig {
            scrolling_history,
            ..TermConfig::default()
        };

        let term = Term::new(config, &TerminalBounds::default(), Listener(events_tx));

        let term = Arc::new(FairMutex::new(term));

        let terminal = Terminal {
            terminal_type: TerminalType::DisplayOnly,
            term,
            events: VecDeque::new(),
            last_content: TerminalContent::default(),
            started_at: Instant::now(),
            command: None,
            exit_status: None,
            _event_loop_task: None,
        };

        Self {
            terminal,
            events_rx,
        }
    }

    /// Subscribe to the event loop and return the finished `Terminal`.
    /// This must be called within a `Context<Terminal>` (i.e., inside `cx.new()`).
    pub fn subscribe(mut self, cx: &mut Context<Terminal>) -> Terminal {
        let mut terminal = self.terminal;

        let task = cx.spawn(async move |this, cx| {
            while let Some(event) = self.events_rx.next().await {
                let is_wakeup = matches!(event, AlacTermEvent::Wakeup);

                let result = this.update(cx, |terminal, cx| {
                    terminal.process_event(event, cx);
                });

                if result.is_err() {
                    break;
                }

                // Batch: drain any pending events without awaiting
                if is_wakeup {
                    while let Ok(event) = self.events_rx.try_recv() {
                        let result = this.update(cx, |terminal, cx| {
                            terminal.process_event(event, cx);
                        });
                        if result.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        terminal._event_loop_task = Some(task);
        terminal
    }
}

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

fn default_shell_program() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
}

fn shell_command_flag() -> &'static str {
    #[cfg(unix)]
    {
        "-c"
    }
    #[cfg(windows)]
    {
        "/C"
    }
}

// ---------------------------------------------------------------------------
// Color helpers (used by terminal_view for mapping ANSI colors to theme)
// ---------------------------------------------------------------------------

/// Map a 256-color index to an RGB value.
/// Indices 0-15 are the standard ANSI colors (caller should map to theme).
/// Indices 16-231 are the 6x6x6 color cube.
/// Indices 232-255 are the grayscale ramp.
pub fn get_indexed_color_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => {
            // Standard ANSI — return black as placeholder, caller should use theme
            (0, 0, 0)
        }
        16..=231 => {
            // 6x6x6 color cube
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let to_val = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            // Grayscale ramp (24 steps)
            let value = 8 + 10 * (index - 232);
            (value, value, value)
        }
    }
}
