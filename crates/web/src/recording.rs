//! Recordings: frames a page painted over a stretch of real time, laid out as
//! one contact sheet so a model can judge motion from a single image.
//!
//! [`crate::Tab::record`] collects the frames (CDP screencast); this module
//! picks which frame each cell shows and draws the sheet.

use crate::tab::MAX_SCREENSHOT_EDGE;
use anyhow::Result;
use image::{Rgba, RgbaImage, imageops};
use std::io::Cursor;

/// Pixels between cells and around the sheet.
const GAP: u32 = 4;
/// Height of the label strip above each cell.
const BAND: u32 = 18;
/// Glyphs are drawn at this many pixels per font pixel.
const GLYPH_SCALE: u32 = 2;
const BACKGROUND: Rgba<u8> = Rgba([40, 40, 40, 255]);
const LABEL: Rgba<u8> = Rgba([255, 255, 255, 255]);
/// Labels of cells that repeat the previous one are dimmed.
const LABEL_REPEATED: Rgba<u8> = Rgba([140, 140, 140, 255]);

/// A recording laid out as a contact sheet, cells left to right, top to
/// bottom.
pub struct Recording {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
    /// Size of one cell (one frame) in the sheet.
    pub cell_width: u32,
    pub cell_height: u32,
    /// One per cell.
    pub frames: Vec<SheetFrame>,
    /// Frames the browser sent: it only sends one when the page repaints.
    pub received: usize,
}

/// What one cell of the sheet shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SheetFrame {
    /// Seconds after the start of the recording that the cell stands for.
    pub at: f64,
    /// The cell shows the same frame as the one before: nothing was painted
    /// in between.
    pub repeated: bool,
}

impl Recording {
    /// `(x, y, width, height)` of cell `i`'s frame in the sheet (below its
    /// label).
    pub fn cell_rect(&self, i: usize) -> (u32, u32, u32, u32) {
        let (col, row) = (i as u32 % self.columns, i as u32 / self.columns);
        (
            GAP + col * (self.cell_width + GAP),
            GAP + row * (BAND + self.cell_height + GAP) + BAND,
            self.cell_width,
            self.cell_height,
        )
    }
}

/// Columns and rows for `n` cells: as square as possible, wider than tall.
pub(crate) fn grid(n: u32) -> (u32, u32) {
    let columns = (n as f64).sqrt().ceil() as u32;
    (columns, n.div_ceil(columns))
}

/// The size of one cell for a `vw`×`vh` (CSS px) viewport: `scale` if given,
/// reduced so the whole sheet stays within [`MAX_SCREENSHOT_EDGE`].
pub(crate) fn cell_size(
    vw: f64,
    vh: f64,
    columns: u32,
    rows: u32,
    scale: Option<f64>,
) -> (u32, u32) {
    let edge = MAX_SCREENSHOT_EDGE as f64;
    let fit_w = (edge - ((columns + 1) * GAP) as f64) / columns as f64 / vw;
    let fit_h = (edge - ((rows + 1) * GAP + rows * BAND) as f64) / rows as f64 / vh;
    let fit = fit_w.min(fit_h);
    let scale = scale.filter(|s| *s > 0.0).map_or(fit, |s| s.min(fit));
    (
        ((vw * scale).floor() as u32).max(1),
        ((vh * scale).floor() as u32).max(1),
    )
}

/// The moments the cells stand for: `n` evenly spread from the start to the
/// end of `duration` seconds.
pub(crate) fn cell_times(n: usize, duration: f64) -> Vec<f64> {
    (0..n)
        .map(|i| duration * i as f64 / (n - 1).max(1) as f64)
        .collect()
}

/// For each moment in `times`, the frame on screen then: the last one painted
/// at or before it, else (before the first paint) the first. `frame_times`
/// are ascending.
pub(crate) fn pick_frames(frame_times: &[f64], times: &[f64]) -> Vec<usize> {
    times
        .iter()
        .map(|t| {
            frame_times
                .iter()
                .rposition(|f| *f <= *t + 1e-6)
                .unwrap_or(0)
        })
        .collect()
}

/// Lay out `frames` (encoded images, decoded here) for the cells at `times`
/// as a contact sheet of `columns`×`rows` cells of `cell` size.
pub(crate) fn contact_sheet(
    frames: &[(f64, Vec<u8>)],
    times: &[f64],
    columns: u32,
    rows: u32,
    cell: (u32, u32),
) -> Result<Recording> {
    anyhow::ensure!(!frames.is_empty(), "the page sent no frames");
    let (cw, ch) = cell;
    let width = GAP + columns * (cw + GAP);
    let height = GAP + rows * (BAND + ch + GAP);
    let mut sheet = RgbaImage::from_pixel(width, height, BACKGROUND);

    let frame_times: Vec<f64> = frames.iter().map(|(t, _)| *t).collect();
    let picks = pick_frames(&frame_times, times);
    let mut recording = Recording {
        png: Vec::new(),
        width,
        height,
        columns,
        rows,
        cell_width: cw,
        cell_height: ch,
        frames: Vec::with_capacity(times.len()),
        received: frames.len(),
    };
    let mut decoded: Option<(usize, RgbaImage)> = None;
    for (i, (&at, &pick)) in times.iter().zip(&picks).enumerate() {
        let repeated = i > 0 && picks[i - 1] == pick;
        if decoded.as_ref().is_none_or(|(n, _)| *n != pick) {
            let mut img = image::load_from_memory(&frames[pick].1)?.to_rgba8();
            if img.dimensions() != (cw, ch) {
                img = imageops::resize(&img, cw, ch, imageops::FilterType::Triangle);
            }
            decoded = Some((pick, img));
        }
        let (x, y, _, _) = recording.cell_rect(i);
        let img = &decoded.as_ref().expect("decoded above").1;
        imageops::replace(&mut sheet, img, x as i64, y as i64);
        let color = if repeated { LABEL_REPEATED } else { LABEL };
        draw_text(
            &mut sheet,
            x + 2,
            y - BAND + 2,
            &format!("#{} +{at:.2}s", i + 1),
            color,
        );
        recording.frames.push(SheetFrame { at, repeated });
    }

    let mut png = Vec::new();
    sheet.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
    recording.png = png;
    Ok(recording)
}

/// Draw `text` with its top-left at `(x, y)`; characters without a glyph
/// are skipped (left blank).
fn draw_text(img: &mut RgbaImage, x: u32, y: u32, text: &str, color: Rgba<u8>) {
    let advance = (GLYPH_WIDTH + 1) * GLYPH_SCALE;
    for (n, c) in text.chars().enumerate() {
        let Some(rows) = glyph(c) else { continue };
        let gx = x + n as u32 * advance;
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_WIDTH {
                if bits & (1 << (GLYPH_WIDTH - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..GLYPH_SCALE {
                    for dx in 0..GLYPH_SCALE {
                        let (px, py) = (
                            gx + col * GLYPH_SCALE + dx,
                            y + row as u32 * GLYPH_SCALE + dy,
                        );
                        if px < img.width() && py < img.height() {
                            img.put_pixel(px, py, color);
                        }
                    }
                }
            }
        }
    }
}

const GLYPH_WIDTH: u32 = 5;

/// A 5×7 bitmap glyph, one byte per row, high bit left — just the
/// characters labels use.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        's' => [0x00, 0x00, 0x0E, 0x10, 0x0E, 0x01, 0x1E],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grids_are_square_or_wider() {
        assert_eq!(grid(9), (3, 3));
        assert_eq!(grid(4), (2, 2));
        assert_eq!(grid(6), (3, 2));
        assert_eq!(grid(2), (2, 1));
        assert_eq!(grid(16), (4, 4));
    }

    #[test]
    fn cells_fit_the_sheet_into_the_edge_limit() {
        let (cw, ch) = cell_size(1280.0, 800.0, 3, 3, None);
        assert!(GAP + 3 * (cw + GAP) <= MAX_SCREENSHOT_EDGE);
        assert!(GAP + 3 * (BAND + ch + GAP) <= MAX_SCREENSHOT_EDGE);
        assert!(cw > 500, "{cw}");
        // A smaller scale is taken as asked, a larger one is capped.
        assert_eq!(cell_size(1280.0, 800.0, 3, 3, Some(0.25)), (320, 200));
        assert_eq!(cell_size(1280.0, 800.0, 3, 3, Some(2.0)), (cw, ch));
    }

    #[test]
    fn each_moment_shows_the_last_frame_painted_by_then() {
        assert_eq!(cell_times(5, 1.0), [0.0, 0.25, 0.5, 0.75, 1.0]);
        let frames = [0.01, 0.2, 0.3, 0.9];
        assert_eq!(
            pick_frames(&frames, &cell_times(5, 1.0)),
            [0, 1, 2, 2, 3],
            "before the first paint the first frame stands in"
        );
    }
}
