use gpui::{canvas, point, prelude::*, px, Hsla, PathBuilder, Pixels};
use std::f32::consts::PI;

/// A small circular progress indicator that shows context window usage.
///
/// Renders as a ring: a muted background circle with a colored arc overlay
/// representing the filled portion.
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
                let center_x = bounds.origin.x + bounds.size.width / 2.0;
                let center_y = bounds.origin.y + bounds.size.height / 2.0;
                let radius = (size / 2.0) - stroke_width;

                // --- Background ring (full circle via two semicircles) ---
                let mut bg = PathBuilder::stroke(stroke_width);
                bg.move_to(point(center_x + radius, center_y));
                bg.arc_to(
                    point(radius, radius),
                    px(0.),
                    false,
                    true,
                    point(center_x - radius, center_y),
                );
                bg.arc_to(
                    point(radius, radius),
                    px(0.),
                    false,
                    true,
                    point(center_x + radius, center_y),
                );
                if let Ok(path) = bg.build() {
                    window.paint_path(path, bg_color);
                }

                // --- Progress arc ---
                if ratio > 0.0 {
                    let mut pb = PathBuilder::stroke(stroke_width);
                    if ratio >= 0.999 {
                        // Full circle
                        pb.move_to(point(center_x + radius, center_y));
                        pb.arc_to(
                            point(radius, radius),
                            px(0.),
                            false,
                            true,
                            point(center_x - radius, center_y),
                        );
                        pb.arc_to(
                            point(radius, radius),
                            px(0.),
                            false,
                            true,
                            point(center_x + radius, center_y),
                        );
                    } else {
                        // Partial arc starting from 12 o'clock (top center)
                        let start_x = center_x;
                        let start_y = center_y - radius;
                        pb.move_to(point(start_x, start_y));

                        let angle = -PI / 2.0 + (ratio * 2.0 * PI);

                        let end_x = center_x + radius * angle.cos();
                        let end_y = center_y + radius * angle.sin();
                        let large_arc = ratio > 0.5;

                        pb.arc_to(
                            point(radius, radius),
                            px(0.),
                            large_arc,
                            true,
                            point(end_x, end_y),
                        );
                    }
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, progress_color);
                    }
                }
            },
        )
        .size(size)
    }
}
