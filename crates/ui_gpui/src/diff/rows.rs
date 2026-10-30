//! The rows of a diff as one element: backgrounds, gutter numbers and
//! text painted directly, instead of three or four layout nodes per row.
//! With a diff card in view, laying those nodes out was the largest part of
//! a frame (see docs/frame-profiling.md); here taffy sees one node per
//! chunk and the text system's line cache does the rest.
//!
//! A row has one cell (stacked) or two side by side (split: the old file's
//! line left, the new file's right, either may be empty). Text wraps, or
//! stays on one line and scrolls sideways with a [`HScroll`] that all rows
//! of a file share; the line numbers stay in place.

use gpui_kit::{
    App, AvailableSpace, Bounds, ContentMask, DispatchPhase, Element, GlobalElementId,
    HighlightStyle, Hsla, InspectorElementId, IntoElement, LayoutId, Length, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, ShapedLine,
    SharedString, Size, Style, TextAlign, TextRun, TextStyle, Window, WrappedLine, fill, point, px,
    relative, size,
};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// One row, ready to paint.
pub(crate) struct DiffRow {
    pub text: SharedString,
    /// Sorted, non-overlapping, within `text`.
    pub highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub background: Option<Hsla>,
    /// A thin bar at the left edge (added / removed rows).
    pub edge: Option<Hsla>,
    pub color: Hsla,
    /// Line number text (already padded to the gutter width) and its color.
    pub gutter: Option<(SharedString, Hsla)>,
}

/// One side of a row and the line it shows, counted from
/// [`RowSelection::base_line`].
pub(crate) struct RowCell {
    pub row: DiffRow,
    pub line: usize,
}

/// A row's cells: the left one only when stacked.
pub(crate) type RowCells = [Option<RowCell>; 2];

/// Horizontal layout of the rows (of each column), in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RowGeometry {
    /// Width of the gutter column; zero without one.
    pub gutter_width: Pixels,
    /// Left padding of the gutter text inside the gutter.
    pub gutter_left: Pixels,
    /// Padding left and right of the text.
    pub content_left: Pixels,
    pub content_right: Pixels,
}

impl RowGeometry {
    fn text_inset(&self) -> Pixels {
        self.gutter_width + self.content_left + self.content_right
    }
}

/// How far unwrapped rows are scrolled sideways, shared by all elements
/// showing one file, and the widest text they have shown. (Atomics: a
/// file's state is built off the UI thread.)
#[derive(Clone, Default, Debug)]
pub(crate) struct HScroll(Arc<HScrollState>);

#[derive(Default, Debug)]
struct HScrollState {
    /// `f32` bits, in pixels.
    offset: AtomicU32,
    widest: AtomicU32,
}

fn load(value: &AtomicU32) -> Pixels {
    px(f32::from_bits(value.load(Ordering::Relaxed)))
}

fn store(value: &AtomicU32, pixels: Pixels) {
    value.store(f32::from(pixels).to_bits(), Ordering::Relaxed);
}

impl HScroll {
    pub fn offset(&self) -> Pixels {
        load(&self.0.offset)
    }

    fn note_width(&self, width: Pixels) {
        if width > load(&self.0.widest) {
            store(&self.0.widest, width);
        }
    }

    /// Scroll by `delta` (positive: the text moves right) where `visible`
    /// of the widest text shows; whether the offset changed.
    fn scroll_by(&self, delta: Pixels, visible: Pixels) -> bool {
        let max = (load(&self.0.widest) - visible).max(px(0.));
        let offset = (self.offset() - delta).clamp(px(0.), max);
        let changed = offset != self.offset();
        store(&self.0.offset, offset);
        changed
    }
}

/// Makes a [`DiffRows`] participate in whole-line text selection. The element
/// maps a pointer position to a flat line index (`base_line` plus the line
/// of the cell under the pointer) and reports it through the callbacks; the
/// owner keeps the actual selection and hands back the flat lines
/// (`highlight`) to paint as selected. Selection is line-granular, which
/// suits grabbing code to paste elsewhere and keeps painting trivial over a
/// wrapped, virtualized list.
pub(crate) type LineCallback = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// Reports that a selection drag has ended.
pub(crate) type EndCallback = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct RowSelection {
    /// What a cell's `line` counts from.
    pub base_line: usize,
    /// Flat lines to paint as selected.
    pub highlight: Range<usize>,
    /// Highlight color (usually the theme's selection background).
    pub color: Hsla,
    /// Flat lines carrying a comment, marked by a bar at the left edge.
    pub marked: Vec<Range<usize>>,
    pub mark_color: Hsla,
    /// Receives the window bounds of the row showing the last highlighted
    /// line when painted, so the owner can float things next to the
    /// selection's end.
    pub anchor: Option<Rc<Cell<Option<Bounds<Pixels>>>>>,
    /// Pointer pressed on a row: the flat line under it.
    pub on_start: LineCallback,
    /// Pointer dragged over a row while the button is held.
    pub on_drag: LineCallback,
    /// Button released (anywhere) — ends the drag.
    pub on_end: EndCallback,
}

pub(crate) struct DiffRows {
    rows: Rc<Vec<RowCells>>,
    /// 1 (stacked) or 2 (split).
    columns: usize,
    geometry: RowGeometry,
    /// Fills the side of a split row that shows no line.
    empty: Option<Hsla>,
    /// `None`: lines wrap; else they scroll sideways together.
    scroll: Option<HScroll>,
    /// Filled by the measure closure, read by prepaint and paint.
    cell: LayoutCell,
    /// Present when the rows are selectable; drives highlight painting and
    /// pointer handling in [`DiffRows::paint`].
    selection: Option<RowSelection>,
}

impl DiffRows {
    /// Stacked rows; row `i` shows line `i`.
    pub fn new(rows: Vec<DiffRow>, geometry: RowGeometry) -> Self {
        let rows = rows
            .into_iter()
            .enumerate()
            .map(|(line, row)| [Some(RowCell { row, line }), None])
            .collect();
        Self::with_cells(rows, 1, geometry)
    }

    /// Rows of two cells side by side; `empty` fills a missing one.
    pub fn split(rows: Vec<RowCells>, geometry: RowGeometry, empty: Hsla) -> Self {
        Self {
            empty: Some(empty),
            ..Self::with_cells(rows, 2, geometry)
        }
    }

    fn with_cells(rows: Vec<RowCells>, columns: usize, geometry: RowGeometry) -> Self {
        Self {
            rows: Rc::new(rows),
            columns,
            geometry,
            empty: None,
            scroll: None,
            cell: Rc::default(),
            selection: None,
        }
    }

    /// Keep each line on one row; they scroll sideways with `scroll`.
    pub(crate) fn unwrapped(mut self, scroll: HScroll) -> Self {
        self.scroll = Some(scroll);
        self
    }

    /// Enable whole-line selection on these rows.
    pub(crate) fn selectable(mut self, selection: RowSelection) -> Self {
        self.selection = Some(selection);
        self
    }

    #[cfg(test)]
    fn layout_cell(&self) -> LayoutCell {
        self.cell.clone()
    }
}

/// The row whose band contains the absolute `y`, clamped to
/// `[0, row_count)`. `tops` holds the absolute top of each row plus a final
/// bottom (so it has `row_count + 1` entries).
fn row_at(tops: &[Pixels], row_count: usize, y: Pixels) -> usize {
    if row_count == 0 {
        return 0;
    }
    tops[..row_count]
        .iter()
        .rposition(|&top| y >= top)
        .unwrap_or(0)
        .min(row_count - 1)
}

/// The line of the cell in `column`, else of the row's other cell.
fn line_of(cells: &RowCells, column: usize) -> Option<usize> {
    let pick = |c: usize| cells.get(c).and_then(|cell| cell.as_ref()).map(|c| c.line);
    pick(column).or_else(|| pick(1 - column.min(1)))
}

/// Shaped rows for one width.
#[derive(Default)]
pub(crate) struct RowsLayout {
    pub line_height: Pixels,
    pub rows: Vec<RowLayout>,
    pub width: Pixels,
    pub height: Pixels,
    /// The widest cell's text plus its padding.
    pub widest: Pixels,
}

pub(crate) struct CellLayout {
    pub lines: Vec<WrappedLine>,
    pub gutter: Option<ShapedLine>,
}

pub(crate) struct RowLayout {
    pub cells: [Option<CellLayout>; 2],
    pub height: Pixels,
}

impl RowLayout {
    /// Visual lines of the first cell after wrapping.
    #[cfg(test)]
    pub fn line_count(&self) -> usize {
        self.cells[0].as_ref().map_or(0, |cell| {
            cell.lines
                .iter()
                .map(|line| line.wrap_boundaries.len() + 1)
                .sum()
        })
    }
}

/// Filled by the measure closure, read by `paint`.
pub(crate) type LayoutCell = Rc<RefCell<Option<RowsLayout>>>;

/// The text runs of a row: `default` (with the row's color) where nothing is
/// highlighted, `default` refined by the highlight elsewhere. They cover the
/// text exactly.
pub(crate) fn row_runs(
    text_len: usize,
    default: &TextStyle,
    highlights: &[(Range<usize>, HighlightStyle)],
) -> Vec<TextRun> {
    let mut runs = Vec::with_capacity(highlights.len() * 2 + 1);
    let mut ix = 0;
    for (range, highlight) in highlights {
        let range = range.start.max(ix)..range.end.min(text_len);
        if range.start >= range.end {
            continue;
        }
        if ix < range.start {
            runs.push(default.to_run(range.start - ix));
        }
        runs.push(default.clone().highlight(*highlight).to_run(range.len()));
        ix = range.end;
    }
    if ix < text_len {
        runs.push(default.to_run(text_len - ix));
    }
    runs
}

/// The text style the rows are shaped with, taken while the ancestors' styles
/// are still on the window's stack.
struct RowTextStyle {
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
}

impl RowTextStyle {
    fn capture(window: &Window) -> Self {
        let style = window.text_style();
        Self {
            font_size: style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
            style,
        }
    }
}

/// What the rows are laid out by besides the width.
#[derive(Clone, Copy)]
struct Shape {
    columns: usize,
    geometry: RowGeometry,
    wrap: bool,
}

fn layout_rows(
    rows: &[RowCells],
    shape: Shape,
    width: Option<Pixels>,
    text: &RowTextStyle,
    window: &mut Window,
) -> RowsLayout {
    let RowTextStyle {
        style: text_style,
        font_size,
        line_height,
    } = text;
    let (font_size, line_height) = (*font_size, *line_height);
    let geometry = shape.geometry;
    let column_width = width.map(|width| width / shape.columns as f32);
    let wrap_width = column_width
        .filter(|_| shape.wrap)
        .map(|width| (width - geometry.text_inset()).max(px(0.)));
    let mut layout = RowsLayout {
        line_height,
        rows: Vec::with_capacity(rows.len()),
        width: width.unwrap_or_default(),
        height: px(0.),
        widest: px(0.),
    };
    for cells in rows {
        let mut height = line_height;
        let mut laid = [None, None];
        for (column, cell) in cells.iter().enumerate().take(shape.columns) {
            let Some(RowCell { row, .. }) = cell else {
                continue;
            };
            let mut style = text_style.clone();
            style.color = row.color;
            let runs = row_runs(row.text.len(), &style, &row.highlights);
            let lines = window
                .text_system()
                .shape_text(row.text.clone(), font_size, &runs, wrap_width, None)
                .map(|lines| lines.into_vec())
                .unwrap_or_default();
            let gutter = row.gutter.as_ref().map(|(text, color)| {
                let mut style = text_style.clone();
                style.color = *color;
                window.text_system().shape_line(
                    text.clone(),
                    font_size,
                    &[style.to_run(text.len())],
                    None,
                )
            });
            let text_width = lines
                .iter()
                .map(|line| line.size(line_height).width)
                .fold(px(0.), Pixels::max);
            layout.widest = layout
                .widest
                .max(text_width + geometry.content_left + geometry.content_right);
            height = height.max(
                lines
                    .iter()
                    .map(|line| line.size(line_height).height)
                    .fold(px(0.), |a, b| a + b),
            );
            laid[column] = Some(CellLayout { lines, gutter });
        }
        layout.height += height;
        layout.rows.push(RowLayout {
            cells: laid,
            height,
        });
    }
    if width.is_none() {
        layout.width = (layout.widest + geometry.gutter_width) * shape.columns as f32;
    }
    layout
}

/// Where a column's parts go, from the element's bounds.
struct ColumnFrame {
    left: Pixels,
    width: Pixels,
}

#[allow(clippy::too_many_arguments)]
fn paint_rows(
    rows: &[RowCells],
    layout: &RowsLayout,
    shape: Shape,
    empty: Option<Hsla>,
    scroll: Pixels,
    bounds: Bounds<Pixels>,
    selection: Option<&RowSelection>,
    window: &mut Window,
    cx: &mut App,
) {
    let geometry = shape.geometry;
    let line_height = layout.line_height;
    let column_width = bounds.size.width / shape.columns as f32;
    let flat = |line: usize| selection.map_or(line, |sel| sel.base_line + line);
    let mut y = bounds.origin.y;
    for (cells, row_layout) in rows.iter().zip(&layout.rows) {
        let height = row_layout.height;
        let columns = cells.iter().zip(&row_layout.cells).take(shape.columns);
        for (column, (cell, cell_layout)) in columns.enumerate() {
            let frame = ColumnFrame {
                left: bounds.origin.x + column_width * column as f32,
                width: column_width,
            };
            let band = |width: Pixels| Bounds::new(point(frame.left, y), size(width, height));
            let (Some(cell), Some(cell_layout)) = (cell, cell_layout) else {
                if let Some(empty) = empty {
                    window.paint_quad(fill(band(frame.width), empty));
                }
                continue;
            };
            let row = &cell.row;
            let line = flat(cell.line);
            if let Some(background) = row.background {
                window.paint_quad(fill(band(frame.width), background));
            }
            if let Some(edge) = row.edge {
                window.paint_quad(fill(band(px(2.)), edge));
            }
            // Selection sits above the add/delete tint but below the glyphs,
            // so the selected text stays readable.
            if let Some(sel) = selection {
                if sel.highlight.contains(&line) {
                    window.paint_quad(fill(band(frame.width), sel.color));
                }
                if sel.marked.iter().any(|range| range.contains(&line)) {
                    window.paint_quad(fill(band(px(3.)), sel.mark_color));
                }
            }
            if let Some(gutter) = &cell_layout.gutter {
                _ = gutter.paint(
                    point(frame.left + geometry.gutter_left, y),
                    line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            let code = Bounds::new(
                point(frame.left + geometry.gutter_width, y),
                size((frame.width - geometry.gutter_width).max(px(0.)), height),
            );
            let mut origin: Point<Pixels> =
                point(code.origin.x + geometry.content_left - scroll, y);
            window.with_content_mask(Some(ContentMask { bounds: code }), |window| {
                for line in &cell_layout.lines {
                    _ = line.paint_background(
                        origin,
                        line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
                    origin.y += line.size(line_height).height;
                }
            });
        }
        y += height;
    }
}

impl IntoElement for DiffRows {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl DiffRows {
    fn shape(&self) -> Shape {
        Shape {
            columns: self.columns,
            geometry: self.geometry,
            wrap: self.scroll.is_none(),
        }
    }
}

impl Element for DiffRows {
    type RequestLayoutState = LayoutCell;
    /// Selectable or scrolling rows take the pointer through a hitbox, so
    /// something floating above them (a selection pill) keeps its clicks.
    type PrepaintState = Option<gpui_kit::Hitbox>;

    fn id(&self) -> Option<gpui_kit::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, LayoutCell) {
        let cell = self.cell.clone();
        let rows = self.rows.clone();
        let shape = self.shape();
        // The measure closure runs in taffy's layout pass, when the enclosing
        // elements' text styles are no longer on the window: read them now.
        let text = RowTextStyle::capture(window);
        let style = Style {
            size: Size {
                width: relative(1.).into(),
                height: Length::Auto,
            },
            ..Style::default()
        };
        let layout_id = window.request_measured_layout(style, {
            let cell = cell.clone();
            move |known, available, window, _cx| {
                let width = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let layout = layout_rows(&rows, shape, width, &text, window);
                let measured = size(layout.width, layout.height);
                *cell.borrow_mut() = Some(layout);
                measured
            }
        });
        (layout_id, cell)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut LayoutCell,
        window: &mut Window,
        _: &mut App,
    ) -> Option<gpui_kit::Hitbox> {
        (self.selection.is_some() || self.scroll.is_some())
            .then(|| window.insert_hitbox(bounds, gpui_kit::HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        cell: &mut LayoutCell,
        hitbox: &mut Option<gpui_kit::Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let borrow = cell.borrow();
        let Some(layout) = borrow.as_ref() else {
            return;
        };
        let shape = self.shape();
        let offset = self.scroll.as_ref().map_or(px(0.), HScroll::offset);

        paint_rows(
            &self.rows,
            layout,
            shape,
            self.empty,
            offset,
            bounds,
            self.selection.as_ref(),
            window,
            cx,
        );

        let column_width = bounds.size.width / shape.columns as f32;
        if let Some(scroll) = self.scroll.clone() {
            scroll.note_width(layout.widest);
            let visible = column_width - shape.geometry.gutter_width;
            let line_height = layout.line_height;
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                let hovered = hitbox
                    .as_ref()
                    .map_or(bounds.contains(&e.position), |h| h.is_hovered(window));
                if phase != DispatchPhase::Bubble || !hovered {
                    return;
                }
                let delta = e.delta.pixel_delta(line_height);
                // Mostly sideways: the rows take it, the list stays put.
                if delta.x.abs() > delta.y.abs() {
                    if scroll.scroll_by(delta.x, visible) {
                        window.refresh();
                    }
                    cx.stop_propagation();
                }
            });
        }

        // Pointer handling for selectable rows. The absolute top of each row
        // (plus a final bottom) lets the listeners below map a pointer y to a
        // row without re-reading the layout at event time.
        if let Some(sel) = &self.selection {
            let mut tops = Vec::with_capacity(layout.rows.len() + 1);
            let mut y = bounds.origin.y;
            for row in &layout.rows {
                tops.push(y);
                y += row.height;
            }
            tops.push(y);
            if let Some(anchor) = &sel.anchor
                && let Some(last) = sel.highlight.end.checked_sub(1)
                && let Some(row) = self.rows.iter().position(|cells| {
                    cells
                        .iter()
                        .flatten()
                        .any(|cell| sel.base_line + cell.line == last)
                })
            {
                anchor.set(Some(Bounds::new(
                    point(bounds.origin.x, tops[row]),
                    size(bounds.size.width, tops[row + 1] - tops[row]),
                )));
            }
            let tops = Rc::new(tops);
            let base = sel.base_line;
            let row_count = layout.rows.len();
            let (on_start, on_drag, on_end) = (
                sel.on_start.clone(),
                sel.on_drag.clone(),
                sel.on_end.clone(),
            );
            drop(borrow);
            // The flat line under a pointer position.
            let rows = self.rows.clone();
            let columns = shape.columns;
            let line_at = Rc::new(move |position: Point<Pixels>| {
                let row = row_at(&tops, row_count, position.y);
                let column = (((position.x - bounds.origin.x) / column_width).floor() as usize)
                    .min(columns - 1);
                rows.get(row)
                    .and_then(|cells| line_of(cells, column))
                    .map(|line| base + line)
            });

            let line_down = line_at.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && e.button == MouseButton::Left
                    && hitbox
                        .as_ref()
                        .map_or(bounds.contains(&e.position), |h| h.is_hovered(window))
                    && let Some(line) = line_down(e.position)
                {
                    on_start(line, window, cx);
                }
            });

            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && e.pressed_button == Some(MouseButton::Left)
                    && bounds.contains(&e.position)
                    && let Some(line) = line_at(e.position)
                {
                    on_drag(line, window, cx);
                }
            });

            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                    on_end(window, cx);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{
        Context, ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, div,
        red, white,
    };

    #[test]
    fn a_pointer_maps_to_a_row_and_a_cell() {
        // Three rows at tops 0,10,20 with bottom 30.
        let tops = [px(0.), px(10.), px(20.), px(30.)];
        assert_eq!(row_at(&tops, 3, px(5.)), 0);
        assert_eq!(row_at(&tops, 3, px(10.)), 1);
        assert_eq!(row_at(&tops, 3, px(25.)), 2);
        // Above the first row and below the last clamp to the ends.
        assert_eq!(row_at(&tops, 3, px(-5.)), 0);
        assert_eq!(row_at(&tops, 3, px(999.)), 2);
        assert_eq!(row_at(&[px(0.)], 0, px(5.)), 0);

        let cell = |line| {
            Some(RowCell {
                row: rows(&["x"]).pop().unwrap(),
                line,
            })
        };
        assert_eq!(line_of(&[cell(4), cell(9)], 1), Some(9));
        // An empty side stands for the other one.
        assert_eq!(line_of(&[cell(4), None], 1), Some(4));
        assert_eq!(line_of(&[None, cell(9)], 0), Some(9));
        assert_eq!(line_of(&[None, None], 0), None);
    }

    #[test]
    fn sideways_scrolling_stays_within_the_widest_text() {
        let scroll = HScroll::default();
        scroll.note_width(px(300.));
        assert!(scroll.scroll_by(px(-50.), px(200.)));
        assert_eq!(scroll.offset(), px(50.));
        assert!(scroll.scroll_by(px(-500.), px(200.)));
        assert_eq!(scroll.offset(), px(100.));
        assert!(!scroll.scroll_by(px(-1.), px(200.)));
        assert!(scroll.scroll_by(px(500.), px(200.)));
        assert_eq!(scroll.offset(), px(0.));
    }

    #[test]
    fn row_runs_cover_the_text_exactly() {
        let style = TextStyle::default();
        let red_style = HighlightStyle {
            color: Some(red()),
            ..Default::default()
        };
        let runs = row_runs(10, &style, &[(2..4, red_style), (4..7, red_style)]);
        assert_eq!(
            runs.iter().map(|r| r.len).collect::<Vec<_>>(),
            vec![2, 2, 3, 3]
        );
        assert_eq!(runs[1].color, red());
        assert_eq!(runs[3].color, style.color);

        // Out-of-range and overlapping highlights are clipped, never counted twice.
        let runs = row_runs(5, &style, &[(3..9, red_style), (2..4, red_style)]);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), 5);
        assert!(row_runs(0, &style, &[]).is_empty());
    }

    struct Root;

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn rows(texts: &[&str]) -> Vec<DiffRow> {
        texts
            .iter()
            .map(|text| DiffRow {
                text: SharedString::from(text.to_string()),
                highlights: Vec::new(),
                background: Some(red()),
                edge: None,
                color: white(),
                gutter: Some(("12".into(), white())),
            })
            .collect()
    }

    const GEOMETRY: RowGeometry = RowGeometry {
        gutter_width: px(30.),
        gutter_left: px(6.),
        content_left: px(4.),
        content_right: px(12.),
    };

    #[gpui_kit::test]
    fn rows_shape_with_the_enclosing_text_style(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Root);
        let cx: &mut VisualTestContext = cx;
        let element = DiffRows::new(rows(&["abc"]), GEOMETRY);
        let cell = element.layout_cell();
        // The measure closure runs during taffy's layout pass, after the
        // ancestors' text styles were popped; the rows must have captured
        // them in `request_layout`.
        cx.draw(point(px(0.), px(0.)), size(px(600.), px(400.)), |_, _| {
            div().text_size(px(10.)).line_height(px(15.)).child(element)
        });
        let layout = cell.borrow();
        let layout = layout.as_ref().expect("measured");
        assert_eq!(layout.line_height, px(15.));
        let cell = layout.rows[0].cells[0].as_ref().unwrap();
        assert_eq!(cell.lines[0].unwrapped_layout.font_size, px(10.));
    }

    #[gpui_kit::test]
    fn rows_take_one_line_each_and_wrap_when_narrow(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Root);
        let cx: &mut VisualTestContext = cx;
        let long = "x".repeat(200);
        let (cell, _) = cx.draw(point(px(0.), px(0.)), size(px(600.), px(400.)), |_, _| {
            DiffRows::new(rows(&["short", "another"]), GEOMETRY)
        });
        {
            let layout = cell.borrow();
            let layout = layout.as_ref().expect("measured");
            assert_eq!(layout.rows.len(), 2);
            assert!(layout.rows.iter().all(|row| row.line_count() == 1));
            assert_eq!(layout.height, layout.line_height * 2.);
            assert_eq!(layout.width, px(600.));
        }

        let (cell, _) = cx.draw(point(px(0.), px(0.)), size(px(120.), px(400.)), |_, _| {
            DiffRows::new(rows(&[long.as_str()]), GEOMETRY)
        });
        let layout = cell.borrow();
        let layout = layout.as_ref().expect("measured");
        let lines = layout.rows[0].line_count();
        assert!(lines > 1, "{lines} lines");
        assert_eq!(layout.height, layout.line_height * lines as f32);
    }

    #[gpui_kit::test]
    fn unwrapped_rows_keep_one_line_and_split_rows_take_the_taller_cell(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Root);
        let cx: &mut VisualTestContext = cx;
        let long = "x".repeat(200);
        let (cell, _) = cx.draw(point(px(0.), px(0.)), size(px(120.), px(400.)), |_, _| {
            DiffRows::new(rows(&[long.as_str()]), GEOMETRY).unwrapped(HScroll::default())
        });
        {
            let layout = cell.borrow();
            let layout = layout.as_ref().expect("measured");
            assert_eq!(layout.rows[0].line_count(), 1);
            assert!(layout.widest > px(120.));
        }

        let pair = |left: &str, right: &str| {
            let mut texts = rows(&[left, right]);
            let right = texts.pop().unwrap();
            let left = texts.pop().unwrap();
            [
                Some(RowCell { row: left, line: 0 }),
                Some(RowCell {
                    row: right,
                    line: 1,
                }),
            ]
        };
        let (cell, _) = cx.draw(point(px(0.), px(0.)), size(px(240.), px(400.)), |_, _| {
            DiffRows::split(vec![pair("short", long.as_str())], GEOMETRY, white())
        });
        let layout = cell.borrow();
        let layout = layout.as_ref().expect("measured");
        let right = layout.rows[0].cells[1].as_ref().unwrap();
        let wrapped: usize = right
            .lines
            .iter()
            .map(|l| l.wrap_boundaries.len() + 1)
            .sum();
        assert!(wrapped > 1);
        assert_eq!(layout.rows[0].height, layout.line_height * wrapped as f32);
    }
}
