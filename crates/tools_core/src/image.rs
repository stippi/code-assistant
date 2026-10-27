//! Image dimension limiting for tool outputs and user attachments.
//!
//! Images reach an LLM from several sources — user attachments/screenshots and
//! tools that produce visual output (`view_images`, the browser screenshot
//! tools, image-returning MCP tools). Providers cap image dimensions server
//! side anyway (Anthropic, for instance, downsizes anything whose longest edge
//! exceeds ~1568px), so uploading larger images just wastes bandwidth and
//! tokens.
//!
//! We therefore shrink oversized images **once, at the point they are
//! created**, so the bounded version is what gets stored and re-sent on every
//! subsequent turn — no repeated decode/resize work as the conversation grows.
//! This helper lives in `tools_core` (next to [`crate::ImageData`]) so every
//! producer — including MCP tools, which don't depend on the `llm` crate — can
//! share one implementation.
//!
//! SVG drawings, which models do not take, are rasterized to PNG here
//! ([`svg_to_png`]); the GUI draws them with the same parser and fonts
//! ([`parse_svg`]).

use base64::Engine as _;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader};
pub use resvg;
use resvg::{tiny_skia, usvg};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Maximum length, in pixels, of an image's longer edge that we forward to an
/// LLM. Mirrors Anthropic's recommended maximum long edge.
pub const MAX_IMAGE_EDGE: u32 = 1568;

/// Base64 engine used for image payloads.
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Cap a base64-encoded image so neither edge exceeds `max_edge`, preserving
/// aspect ratio.
///
/// Returns `Some((media_type, base64_data))` with the re-encoded image when it
/// was actually shrunk, and `None` when nothing changed — either because the
/// image already fits, or because it could not be decoded (in which case the
/// caller keeps the original bytes; we never drop an image just because we
/// failed to resize it).
///
/// The returned media type usually matches the input; formats we cannot
/// re-encode (e.g. WebP/TIFF) are converted to PNG.
pub fn cap_base64_image(
    media_type: &str,
    base64_data: &str,
    max_edge: u32,
) -> Option<(String, String)> {
    if max_edge == 0 {
        return None;
    }

    // base64 payloads from some sources contain incidental whitespace.
    let cleaned: String = base64_data
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let bytes = BASE64.decode(cleaned.as_bytes()).ok()?;

    // Cheap path: read only the header to learn the dimensions. If the image
    // already fits, avoid decoding the pixels entirely.
    let (width, height) = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;

    if width <= max_edge && height <= max_edge {
        return None;
    }

    // Oversized: decode, downscale to fit the box (aspect ratio preserved),
    // and re-encode.
    let decoded = image::load_from_memory(&bytes).ok()?;
    let resized = decoded.resize(max_edge, max_edge, FilterType::Lanczos3);
    let (out_media_type, out_bytes) = encode(resized, media_type)?;
    Some((out_media_type.to_string(), BASE64.encode(out_bytes)))
}

/// Convenience wrapper that mutates an [`ImageData`] in place, shrinking it if
/// it exceeds `max_edge`.
pub fn cap_image_data(image: &mut crate::ImageData, max_edge: u32) {
    if let Some((media_type, base64_data)) =
        cap_base64_image(&image.media_type, &image.base64_data, max_edge)
    {
        image.media_type = media_type;
        image.base64_data = base64_data;
    }
}

/// The shortest an SVG drawing's longer edge is rasterized at for a model,
/// so an icon shows large enough to make out.
pub const MIN_SVG_EDGE: u32 = 1024;

/// The system fonts, for text in SVG drawings; loaded once, when first
/// needed.
pub fn svg_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Parse an SVG drawing; relative references (embedded images) resolve
/// against `resources_dir`. The system fonts are only loaded for a drawing
/// with text in it.
pub fn parse_svg(data: &[u8], resources_dir: Option<PathBuf>) -> anyhow::Result<usvg::Tree> {
    let has_text = data.windows(5).any(|w| w == b"<text");
    let mut options = usvg::Options {
        resources_dir,
        ..Default::default()
    };
    if has_text {
        options.fontdb = svg_fonts();
    }
    Ok(usvg::Tree::from_data(data, &options)?)
}

/// An SVG drawing as PNG on white, for a model: its longer edge scaled to
/// at least [`MIN_SVG_EDGE`] and at most `max_edge`.
pub fn svg_to_png(
    data: &[u8],
    resources_dir: Option<PathBuf>,
    max_edge: u32,
) -> anyhow::Result<Vec<u8>> {
    let tree = parse_svg(data, resources_dir)?;
    let natural = tree.size();
    let longest = natural.width().max(natural.height());
    let edge = longest.clamp(MIN_SVG_EDGE.min(max_edge) as f32, max_edge.max(1) as f32);
    let scale = edge / longest;
    let width = (natural.width() * scale).round().max(1.) as u32;
    let height = (natural.height() * scale).round().max(1.) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("cannot draw a {width}×{height} image"))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // Opaque, so the premultiplied pixels are the plain ones.
    let image = image::RgbaImage::from_raw(width, height, pixmap.take())
        .ok_or_else(|| anyhow::anyhow!("pixel buffer of the wrong size"))?;
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(image).write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

/// Re-encode a resized image, keeping the source format where we can and
/// falling back to PNG otherwise. Returns the (media_type, bytes) pair.
fn encode(img: DynamicImage, media_type: &str) -> Option<(&'static str, Vec<u8>)> {
    let (format, out_media_type) = match media_type.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => (ImageFormat::Jpeg, "image/jpeg"),
        "image/png" => (ImageFormat::Png, "image/png"),
        "image/gif" => (ImageFormat::Gif, "image/gif"),
        "image/bmp" => (ImageFormat::Bmp, "image/bmp"),
        // WebP/TIFF/unknown: re-encode as PNG (lossless, always supported).
        _ => (ImageFormat::Png, "image/png"),
    };

    // JPEG has no alpha channel; drop it to avoid an encode error.
    let img = if matches!(format, ImageFormat::Jpeg) {
        DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img
    };

    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), format).ok()?;
    Some((out_media_type, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a solid-colour RGBA image of the given size as a base64 PNG.
    fn png_base64(width: u32, height: u32) -> String {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([10, 20, 30, 255]),
        ));
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        BASE64.encode(bytes)
    }

    fn dimensions_of(base64_data: &str) -> (u32, u32) {
        let bytes = BASE64.decode(base64_data).unwrap();
        ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap()
    }

    #[test]
    fn svg_drawings_become_pngs_large_enough_to_see() {
        let icon = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8">
            <rect width="8" height="8" fill="black"/></svg>"#;
        let png = svg_to_png(icon, None, MAX_IMAGE_EDGE).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (1024, 512));
        assert_eq!(img.get_pixel(10, 10).0, [0, 0, 0, 255], "the drawing");
        assert_eq!(
            img.get_pixel(1000, 10).0,
            [255, 255, 255, 255],
            "white behind"
        );

        let poster = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="1000"/>"#;
        let png = svg_to_png(poster, None, MAX_IMAGE_EDGE).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (MAX_IMAGE_EDGE, 392));

        assert!(svg_to_png(b"<not svg", None, MAX_IMAGE_EDGE).is_err());
    }

    #[test]
    fn leaves_small_images_untouched() {
        let data = png_base64(800, 600);
        assert!(cap_base64_image("image/png", &data, MAX_IMAGE_EDGE).is_none());
    }

    #[test]
    fn shrinks_oversized_landscape_preserving_aspect_ratio() {
        let data = png_base64(4000, 2000);
        let (media_type, capped) =
            cap_base64_image("image/png", &data, MAX_IMAGE_EDGE).expect("should be resized");
        assert_eq!(media_type, "image/png");
        let (w, h) = dimensions_of(&capped);
        assert_eq!(w, MAX_IMAGE_EDGE);
        assert_eq!(h, MAX_IMAGE_EDGE / 2);
    }

    #[test]
    fn shrinks_oversized_tall_image() {
        // A tall full-page-style screenshot: long edge is the height.
        let data = png_base64(1000, 5000);
        let (_media_type, capped) =
            cap_base64_image("image/png", &data, MAX_IMAGE_EDGE).expect("should be resized");
        let (w, h) = dimensions_of(&capped);
        assert!(w <= MAX_IMAGE_EDGE && h <= MAX_IMAGE_EDGE);
        assert_eq!(h, MAX_IMAGE_EDGE);
    }

    #[test]
    fn jpeg_stays_jpeg() {
        // Build an oversized JPEG source.
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3000,
            1000,
            image::Rgb([200, 100, 50]),
        ));
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        let data = BASE64.encode(bytes);

        let (media_type, capped) =
            cap_base64_image("image/jpeg", &data, MAX_IMAGE_EDGE).expect("should be resized");
        assert_eq!(media_type, "image/jpeg");
        let (w, _h) = dimensions_of(&capped);
        assert_eq!(w, MAX_IMAGE_EDGE);
    }

    #[test]
    fn invalid_data_is_left_unchanged() {
        assert!(cap_base64_image("image/png", "not base64 image data", MAX_IMAGE_EDGE).is_none());
    }

    #[test]
    fn zero_max_edge_is_a_noop() {
        let data = png_base64(4000, 2000);
        assert!(cap_base64_image("image/png", &data, 0).is_none());
    }

    #[test]
    fn cap_image_data_shrinks_in_place() {
        let mut image = crate::ImageData {
            media_type: "image/png".to_string(),
            base64_data: png_base64(4000, 2000),
        };
        cap_image_data(&mut image, MAX_IMAGE_EDGE);
        let (w, h) = dimensions_of(&image.base64_data);
        assert_eq!((w, h), (MAX_IMAGE_EDGE, MAX_IMAGE_EDGE / 2));
    }
}
