use gpui::{
    canvas, point, prelude::*, px, FillOptions, FillRule, Hsla, PathBuilder, PathStyle, Pixels,
};
use std::f32::consts::PI;

/// A small circular progress indicator that shows context window usage.
///
/// Renders as a ring: a muted background circle with a colored arc overlay
/// representing the filled portion.
///
/// Both the background ring and the progress arc are drawn as filled annular
/// shapes (outer circle minus inner circle) rather than stroked paths.
/// This avoids anti-aliasing artefacts caused by stroke line-cap styles and
/// overlapping semi-transparent strokes.
#[derive(IntoElement)]
pub struct ContextIndicator {
    /// Progress value in 0.0..=1.0
    ratio: f32,
    /// Outer diameter of the indicator
    size: Pixels,
    /// Width of the ring stroke
    stroke_width: Pixels,
    /// Color for the unfilled background ring
    bg_color: Hsla,
    /// Color for the filled progress arc
    progress_color: Hsla,
}

impl ContextIndicator {
    pub fn new(ratio: f32) -> Self {
        Self {
            ratio: ratio.clamp(0.0, 1.0),
            size: px(16.),
            stroke_width: px(2.5),
            bg_color: gpui::hsla(0., 0., 0.5, 0.25),
            progress_color: gpui::hsla(0., 0., 0.7, 0.9),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn stroke_width(mut self, width: Pixels) -> Self {
        self.stroke_width = width;
        self
    }

    pub fn bg_color(mut self, color: Hsla) -> Self {
        self.bg_color = color;
        self
    }

    pub fn progress_color(mut self, color: Hsla) -> Self {
        self.progress_color = color;
        self
    }
}

/// Number of line segments used to approximate a full circle.
const CIRCLE_SEGMENTS: usize = 64;

/// Build a filled full-circle annulus (ring) path.
///
/// The ring is defined by an outer and inner radius, centred at (`cx`, `cy`).
/// Uses a polygon approximation with `CIRCLE_SEGMENTS` segments for each
/// circle, connected so the fill rule produces a ring.
fn build_ring(center_x: Pixels, center_y: Pixels, outer_r: Pixels, inner_r: Pixels) -> PathBuilder {
    let fill_opts = FillOptions::default().with_fill_rule(FillRule::EvenOdd);
    let mut pb = PathBuilder::fill().with_style(PathStyle::Fill(fill_opts));

    // Outer circle – clockwise
    add_circle(&mut pb, center_x, center_y, outer_r, true);
    // Inner circle – counter-clockwise
    add_circle(&mut pb, center_x, center_y, inner_r, false);

    pb
}

/// Append a closed circular polygon to the path builder.
fn add_circle(pb: &mut PathBuilder, cx: Pixels, cy: Pixels, r: Pixels, cw: bool) {
    let n = CIRCLE_SEGMENTS;
    for i in 0..=n {
        let t = if cw {
            2.0 * PI * (i as f32) / (n as f32)
        } else {
            -2.0 * PI * (i as f32) / (n as f32)
        };
        let px = cx + r * t.cos();
        let py = cy + r * t.sin();
        if i == 0 {
            pb.move_to(point(px, py));
        } else {
            pb.line_to(point(px, py));
        }
    }
    pb.close();
}

/// Build a filled arc sector (annular wedge) from `start_angle` to
/// `end_angle` (in radians, 0 = 3 o'clock, positive = clockwise in screen
/// coords).
fn build_arc_sector(
    center_x: Pixels,
    center_y: Pixels,
    outer_r: Pixels,
    inner_r: Pixels,
    start_angle: f32,
    end_angle: f32,
) -> PathBuilder {
    let mut pb = PathBuilder::fill();

    let n = CIRCLE_SEGMENTS;
    let span = end_angle - start_angle;

    // Outer arc: start → end
    for i in 0..=n {
        let t = start_angle + span * (i as f32) / (n as f32);
        let px = center_x + outer_r * t.cos();
        let py = center_y + outer_r * t.sin();
        if i == 0 {
            pb.move_to(point(px, py));
        } else {
            pb.line_to(point(px, py));
        }
    }

    // Inner arc: end → start (reverse direction to close the shape)
    for i in 0..=n {
        let t = end_angle - span * (i as f32) / (n as f32);
        let px = center_x + inner_r * t.cos();
        let py = center_y + inner_r * t.sin();
        pb.line_to(point(px, py));
    }

    pb.close();
    pb
}

impl RenderOnce for ContextIndicator {
    fn render(self, _window: &mut gpui::Window, _cx: &mut gpui::App) -> impl IntoElement {
        let ratio = self.ratio;
        let stroke_width = self.stroke_width;
        let bg_color = self.bg_color;
        let progress_color = self.progress_color;
        let size = self.size;

        canvas(
            |_, _, _| {},
            move |bounds, _, window, _cx| {
                let cx = bounds.origin.x + bounds.size.width / 2.0;
                let cy = bounds.origin.y + bounds.size.height / 2.0;

                // Outer and inner radii – ensure the full ring stays within
                // the element bounds with a 0.5 px margin for anti-aliasing.
                let half = size / 2.0;
                let outer_r = half - px(0.5);
                let inner_r = outer_r - stroke_width;

                // --- Background ring (full circle) ---
                if let Ok(path) = build_ring(cx, cy, outer_r, inner_r).build() {
                    window.paint_path(path, bg_color);
                }

                // --- Progress arc ---
                if ratio > 0.001 {
                    // Start at 12 o'clock (-π/2) and sweep clockwise.
                    let start = -PI / 2.0;
                    let end = start + ratio.min(0.999) * 2.0 * PI;

                    if let Ok(path) = build_arc_sector(cx, cy, outer_r, inner_r, start, end).build()
                    {
                        window.paint_path(path, progress_color);
                    }
                }
            },
        )
        .size(size)
    }
}
