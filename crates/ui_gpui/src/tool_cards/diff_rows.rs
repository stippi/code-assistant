//! The rows of a diff as one element: backgrounds, gutter numbers and
//! wrapped text painted directly, instead of three or four layout nodes per
//! row. With a diff card in view, laying those nodes out was the largest
//! part of a frame (see docs/frame-profiling.md); here taffy sees one node
//! per chunk and the text system's line cache does the rest.

use gpui_kit::{
    App, AvailableSpace, Bounds, Element, GlobalElementId, HighlightStyle, Hsla,
    InspectorElementId, IntoElement, LayoutId, Length, Pixels, Point, ShapedLine, SharedString,
    Size, Style, TextAlign, TextRun, TextStyle, Window, WrappedLine, fill, point, px, relative,
    size,
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

pub(crate) struct DiffRows {
    rows: Rc<Vec<DiffRow>>,
    geometry: RowGeometry,
}

impl DiffRows {
    pub fn new(rows: Vec<DiffRow>, geometry: RowGeometry) -> Self {
        Self {
            rows: Rc::new(rows),
            geometry,
        }
    }
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

fn layout_rows(
    rows: &[DiffRow],
    geometry: RowGeometry,
    width: Option<Pixels>,
    window: &mut Window,
) -> RowsLayout {
    let text_style = window.text_style();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
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
    window: &mut Window,
    cx: &mut App,
) {
    let line_height = layout.line_height;
    let mut y = bounds.origin.y;
    for (row, row_layout) in rows.iter().zip(&layout.rows) {
        if let Some(background) = row.background {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, y),
                    size(bounds.size.width, row_layout.height),
                ),
                background,
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
    type PrepaintState = ();

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
        let cell: LayoutCell = Rc::default();
        let rows = self.rows.clone();
        let geometry = self.geometry;
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
                let layout = layout_rows(&rows, geometry, width, window);
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
        _: Bounds<Pixels>,
        _: &mut LayoutCell,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        cell: &mut LayoutCell,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(layout) = cell.borrow().as_ref() {
            paint_rows(&self.rows, layout, self.geometry, bounds, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{Context, Render, TestAppContext, VisualTestContext, div, red, white};

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
    fn rows_take_one_line_each_and_wrap_when_narrow(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Root);
        let cx: &mut VisualTestContext = cx;
        let long = "x".repeat(200);
        let (cell, ()) = cx.draw(point(px(0.), px(0.)), size(px(600.), px(400.)), |_, _| {
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

        let (cell, ()) = cx.draw(point(px(0.), px(0.)), size(px(120.), px(400.)), |_, _| {
            DiffRows::new(rows(&[long.as_str()]), GEOMETRY)
        });
        let layout = cell.borrow();
        let layout = layout.as_ref().expect("measured");
        let lines = layout.rows[0].line_count();
        assert!(lines > 1, "{lines} lines");
        assert_eq!(layout.height, layout.line_height * lines as f32);
    }
}
