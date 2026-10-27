//! SVG files drawn, fitted into the view.
//!
//! The drawing is rasterized for the size it is shown at (times the
//! display's scale) and again when the view's size changes, so it stays
//! sharp at any size; parsing and rasterizing run in the background.
//!
//! Most drawings expect a white page behind them; a button in the corner
//! switches to the theme's background for drawings made for dark ones.
//! Draw.io's `.drawio.svg` files show their diagram this way too.

use super::{FileRenderer, RendererHandle, RendererView, has_extension};
use crate::comments::CommentChange;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    App, Context, EventEmitter, ImageSource, Pixels, Render, RenderImage, SharedString, Task,
    Window, canvas, div, img, prelude::*, px, size, white,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tools_core::image::parse_svg;
use tools_core::image::resvg::{self, tiny_skia, usvg};

pub const RENDERER: FileRenderer = FileRenderer {
    id: "svg",
    label: "Image",
    accepts: |path| has_extension(path, &["svg"]),
    create,
};

fn create(
    file: PathBuf,
    _root: PathBuf,
    _window: &mut Window,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|_| SvgPreview::new(file)))
}

/// Space around the drawing.
const PADDING: Pixels = px(12.);
/// Longest side of a rasterized drawing, in device pixels.
const MAX_PIXELS: f32 = 4096.;

/// What the view shows.
enum Drawing {
    Empty,
    Parsing,
    Parsed(Arc<usvg::Tree>),
    Invalid(String),
}

/// A drawing rasterized for one size of the view.
struct Raster {
    /// The view's inner size in device pixels it was made for.
    target: (u32, u32),
    image: Arc<RenderImage>,
    /// Its size on screen.
    size: gpui_kit::Size<Pixels>,
}

pub struct SvgPreview {
    file: PathBuf,
    source: String,
    drawing: Drawing,
    raster: Option<Raster>,
    /// Rasters replaced, to be freed from the window's atlas.
    stale: Vec<Arc<RenderImage>>,
    /// The device-pixel size a raster is being made for.
    rasterizing: Option<(u32, u32)>,
    /// The view's inner size in the last frame, in device pixels.
    measured: Rc<Cell<Option<(u32, u32)>>>,
    /// The theme's background behind the drawing instead of white.
    themed_background: bool,
    parse_task: Option<Task<()>>,
    raster_task: Option<Task<()>>,
}

impl EventEmitter<CommentChange> for SvgPreview {}

impl SvgPreview {
    fn new(file: PathBuf) -> Self {
        Self {
            file,
            source: String::new(),
            drawing: Drawing::Empty,
            raster: None,
            stale: Vec::new(),
            rasterizing: None,
            measured: Rc::default(),
            themed_background: false,
            parse_task: None,
            raster_task: None,
        }
    }

    /// Rasterize the drawing for `target` (device pixels) unless that is
    /// done or running.
    fn rasterize(&mut self, target: (u32, u32), scale: f32, cx: &mut Context<Self>) {
        let Drawing::Parsed(tree) = &self.drawing else {
            return;
        };
        if self.raster.as_ref().is_some_and(|r| r.target == target)
            || self.rasterizing == Some(target)
        {
            return;
        }
        self.rasterizing = Some(target);
        let tree = tree.clone();
        let job = cx
            .background_executor()
            .spawn(async move { rasterize(&tree, target) });
        self.raster_task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                if this.rasterizing != Some(target) {
                    return;
                }
                this.rasterizing = None;
                if let Some((image, width, height)) = result {
                    let raster = Raster {
                        target,
                        image: Arc::new(image),
                        size: size(px(width as f32 / scale), px(height as f32 / scale)),
                    };
                    if let Some(old) = this.raster.replace(raster) {
                        this.stale.push(old.image);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

/// `tree` rasterized to fit into `target` (device pixels), keeping its
/// aspect ratio: the image (BGRA, as GPUI takes it) and its pixel size.
fn rasterize(tree: &usvg::Tree, target: (u32, u32)) -> Option<(RenderImage, u32, u32)> {
    let natural = tree.size();
    let fit = (target.0 as f32 / natural.width()).min(target.1 as f32 / natural.height());
    let longest = natural.width().max(natural.height()) * fit;
    let scale = if longest > MAX_PIXELS {
        fit * MAX_PIXELS / longest
    } else {
        fit
    };
    let width = (natural.width() * scale).round().max(1.) as u32;
    let height = (natural.height() * scale).round().max(1.) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut data = pixmap.take();
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, data)?;
    let image = RenderImage::new(vec![image::Frame::new(buffer)]);
    Some((image, width, height))
}

impl RendererView for SvgPreview {
    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text == self.source {
            return;
        }
        self.source = text.to_owned();
        if text.trim().is_empty() {
            self.drawing = Drawing::Empty;
            self.parse_task = None;
            cx.notify();
            return;
        }
        if !matches!(self.drawing, Drawing::Parsed(_)) {
            self.drawing = Drawing::Parsing;
        }
        let text = text.to_owned();
        let resources_dir = self.file.parent().map(PathBuf::from);
        let job = cx.background_executor().spawn(async move {
            parse_svg(text.as_bytes(), resources_dir).map_err(|e| e.to_string())
        });
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.drawing = match result {
                    Ok(tree) => Drawing::Parsed(Arc::new(tree)),
                    Err(e) => Drawing::Invalid(e),
                };
                // Drawn again for the new drawing, at the same size.
                this.rasterizing = None;
                if let Some(old) = this.raster.take() {
                    this.stale.push(old.image);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for SvgPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.stale.drain(..) {
            window.drop_image(image).ok();
        }
        let scale = window.scale_factor();
        if let Some(target) = self.measured.get() {
            self.rasterize(target, scale, cx);
        }

        let theme = cx.theme();
        let background = if self.themed_background {
            theme.background
        } else {
            white()
        };
        let muted = theme.muted_foreground;
        let themed = self.themed_background;
        let notice = |text: String| {
            div()
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element()
        };
        let body = match (&self.drawing, &self.raster) {
            (Drawing::Empty, _) => notice("Empty file".into()),
            (Drawing::Invalid(error), _) => notice(format!("Not a drawable SVG: {error}")),
            (_, Some(raster)) => img(ImageSource::Render(raster.image.clone()))
                .w(raster.size.width)
                .h(raster.size.height)
                .into_any_element(),
            (_, None) => div().into_any_element(),
        };
        let measured = self.measured.clone();
        div()
            .size_full()
            .relative()
            .bg(background)
            .child(
                div().size_full().p(PADDING).child(
                    div()
                        .size_full()
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        // The space the drawing fits into; a new size
                        // draws it again.
                        .child(
                            canvas(
                                move |bounds, window, _| {
                                    let scale = window.scale_factor();
                                    let device = (
                                        (f32::from(bounds.size.width) * scale) as u32,
                                        (f32::from(bounds.size.height) * scale) as u32,
                                    );
                                    if device.0 > 0
                                        && device.1 > 0
                                        && measured.replace(Some(device)) != Some(device)
                                    {
                                        window.refresh();
                                    }
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(body),
                ),
            )
            .child(
                div().absolute().top(px(6.)).right(px(6.)).child(
                    Button::new("svg-background")
                        .icon(
                            Icon::default()
                                .path(SharedString::from(if themed {
                                    "icons/theme_light.svg"
                                } else {
                                    "icons/theme_dark.svg"
                                }))
                                .with_size(Size::XSmall),
                        )
                        .xsmall()
                        .ghost()
                        .tooltip(if themed {
                            "White background"
                        } else {
                            "Theme background"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.themed_background = !this.themed_background;
                            cx.notify();
                        })),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drawing_is_rasterized_to_fit_its_target() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12">
            <rect width="24" height="12" fill="red"/></svg>"#;
        let tree = parse_svg(svg.as_bytes(), None).unwrap();
        // Scaled up to the target's width, keeping the aspect ratio.
        let (_, w, h) = rasterize(&tree, (480, 480)).unwrap();
        assert_eq!((w, h), (480, 240));
        // Capped at the longest side.
        let (_, w, h) = rasterize(&tree, (100_000, 100_000)).unwrap();
        assert_eq!((w, h), (MAX_PIXELS as u32, MAX_PIXELS as u32 / 2));
    }
}
