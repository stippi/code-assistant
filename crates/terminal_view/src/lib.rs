//! Terminal view crate — a GPUI Element that renders the terminal grid.
//!
//! This crate provides:
//! - `TerminalElement`: A GPUI `Element` that paints the terminal cell grid with colors
//! - `TerminalView`: A GPUI entity that wraps `TerminalElement` and manages embedded mode
//!
//! # Architecture
//!
//! The rendering pipeline works as follows:
//! 1. `prepaint()`: Read the terminal content snapshot, compute font metrics,
//!    convert cells to `BatchedTextRun`s (styled text) and `LayoutRect`s (backgrounds)
//! 2. `paint()`: Draw backgrounds, then text, then cursor

use gpui::{div, Font, FontStyle, FontWeight, TextRun};
use gpui::{
    fill, point, px, relative, size, App, Bounds, ContentMask, Context, Element, Entity,
    EventEmitter, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, ParentElement,
    Pixels, Point, Render, SharedString, Size, Style, Styled, Window,
};
use terminal::{AlacCell, AlacCellFlags, GridPoint, IndexedCell, Terminal, TerminalBounds};

// Re-exports
pub use terminal;

// ---------------------------------------------------------------------------
// Color conversion — maps ANSI colors to GPUI Hsla
// ---------------------------------------------------------------------------

/// Convert an alacritty color to an GPUI Hsla color.
pub fn convert_color(
    color: &alacritty_terminal::vte::ansi::Color,
    theme_colors: &TerminalThemeColors,
) -> Hsla {
    use alacritty_terminal::vte::ansi::Color;
    use alacritty_terminal::vte::ansi::NamedColor;

    match color {
        Color::Named(named) => match named {
            NamedColor::Black => theme_colors.ansi_black,
            NamedColor::Red => theme_colors.ansi_red,
            NamedColor::Green => theme_colors.ansi_green,
            NamedColor::Yellow => theme_colors.ansi_yellow,
            NamedColor::Blue => theme_colors.ansi_blue,
            NamedColor::Magenta => theme_colors.ansi_magenta,
            NamedColor::Cyan => theme_colors.ansi_cyan,
            NamedColor::White => theme_colors.ansi_white,
            NamedColor::BrightBlack => theme_colors.ansi_bright_black,
            NamedColor::BrightRed => theme_colors.ansi_bright_red,
            NamedColor::BrightGreen => theme_colors.ansi_bright_green,
            NamedColor::BrightYellow => theme_colors.ansi_bright_yellow,
            NamedColor::BrightBlue => theme_colors.ansi_bright_blue,
            NamedColor::BrightMagenta => theme_colors.ansi_bright_magenta,
            NamedColor::BrightCyan => theme_colors.ansi_bright_cyan,
            NamedColor::BrightWhite => theme_colors.ansi_bright_white,
            NamedColor::Foreground => theme_colors.foreground,
            NamedColor::Background => theme_colors.background,
            NamedColor::Cursor => theme_colors.cursor,
            // Dim variants — use the normal color with reduced alpha
            NamedColor::DimBlack => with_dim(theme_colors.ansi_black),
            NamedColor::DimRed => with_dim(theme_colors.ansi_red),
            NamedColor::DimGreen => with_dim(theme_colors.ansi_green),
            NamedColor::DimYellow => with_dim(theme_colors.ansi_yellow),
            NamedColor::DimBlue => with_dim(theme_colors.ansi_blue),
            NamedColor::DimMagenta => with_dim(theme_colors.ansi_magenta),
            NamedColor::DimCyan => with_dim(theme_colors.ansi_cyan),
            NamedColor::DimWhite => with_dim(theme_colors.ansi_white),
            NamedColor::DimForeground => with_dim(theme_colors.foreground),
            NamedColor::BrightForeground => theme_colors.foreground,
        },
        Color::Spec(rgb) => rgba_color(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => {
            match *index {
                // Standard 16 colors
                0 => theme_colors.ansi_black,
                1 => theme_colors.ansi_red,
                2 => theme_colors.ansi_green,
                3 => theme_colors.ansi_yellow,
                4 => theme_colors.ansi_blue,
                5 => theme_colors.ansi_magenta,
                6 => theme_colors.ansi_cyan,
                7 => theme_colors.ansi_white,
                8 => theme_colors.ansi_bright_black,
                9 => theme_colors.ansi_bright_red,
                10 => theme_colors.ansi_bright_green,
                11 => theme_colors.ansi_bright_yellow,
                12 => theme_colors.ansi_bright_blue,
                13 => theme_colors.ansi_bright_magenta,
                14 => theme_colors.ansi_bright_cyan,
                15 => theme_colors.ansi_bright_white,
                // 6x6x6 color cube and grayscale
                16..=255 => {
                    let (r, g, b) = terminal::get_indexed_color_rgb(*index);
                    rgba_color(r, g, b)
                }
            }
        }
    }
}

fn rgba_color(r: u8, g: u8, b: u8) -> Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn with_dim(color: Hsla) -> Hsla {
    let mut c = color;
    c.a *= 0.7;
    c
}

// ---------------------------------------------------------------------------
// TerminalThemeColors — theme color tokens for terminal rendering
// ---------------------------------------------------------------------------

/// Theme colors used for terminal ANSI color mapping.
/// The consumer is responsible for populating these from their theme system.
#[derive(Clone, Debug)]
pub struct TerminalThemeColors {
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub ansi_black: Hsla,
    pub ansi_red: Hsla,
    pub ansi_green: Hsla,
    pub ansi_yellow: Hsla,
    pub ansi_blue: Hsla,
    pub ansi_magenta: Hsla,
    pub ansi_cyan: Hsla,
    pub ansi_white: Hsla,
    pub ansi_bright_black: Hsla,
    pub ansi_bright_red: Hsla,
    pub ansi_bright_green: Hsla,
    pub ansi_bright_yellow: Hsla,
    pub ansi_bright_blue: Hsla,
    pub ansi_bright_magenta: Hsla,
    pub ansi_bright_cyan: Hsla,
    pub ansi_bright_white: Hsla,
}

impl Default for TerminalThemeColors {
    fn default() -> Self {
        // Sensible dark theme defaults
        Self {
            foreground: rgba_color(204, 204, 204),
            background: rgba_color(30, 30, 30),
            cursor: rgba_color(204, 204, 204),
            ansi_black: rgba_color(0, 0, 0),
            ansi_red: rgba_color(205, 49, 49),
            ansi_green: rgba_color(13, 188, 121),
            ansi_yellow: rgba_color(229, 229, 16),
            ansi_blue: rgba_color(36, 114, 200),
            ansi_magenta: rgba_color(188, 63, 188),
            ansi_cyan: rgba_color(17, 168, 205),
            ansi_white: rgba_color(204, 204, 204),
            ansi_bright_black: rgba_color(102, 102, 102),
            ansi_bright_red: rgba_color(241, 76, 76),
            ansi_bright_green: rgba_color(35, 209, 139),
            ansi_bright_yellow: rgba_color(245, 245, 67),
            ansi_bright_blue: rgba_color(59, 142, 234),
            ansi_bright_magenta: rgba_color(214, 112, 214),
            ansi_bright_cyan: rgba_color(41, 184, 219),
            ansi_bright_white: rgba_color(229, 229, 229),
        }
    }
}

// ---------------------------------------------------------------------------
// ContentMode — how the terminal fits into its container
// ---------------------------------------------------------------------------

/// How the terminal content is displayed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentMode {
    /// Terminal grows with content (no scrollbar), showing up to `displayed_lines`.
    Inline {
        displayed_lines: usize,
        total_lines: usize,
    },
    /// Fixed height with internal scrolling.
    Scrollable,
}

impl ContentMode {
    pub fn is_scrollable(&self) -> bool {
        matches!(self, ContentMode::Scrollable)
    }
}

// ---------------------------------------------------------------------------
// LayoutRect — a colored rectangle for cell backgrounds
// ---------------------------------------------------------------------------

struct LayoutRect {
    point: GridPoint,
    num_of_cells: usize,
    color: Hsla,
}

impl LayoutRect {
    fn paint(&self, origin: Point<Pixels>, dimensions: &TerminalBounds, window: &mut Window) {
        let position = point(
            (origin.x + self.point.column.0 as f32 * dimensions.cell_width).floor(),
            origin.y + self.point.line.0 as f32 * dimensions.line_height,
        );
        let size: Size<Pixels> = size(
            (dimensions.cell_width * self.num_of_cells as f32).ceil(),
            dimensions.line_height,
        );
        window.paint_quad(fill(Bounds::new(position, size), self.color));
    }
}

// ---------------------------------------------------------------------------
// BatchedTextRun — adjacent cells with same style batched for rendering
// ---------------------------------------------------------------------------

struct BatchedTextRun {
    start_point: GridPoint,
    text: String,
    cell_count: usize,
    style: TextRun,
    font_size: Pixels,
}

impl BatchedTextRun {
    fn paint(
        &self,
        origin: Point<Pixels>,
        dimensions: &TerminalBounds,
        window: &mut Window,
        cx: &mut App,
    ) {
        let pos = point(
            origin.x + self.start_point.column.0 as f32 * dimensions.cell_width,
            origin.y + self.start_point.line.0 as f32 * dimensions.line_height,
        );

        let _ = window
            .text_system()
            .shape_line(
                SharedString::from(self.text.clone()),
                self.font_size,
                std::slice::from_ref(&self.style),
                Some(dimensions.cell_width),
            )
            .paint(pos, dimensions.line_height, window, cx);
    }
}

// ---------------------------------------------------------------------------
// LayoutState — precomputed data for painting
// ---------------------------------------------------------------------------

pub struct LayoutState {
    batched_text_runs: Vec<BatchedTextRun>,
    rects: Vec<LayoutRect>,
    background_color: Hsla,
    dimensions: TerminalBounds,
}

// ---------------------------------------------------------------------------
// TerminalElement — the GPUI Element that renders the terminal grid
// ---------------------------------------------------------------------------

/// A GPUI Element that renders a terminal's cell grid.
pub struct TerminalElement {
    terminal: Entity<Terminal>,
    font_family: SharedString,
    font_size: Pixels,
    theme_colors: TerminalThemeColors,
    content_mode: ContentMode,
}

impl TerminalElement {
    pub fn new(
        terminal: Entity<Terminal>,
        font_family: SharedString,
        font_size: Pixels,
        theme_colors: TerminalThemeColors,
        content_mode: ContentMode,
    ) -> Self {
        Self {
            terminal,
            font_family,
            font_size,
            theme_colors,
            content_mode,
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<LayoutState>;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();

        match self.content_mode {
            ContentMode::Inline {
                displayed_lines, ..
            } => {
                // Use font_size * 1.4 as line height approximation for layout
                let line_height = self.font_size * 1.4;
                let height = line_height * displayed_lines as f32;
                style.size.height = height.into();
            }
            ContentMode::Scrollable => {
                style.size.height = relative(1.).into();
            }
        }

        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return None;
        }

        let font = Font {
            family: self.font_family.clone(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: Default::default(),
            fallbacks: None,
        };

        let font_id = window.text_system().resolve_font(&font);

        // Compute cell dimensions from font metrics
        let cell_width = window
            .text_system()
            .advance(font_id, self.font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(self.font_size * 0.6);

        // Line height: use the advance height if available, or font_size * 1.4
        let line_height = window
            .text_system()
            .advance(font_id, self.font_size, 'M')
            .map(|s| {
                // advance height is typically 0 for horizontal fonts, so use a multiplier
                if s.height > px(0.) {
                    s.height
                } else {
                    self.font_size * 1.4
                }
            })
            .unwrap_or(self.font_size * 1.4);

        let terminal_bounds = TerminalBounds::new(line_height, cell_width, bounds);

        // Update terminal dimensions and get content snapshot
        self.terminal.update(cx, |terminal, cx| {
            terminal.set_size(terminal_bounds);
            terminal.sync(cx);
        });

        let content = self.terminal.read(cx);
        let last_content = &content.last_content;

        // Layout the grid
        let (rects, text_runs) = layout_grid(
            &last_content.cells,
            &self.theme_colors,
            &font,
            self.font_size,
            cell_width,
            &terminal_bounds,
        );

        Some(LayoutState {
            batched_text_runs: text_runs,
            rects,
            background_color: self.theme_colors.background,
            dimensions: terminal_bounds,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(layout) = prepaint.take() else {
            return;
        };

        let content_mask = ContentMask { bounds };
        window.with_content_mask(Some(content_mask), |window| {
            // 1. Fill background
            window.paint_quad(fill(bounds, layout.background_color));

            let origin = bounds.origin;

            // 2. Paint cell backgrounds
            for rect in &layout.rects {
                rect.paint(origin, &layout.dimensions, window);
            }

            // 3. Paint text
            for batch in &layout.batched_text_runs {
                batch.paint(origin, &layout.dimensions, window, cx);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// layout_grid — convert cells to rendering primitives
// ---------------------------------------------------------------------------

fn layout_grid(
    cells: &[IndexedCell],
    theme_colors: &TerminalThemeColors,
    font: &Font,
    font_size: Pixels,
    _cell_width: Pixels,
    _terminal_bounds: &TerminalBounds,
) -> (Vec<LayoutRect>, Vec<BatchedTextRun>) {
    let mut rects: Vec<LayoutRect> = Vec::new();
    let mut text_runs: Vec<BatchedTextRun> = Vec::new();

    // Group cells by line
    let mut current_line: Option<i32> = None;

    // Background region tracking
    let mut bg_start: Option<(GridPoint, Hsla)> = None;
    let mut bg_count: usize = 0;

    // Text batch tracking
    let mut current_batch: Option<BatchedTextRun> = None;

    for indexed_cell in cells {
        let point = indexed_cell.point;
        let cell = &indexed_cell.cell;

        // Detect line change
        if current_line != Some(point.line.0) {
            // Flush background region
            flush_bg_region(&mut rects, &mut bg_start, bg_count);
            bg_count = 0;

            // Flush text batch
            flush_text_batch(&mut text_runs, &mut current_batch);

            current_line = Some(point.line.0);
        }

        // Handle cell flags
        let mut fg = cell.fg;
        let mut bg = cell.bg;

        // INVERSE flag swaps fg/bg
        if cell.flags.contains(AlacCellFlags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }

        let fg_color = convert_color(&fg, theme_colors);
        let bg_color = convert_color(&bg, theme_colors);

        // Collect background if non-default
        let is_default_bg = bg
            == alacritty_terminal::vte::ansi::Color::Named(
                alacritty_terminal::vte::ansi::NamedColor::Background,
            );

        if !is_default_bg {
            match &bg_start {
                Some((start_pt, start_color))
                    if *start_color == bg_color && start_pt.line == point.line =>
                {
                    bg_count += 1;
                }
                _ => {
                    flush_bg_region(&mut rects, &mut bg_start, bg_count);
                    bg_start = Some((point, bg_color));
                    bg_count = 1;
                }
            }
        } else {
            flush_bg_region(&mut rects, &mut bg_start, bg_count);
            bg_count = 0;
        }

        // Skip wide char spacers
        if cell.flags.contains(AlacCellFlags::WIDE_CHAR_SPACER) {
            continue;
        }

        let ch = cell.c;

        // Skip truly empty cells
        if ch == ' '
            && !cell.flags.contains(AlacCellFlags::UNDERLINE)
            && !cell.flags.contains(AlacCellFlags::STRIKEOUT)
        {
            flush_text_batch(&mut text_runs, &mut current_batch);
            continue;
        }

        // Build the text style
        let text_run = cell_style(cell, fg_color, font, font_size);

        // Try to extend current batch
        let can_extend = current_batch.as_ref().is_some_and(|batch| {
            batch.style.font == text_run.font
                && batch.style.color == text_run.color
                && batch.style.underline == text_run.underline
                && batch.style.strikethrough == text_run.strikethrough
                && batch.start_point.line == point.line
        });

        if can_extend {
            let batch = current_batch.as_mut().unwrap();
            batch.text.push(ch);
            batch.cell_count += 1;
        } else {
            flush_text_batch(&mut text_runs, &mut current_batch);
            let mut text = String::new();
            text.push(ch);
            current_batch = Some(BatchedTextRun {
                start_point: point,
                text,
                cell_count: 1,
                style: text_run,
                font_size,
            });
        }
    }

    // Flush remaining
    flush_bg_region(&mut rects, &mut bg_start, bg_count);
    flush_text_batch(&mut text_runs, &mut current_batch);

    (rects, text_runs)
}

fn flush_bg_region(
    rects: &mut Vec<LayoutRect>,
    bg_start: &mut Option<(GridPoint, Hsla)>,
    count: usize,
) {
    if let Some((point, color)) = bg_start.take() {
        if count > 0 {
            rects.push(LayoutRect {
                point,
                num_of_cells: count,
                color,
            });
        }
    }
}

fn flush_text_batch(
    text_runs: &mut Vec<BatchedTextRun>,
    current_batch: &mut Option<BatchedTextRun>,
) {
    if let Some(mut batch) = current_batch.take() {
        // The TextRun.len must match the byte length of the text
        batch.style.len = batch.text.len();
        text_runs.push(batch);
    }
}

// ---------------------------------------------------------------------------
// cell_style — convert cell flags to a GPUI TextRun
// ---------------------------------------------------------------------------

fn cell_style(cell: &AlacCell, fg_color: Hsla, font: &Font, _font_size: Pixels) -> TextRun {
    let mut weight = font.weight;
    let mut style = font.style;

    if cell.flags.contains(AlacCellFlags::BOLD) {
        weight = FontWeight::BOLD;
    }
    if cell.flags.contains(AlacCellFlags::ITALIC) {
        style = FontStyle::Italic;
    }

    let underline = if cell.flags.contains(AlacCellFlags::UNDERLINE) {
        Some(gpui::UnderlineStyle {
            thickness: px(1.),
            color: Some(fg_color),
            wavy: false,
        })
    } else {
        None
    };

    let strikethrough = if cell.flags.contains(AlacCellFlags::STRIKEOUT) {
        Some(gpui::StrikethroughStyle {
            thickness: px(1.),
            color: Some(fg_color),
        })
    } else {
        None
    };

    let mut color = fg_color;
    if cell.flags.contains(AlacCellFlags::DIM) {
        color.a *= 0.7;
    }

    TextRun {
        len: 1, // Will be updated when flushing
        font: Font {
            family: font.family.clone(),
            weight,
            style,
            features: font.features.clone(),
            fallbacks: font.fallbacks.clone(),
        },
        color,
        background_color: None,
        underline,
        strikethrough,
    }
}

// ---------------------------------------------------------------------------
// TerminalView — GPUI entity wrapping TerminalElement
// ---------------------------------------------------------------------------

/// Mode the terminal view operates in.
#[derive(Debug, Clone, Copy)]
pub enum TerminalMode {
    /// Full standalone terminal (scrollable).
    Standalone,
    /// Embedded in another view (inline, grows with content).
    Embedded {
        /// Maximum lines to show when not focused.
        max_lines_when_unfocused: Option<usize>,
    },
}

/// A GPUI entity that manages a terminal and renders it as an element.
pub struct TerminalView {
    terminal: Entity<Terminal>,
    mode: TerminalMode,
    font_family: SharedString,
    font_size: Pixels,
    theme_colors: TerminalThemeColors,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

/// Events emitted by TerminalView.
#[derive(Debug, Clone)]
pub enum TerminalViewEvent {
    /// The terminal content has changed.
    Wakeup,
    /// The child process exited.
    ChildExit(Option<i32>),
}

const MAX_EMBEDDED_LINES: usize = 1_000;

impl TerminalView {
    pub fn new(
        terminal: Entity<Terminal>,
        font_family: impl Into<SharedString>,
        font_size: Pixels,
        theme_colors: TerminalThemeColors,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.subscribe(
            &terminal,
            |_this: &mut Self, _terminal, event, cx| match event {
                terminal::Event::Wakeup => {
                    cx.emit(TerminalViewEvent::Wakeup);
                    cx.notify();
                }
                terminal::Event::ChildExit(status) => {
                    cx.emit(TerminalViewEvent::ChildExit(*status));
                    cx.notify();
                }
                _ => {}
            },
        );

        Self {
            terminal,
            mode: TerminalMode::Standalone,
            font_family: font_family.into(),
            font_size,
            theme_colors,
            _subscriptions: vec![sub],
        }
    }

    /// Set the terminal into embedded mode (inline growth with optional line limit).
    pub fn set_embedded_mode(
        &mut self,
        max_lines_when_unfocused: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.mode = TerminalMode::Embedded {
            max_lines_when_unfocused,
        };
        cx.notify();
    }

    /// The underlying terminal entity.
    pub fn terminal(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    /// Compute the content mode based on the terminal mode and content.
    pub fn content_mode(&self, cx: &App) -> ContentMode {
        match &self.mode {
            TerminalMode::Standalone => ContentMode::Scrollable,
            TerminalMode::Embedded {
                max_lines_when_unfocused,
            } => {
                let terminal = self.terminal.read(cx);
                let total_lines = terminal.total_lines();
                // Use content_lines() for height so the card grows with
                // actual output rather than showing empty grid rows.
                let content_lines = terminal.content_lines();
                if total_lines > MAX_EMBEDDED_LINES {
                    ContentMode::Scrollable
                } else {
                    let displayed_lines = if let Some(max) = max_lines_when_unfocused {
                        content_lines.min(*max)
                    } else {
                        content_lines
                    };
                    ContentMode::Inline {
                        displayed_lines: displayed_lines.max(1),
                        total_lines,
                    }
                }
            }
        }
    }

    /// Update theme colors.
    pub fn set_theme_colors(&mut self, colors: TerminalThemeColors, cx: &mut Context<Self>) {
        self.theme_colors = colors;
        cx.notify();
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content_mode = self.content_mode(cx);

        div().size_full().child(TerminalElement::new(
            self.terminal.clone(),
            self.font_family.clone(),
            self.font_size,
            self.theme_colors.clone(),
            content_mode,
        ))
    }
}
