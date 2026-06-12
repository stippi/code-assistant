use base64::{
    alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine as _,
};
use gpui::{Image, ImageFormat};
use image;
use std::sync::Arc;

/// Base64 decoder that's lenient about padding
const STANDARD_INDIFFERENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Parse base64 image data into a GPUI Image
pub fn parse_base64_image(media_type: &str, data: &str) -> Option<Arc<Image>> {
    // Remove whitespace from base64 data
    let filtered = data.replace(&[' ', '\n', '\t', '\r', '\x0b', '\x0c'][..], "");

    // Decode base64
    let bytes = STANDARD_INDIFFERENT.decode(filtered).ok()?;

    // Determine image format from media type
    let format = match media_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        "image/tiff" => ImageFormat::Tiff,
        "image/bmp" => ImageFormat::Bmp,
        _ => {
            // Try to guess the format from the bytes
            match image::guess_format(&bytes).ok()? {
                image::ImageFormat::Png => ImageFormat::Png,
                image::ImageFormat::Jpeg => ImageFormat::Jpeg,
                image::ImageFormat::Gif => ImageFormat::Gif,
                image::ImageFormat::WebP => ImageFormat::Webp,
                image::ImageFormat::Tiff => ImageFormat::Tiff,
                image::ImageFormat::Bmp => ImageFormat::Bmp,
                _ => return None,
            }
        }
    };

    // Create GPUI Image
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_base64_image_png() {
        // Simple 1x1 transparent PNG in base64
        let png_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChAI9jU77zgAAAABJRU5ErkJggg==";
        let image = parse_base64_image("image/png", png_data);

        assert!(image.is_some());
        let image = image.unwrap();
        assert_eq!(image.format, ImageFormat::Png);
    }

    #[test]
    fn test_parse_base64_image_invalid_data() {
        let invalid_data = "not-valid-base64!@#$";
        let image = parse_base64_image("image/png", invalid_data);

        assert!(image.is_none());
    }

    #[test]
    fn test_parse_base64_image_unknown_format() {
        // Valid base64 but not a valid image
        let invalid_image_data = "SGVsbG8gV29ybGQ="; // "Hello World" in base64
        let image = parse_base64_image("image/unknown", invalid_image_data);

        assert!(image.is_none());
    }

    #[test]
    fn test_parse_base64_image_media_type_detection() {
        // Simple 1x1 transparent PNG in base64
        let png_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChAI9jU77zgAAAABJRU5ErkJggg==";

        // Test with correct media type
        let image1 = parse_base64_image("image/png", png_data);
        assert!(image1.is_some());
        assert_eq!(image1.unwrap().format, ImageFormat::Png);

        // Test with incorrect media type - should still work due to format guessing
        let image2 = parse_base64_image("image/unknown", png_data);
        assert!(image2.is_some());
        // The format should be detected from the actual image data
        let detected_format = image2.unwrap().format;
        // PNG format should be detected
        assert_eq!(detected_format, ImageFormat::Png);
    }

    #[test]
    fn test_image_creation() {
        // Simple 1x1 transparent PNG in base64
        let png_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChAI9jU77zgAAAABJRU5ErkJggg==";

        // Create an image
        let image = parse_base64_image("image/png", png_data);

        // Verify the image was parsed successfully
        assert!(image.is_some());
        let image = image.unwrap();
        assert_eq!(image.format, ImageFormat::Png);
    }
}
