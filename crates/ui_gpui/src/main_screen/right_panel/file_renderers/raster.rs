//! Raster images (PNG, JPEG, GIF, WebP, …) drawn, scaled down to fit the
//! view and never up.
//!
//! The text editor cannot hold these files, so the view reads the file
//! itself, in the background, and again when it changes on disk. GPUI
//! decodes it (animated GIFs play). A line under the image tells its size
//! and format.
//!
//! As for SVG, the image starts on a white page; a button in the corner
//! switches to the theme's background.

use super::{FileRenderer, RendererHandle, RendererView, background_button, has_extension};
use crate::comments::CommentChange;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, Context, EventEmitter, Image, ImageFormat, ImageSource, ObjectFit, Render, StyledImage,
    Task, Window, div, img, prelude::*, px, white,
};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const RENDERER: FileRenderer = FileRenderer {
    id: "raster",
    label: "Image",
    accepts: |path| has_extension(path, EXTENSIONS),
    reads_file: true,
    create,
};

/// The extensions of the formats GPUI decodes.
const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "pbm", "pgm", "ppm", "pnm",
];

fn create(
    file: PathBuf,
    _root: PathBuf,
    _window: &mut Window,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|cx| {
        let mut view = RasterPreview::new(file);
        view.reload(cx);
        view
    }))
}

/// Space around the image.
const PADDING: gpui_kit::Pixels = px(12.);

/// An image file as read from disk.
struct Picture {
    image: Arc<Image>,
    width: u32,
    height: u32,
    format: &'static str,
    file_size: u64,
}

/// What the view shows.
enum Content {
    Reading,
    Shown(Picture),
    Invalid(String),
}

pub struct RasterPreview {
    file: PathBuf,
    content: Content,
    /// Images replaced, to be dropped from GPUI's caches.
    stale: Vec<Arc<Image>>,
    /// The theme's background behind the image instead of white.
    themed_background: bool,
    read_task: Option<Task<()>>,
}

impl EventEmitter<CommentChange> for RasterPreview {}

impl RasterPreview {
    fn new(file: PathBuf) -> Self {
        Self {
            file,
            content: Content::Reading,
            stale: Vec::new(),
            themed_background: false,
            read_task: None,
        }
    }
}

/// Read the image file at `file`: its bytes for GPUI to decode, and what
/// the line under it tells.
fn read_picture(file: &Path) -> Result<Picture, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (format, name) = match reader.format() {
        Some(image::ImageFormat::Png) => (ImageFormat::Png, "PNG"),
        Some(image::ImageFormat::Jpeg) => (ImageFormat::Jpeg, "JPEG"),
        Some(image::ImageFormat::Gif) => (ImageFormat::Gif, "GIF"),
        Some(image::ImageFormat::WebP) => (ImageFormat::Webp, "WebP"),
        Some(image::ImageFormat::Bmp) => (ImageFormat::Bmp, "BMP"),
        Some(image::ImageFormat::Tiff) => (ImageFormat::Tiff, "TIFF"),
        Some(image::ImageFormat::Ico) => (ImageFormat::Ico, "ICO"),
        Some(image::ImageFormat::Pnm) => (ImageFormat::Pnm, "PNM"),
        _ => return Err("unknown image format".into()),
    };
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    let file_size = bytes.len() as u64;
    Ok(Picture {
        image: Arc::new(Image::from_bytes(format, bytes)),
        width,
        height,
        format: name,
        file_size,
    })
}

/// A file size for people: bytes, KB or MB.
fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

impl RendererView for RasterPreview {
    fn set_text(&mut self, _text: &str, _cx: &mut Context<Self>) {}

    fn reload(&mut self, cx: &mut Context<Self>) {
        let file = self.file.clone();
        let job = cx
            .background_executor()
            .spawn(async move { read_picture(&file) });
        self.read_task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                let content = match result {
                    Ok(picture) => Content::Shown(picture),
                    Err(e) => Content::Invalid(e),
                };
                if let Content::Shown(old) = &this.content {
                    // The same bytes are the same cached image.
                    if matches!(&content, Content::Shown(new) if new.image.id() == old.image.id()) {
                        return;
                    }
                    this.stale.push(old.image.clone());
                }
                this.content = content;
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for RasterPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.stale.drain(..) {
            if let Some(render_image) = image.clone().get_render_image(window, cx) {
                window.drop_image(render_image).ok();
            }
            image.remove_asset(cx);
        }

        let theme = cx.theme();
        let background = if self.themed_background {
            theme.background
        } else {
            white()
        };
        let muted = theme.muted_foreground;
        let notice = move |text: String| {
            div()
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element()
        };
        let (body, info) = match &self.content {
            Content::Reading => (div().into_any_element(), None),
            Content::Invalid(error) => (notice(format!("Not a readable image: {error}")), None),
            Content::Shown(picture) => (
                img(ImageSource::Image(picture.image.clone()))
                    .size_full()
                    .object_fit(ObjectFit::ScaleDown)
                    .with_fallback(move || notice("The image could not be decoded".into()))
                    .into_any_element(),
                Some(format!(
                    "{} × {} · {} · {}",
                    picture.width,
                    picture.height,
                    picture.format,
                    human_size(picture.file_size)
                )),
            ),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .bg(background)
                    .p(PADDING)
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(body),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .right(px(6.))
                            .child(background_button(
                                self.themed_background,
                                cx.listener(|this, _, _, cx| {
                                    this.themed_background = !this.themed_background;
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .children(info.map(|info| {
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(muted)
                    .child(info)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_file_is_read_with_its_size_and_format() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dot.png");
        image::RgbaImage::new(3, 2).save(&file).unwrap();
        let picture = read_picture(&file).unwrap();
        assert_eq!(
            (picture.width, picture.height, picture.format),
            (3, 2, "PNG")
        );
        assert_eq!(picture.image.format(), ImageFormat::Png);

        // The format comes from the bytes, not the extension.
        let misnamed = dir.path().join("dot.jpg");
        std::fs::copy(&file, &misnamed).unwrap();
        assert_eq!(read_picture(&misnamed).unwrap().format, "PNG");

        let text = dir.path().join("notes.png");
        std::fs::write(&text, "not an image").unwrap();
        assert!(read_picture(&text).is_err());
    }

    #[test]
    fn file_sizes_read_for_people() {
        assert_eq!(human_size(512), "512 bytes");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(3 * 1_048_576 / 2), "1.5 MB");
    }
}
