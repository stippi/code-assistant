//! Between a frame as the panel draws it and the page it shows.
//!
//! A screencast frame covers `device_width`×`device_height` DIP, the page
//! starting `offset_top` DIP down and zoomed by `page_scale_factor`. The panel
//! draws the frame scaled into a `view` size; input events and the agent's
//! clicks are in the page's CSS pixels.

use web::FrameMetadata;

/// The page point (CSS px) under `at` (view px from the frame's top left),
/// `None` outside the frame.
pub fn to_page(meta: &FrameMetadata, view: (f32, f32), at: (f32, f32)) -> Option<(f64, f64)> {
    let (vw, vh) = (view.0 as f64, view.1 as f64);
    let (x, y) = (at.0 as f64, at.1 as f64);
    if vw <= 0.0 || vh <= 0.0 || !(0.0..vw).contains(&x) || !(0.0..vh).contains(&y) {
        return None;
    }
    let scale = meta.page_scale_factor.max(f64::EPSILON);
    let dip_x = x / vw * meta.device_width;
    let dip_y = y / vh * meta.device_height - meta.offset_top;
    (dip_y >= 0.0).then(|| (dip_x / scale, dip_y / scale))
}

/// Where the page point `css` (CSS px) is drawn, in view px.
pub fn to_view(meta: &FrameMetadata, view: (f32, f32), css: (f64, f64)) -> (f32, f32) {
    let scale = meta.page_scale_factor;
    let x = css.0 * scale / meta.device_width * view.0 as f64;
    let y = (css.1 * scale + meta.offset_top) / meta.device_height * view.1 as f64;
    (x as f32, y as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(offset_top: f64, page_scale_factor: f64) -> FrameMetadata {
        FrameMetadata {
            device_width: 1280.0,
            device_height: 800.0,
            offset_top,
            page_scale_factor,
            scroll_x: 0.0,
            scroll_y: 0.0,
            painted_at: None,
        }
    }

    #[test]
    fn a_frame_drawn_at_half_size_maps_to_double_css_pixels() {
        let m = meta(0.0, 1.0);
        let view = (640.0, 400.0);
        assert_eq!(to_page(&m, view, (320.0, 100.0)), Some((640.0, 200.0)));
        assert_eq!(to_view(&m, view, (640.0, 200.0)), (320.0, 100.0));
    }

    #[test]
    fn offset_and_pinch_zoom_are_undone() {
        let m = meta(40.0, 2.0);
        let view = (1280.0, 800.0);
        // 140 DIP down is 100 DIP into the page, 50 CSS px at 2× zoom.
        assert_eq!(to_page(&m, view, (200.0, 140.0)), Some((100.0, 50.0)));
        assert_eq!(to_view(&m, view, (100.0, 50.0)), (200.0, 140.0));
        assert_eq!(to_page(&m, view, (200.0, 20.0)), None, "above the page");
    }

    #[test]
    fn points_outside_the_frame_are_not_on_the_page() {
        let m = meta(0.0, 1.0);
        let view = (640.0, 400.0);
        assert_eq!(to_page(&m, view, (-1.0, 10.0)), None);
        assert_eq!(to_page(&m, view, (10.0, 400.0)), None);
        assert_eq!(to_page(&m, (0.0, 0.0), (0.0, 0.0)), None);
    }
}
