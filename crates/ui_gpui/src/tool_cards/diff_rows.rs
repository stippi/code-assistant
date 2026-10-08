//! The rows of a diff as one element: backgrounds, gutter numbers and
//! wrapped text painted directly, instead of three or four layout nodes per
//! row. With a diff card in view, laying those nodes out was the largest
//! part of a frame (see docs/frame-profiling.md); here taffy sees one node
//! per chunk and the text system's line cache does the rest.

use gpui_kit::{
    App, AvailableSpace, Bounds, DispatchPhase, Element, GlobalElementId, HighlightStyle, Hsla,
    InspectorElementId, IntoElement, LayoutId, Length, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ShapedLine, SharedString, Size, Style, TextAlign, TextRun,
    TextStyle, Window, WrappedLine, fill, point, px, relative, size,
};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

/// One row, ready to paint.
pub(crate) struct DiffRow {
    pub text: SharedString,
    /// Sorted, non-overlapping, within `text`.
    pub highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub background: Option<Hsla>,
    pub color: Hsla,
    /// Line number text (already padded to the gutter width) and its color.
    pub gutter: Option<(SharedString, Hsla)>,
}

/// Horizontal layout of the rows, in pixels.
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

/// Makes a [`DiffRows`] participate in whole-line text selection. The element
/// maps a pointer position to a flat line index (`base_line` plus the row
/// under the pointer) and reports it through the callbacks; the owner keeps
/// the actual selection and hands back the local rows (`highlight`) to paint
/// as selected. Selection is line-granular, which suits grabbing code to paste
/// elsewhere and keeps painting trivial over a wrapped, virtualized list.
/// Reports a flat line index (`base_line` + row) when selection starts or
/// extends over a row.
pub(crate) type LineCallback = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// Reports that a selection drag has ended.
pub(crate) type EndCallback = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct RowSelection {
    /// Flat line index of this element's first row (row 0).
    pub base_line: usize,
    /// Local rows (indices into `rows`) to paint as selected.
    pub highlight: Range<usize>,
    /// Highlight color (usually the theme's selection background).
    pub color: Hsla,
    /// Local rows carrying a comment, marked by a bar at the left edge.
    pub marked: Vec<Range<usize>>,
    pub mark_color: Hsla,
    /// Receives the window bounds of the last highlighted row when painted,
    /// so the owner can float things next to the selection's end.
    pub anchor: Option<Rc<std::cell::Cell<Option<Bounds<Pixels>>>>>,
    /// Pointer pressed on a row: `base_line + row`.
    pub on_start: LineCallback,
    /// Pointer dragged over a row while the button is held.
    pub on_drag: LineCallback,
    /// Button released (anywhere) — ends the drag.
    pub on_end: EndCallback,
}

pub(crate) struct DiffRows {
    rows: Rc<Vec<DiffRow>>,
    geometry: RowGeometry,
    /// Filled by the measure closure, read by prepaint and paint.
    cell: LayoutCell,
    /// Present when the rows are selectable (the Review panel); drives
    /// highlight painting and pointer handling in [`DiffRows::paint`].
    selection: Option<RowSelection>,
}

impl DiffRows {
    pub fn new(rows: Vec<DiffRow>, geometry: RowGeometry) -> Self {
        Self {
            rows: Rc::new(rows),
            geometry,
            cell: Rc::default(),
            selection: None,
        }
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

/// Map an absolute `y` to a flat line index: `base` plus the row whose band
/// contains `y`, clamped to `[0, row_count)`. `tops` holds the absolute top of
/// each row plus a final bottom (so it has `row_count + 1` entries).
fn local_line(tops: &[Pixels], base: usize, row_count: usize, y: Pixels) -> usize {
    if row_count == 0 {
        return base;
    }
    let row = tops[..row_count]
        .iter()
        .rposition(|&top| y >= top)
        .unwrap_or(0)
        .min(row_count - 1);
    base + row
}

/// Shaped rows for one width.
#[derive(Default)]
pub(crate) struct RowsLayout {
    pub line_height: Pixels,
    pub rows: Vec<RowLayout>,
    pub width: Pixels,
    pub height: Pixels,
}

pub(crate) struct RowLayout {
    pub lines: Vec<WrappedLine>,
    pub gutter: Option<ShapedLine>,
    pub height: Pixels,
}

impl RowLayout {
    /// Visual lines after wrapping.
    #[cfg(test)]
    pub fn line_count(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.wrap_boundaries.len() + 1)
            .sum()
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

fn layout_rows(
    rows: &[DiffRow],
    geometry: RowGeometry,
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
    let wrap_width = width.map(|width| (width - geometry.text_inset()).max(px(0.)));
    let mut layout = RowsLayout {
        line_height,
        rows: Vec::with_capacity(rows.len()),
        width: width.unwrap_or_default(),
        height: px(0.),
    };
    for row in rows {
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
        if width.is_none() {
            layout.width = layout.width.max(text_width + geometry.text_inset());
        }
        let row_layout = RowLayout {
            height: lines
                .iter()
                .map(|line| line.size(line_height).height)
                .fold(px(0.), |a, b| a + b)
                .max(line_height),
            lines,
            gutter,
        };
        layout.height += row_layout.height;
        layout.rows.push(row_layout);
    }
    layout
}

fn paint_rows(
    rows: &[DiffRow],
    layout: &RowsLayout,
    geometry: RowGeometry,
    bounds: Bounds<Pixels>,
    selection: Option<&RowSelection>,
    window: &mut Window,
    cx: &mut App,
) {
    let line_height = layout.line_height;
    let mut y = bounds.origin.y;
    for (ix, (row, row_layout)) in rows.iter().zip(&layout.rows).enumerate() {
        if let Some(background) = row.background {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, y),
                    size(bounds.size.width, row_layout.height),
                ),
                background,
            ));
        }
        // Selection sits above the add/delete tint but below the glyphs, so
        // the selected text stays readable.
        if let Some(sel) = selection
            && sel.highlight.contains(&ix)
        {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, y),
                    size(bounds.size.width, row_layout.height),
                ),
                sel.color,
            ));
        }
        if let Some(sel) = selection
            && sel.marked.iter().any(|range| range.contains(&ix))
        {
            window.paint_quad(fill(
                Bounds::new(point(bounds.origin.x, y), size(px(3.), row_layout.height)),
                sel.mark_color,
            ));
        }
        if let Some(gutter) = &row_layout.gutter {
            _ = gutter.paint(
                point(bounds.origin.x + geometry.gutter_left, y),
                line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        let mut origin: Point<Pixels> = point(
            bounds.origin.x + geometry.gutter_width + geometry.content_left,
            y,
        );
        for line in &row_layout.lines {
            _ = line.paint_background(origin, line_height, TextAlign::Left, None, window, cx);
            _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
            origin.y += line.size(line_height).height;
        }
        y += row_layout.height;
    }
}

impl IntoElement for DiffRows {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for DiffRows {
    type RequestLayoutState = LayoutCell;
    /// Selectable rows take presses through a hitbox, so something floating
    /// above them (a selection pill) keeps its clicks.
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
        let geometry = self.geometry;
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
                let layout = layout_rows(&rows, geometry, width, &text, window);
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
        self.selection
            .is_some()
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

        paint_rows(
            &self.rows,
            layout,
            self.geometry,
            bounds,
            self.selection.as_ref(),
            window,
            cx,
        );

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
                && last < layout.rows.len()
            {
                anchor.set(Some(Bounds::new(
                    point(bounds.origin.x, tops[last]),
                    size(bounds.size.width, tops[last + 1] - tops[last]),
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

            let tops_down = tops.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && e.button == MouseButton::Left
                    && hitbox
                        .as_ref()
                        .map_or(bounds.contains(&e.position), |h| h.is_hovered(window))
                {
                    on_start(
                        local_line(&tops_down, base, row_count, e.position.y),
                        window,
                        cx,
                    );
                }
            });

            let tops_move = tops.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && e.pressed_button == Some(MouseButton::Left)
                    && bounds.contains(&e.position)
                {
                    on_drag(
                        local_line(&tops_move, base, row_count, e.position.y),
                        window,
                        cx,
                    );
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
    fn local_line_maps_y_to_a_row_and_clamps() {
        // Three rows at tops 0,10,20 with bottom 30; base 100.
        let tops = [px(0.), px(10.), px(20.), px(30.)];
        assert_eq!(local_line(&tops, 100, 3, px(5.)), 100);
        assert_eq!(local_line(&tops, 100, 3, px(10.)), 101);
        assert_eq!(local_line(&tops, 100, 3, px(25.)), 102);
        // Above the first row and below the last clamp to the ends.
        assert_eq!(local_line(&tops, 100, 3, px(-5.)), 100);
        assert_eq!(local_line(&tops, 100, 3, px(999.)), 102);
        // No rows: the base line.
        assert_eq!(local_line(&[px(0.)], 7, 0, px(5.)), 7);
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
        assert_eq!(layout.rows[0].lines[0].unwrapped_layout.font_size, px(10.));
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
}
