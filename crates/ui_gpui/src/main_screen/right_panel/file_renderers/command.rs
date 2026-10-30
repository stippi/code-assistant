//! A renderer from the settings: a command turns the file into an SVG or an
//! image, which this view shows ([`custom_renderers::run`]).
//!
//! The command runs again whenever the text changes (a reload from disk),
//! a moment after the last change, on the background executor; a run still
//! going is stopped. The last
//! output stays in view while the next run goes. An SVG is shown by the
//! SVG renderer's view, so it stays sharp at any size; an image is fitted
//! into the view, never scaled up.
//!
//! [`custom_renderers::run`]: crate::shared::custom_renderers::run

use super::svg::SvgPreview;
use super::{RendererHandle, RendererView, TextEdit};
use crate::comments::CommentChange;
use crate::shared::custom_renderers::{self, CommandRenderer, OutputFormat};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, Context, Entity, EventEmitter, ImageSource, ObjectFit, Render, RenderImage, SharedString,
    StyledImage, Task, Window, div, img, prelude::*, px, white,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Quiet period after a change before the command runs again.
const DEBOUNCE: Duration = Duration::from_millis(400);

pub fn create(
    label: SharedString,
    renderer: CommandRenderer,
    file: PathBuf,
    root: PathBuf,
    _window: &mut Window,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|_| CommandPreview::new(label, Arc::new(renderer), file, root)))
}

/// The latest output.
enum Output {
    None,
    Svg(Entity<SvgPreview>),
    Image(Arc<RenderImage>),
}

/// What a run produced, decoded off the main thread.
enum Produced {
    Svg(String),
    Image(RenderImage),
}

pub struct CommandPreview {
    label: SharedString,
    renderer: Arc<CommandRenderer>,
    file: PathBuf,
    root: PathBuf,
    /// The text last handed in.
    text: Option<String>,
    output: Output,
    /// Why the last run failed.
    error: Option<String>,
    running: bool,
    /// Stops the running command.
    cancel: Arc<AtomicBool>,
    run_task: Option<Task<()>>,
    /// Images replaced, to be freed from the window's atlas.
    stale: Vec<Arc<RenderImage>>,
}

impl EventEmitter<CommentChange> for CommandPreview {}
impl EventEmitter<TextEdit> for CommandPreview {}

impl CommandPreview {
    fn new(
        label: SharedString,
        renderer: Arc<CommandRenderer>,
        file: PathBuf,
        root: PathBuf,
    ) -> Self {
        Self {
            label,
            renderer,
            file,
            root,
            text: None,
            output: Output::None,
            error: None,
            running: false,
            cancel: Arc::default(),
            run_task: None,
            stale: Vec::new(),
        }
    }

    fn produced(&mut self, produced: Produced, cx: &mut Context<Self>) {
        match produced {
            Produced::Svg(svg) => {
                let view = match &self.output {
                    Output::Svg(view) => view.clone(),
                    _ => cx.new(|_| SvgPreview::new(self.file.clone())),
                };
                view.update(cx, |view, cx| view.set_text(&svg, cx));
                self.replace_output(Output::Svg(view));
            }
            Produced::Image(image) => self.replace_output(Output::Image(Arc::new(image))),
        }
    }

    fn replace_output(&mut self, output: Output) {
        if let Output::Image(old) = std::mem::replace(&mut self.output, output) {
            self.stale.push(old);
        }
    }
}

impl Drop for CommandPreview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Run `renderer` and decode its output; blocks.
fn produce(
    renderer: &CommandRenderer,
    file: &std::path::Path,
    root: &std::path::Path,
    text: &str,
    cancel: &AtomicBool,
) -> anyhow::Result<Produced> {
    let bytes = custom_renderers::run(renderer, file, root, text, cancel)?;
    Ok(match renderer.output {
        OutputFormat::Svg => Produced::Svg(String::from_utf8_lossy(&bytes).into_owned()),
        OutputFormat::Image => {
            let mut rgba = image::load_from_memory(&bytes)
                .map_err(|e| anyhow::anyhow!("The command's output is not an image: {e}"))?
                .to_rgba8();
            // GPUI takes BGRA.
            for pixel in rgba.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Produced::Image(RenderImage::new(vec![image::Frame::new(rgba)]))
        }
    })
}

impl RendererView for CommandPreview {
    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.text.as_deref() == Some(text) {
            return;
        }
        // The first run starts at once, later ones once changes pause.
        let wait = self.text.is_some();
        self.text = Some(text.to_owned());
        self.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.running = true;

        let renderer = self.renderer.clone();
        let (file, root, text) = (self.file.clone(), self.root.clone(), text.to_owned());
        self.run_task = Some(cx.spawn(async move |this, cx| {
            if wait {
                cx.background_executor().timer(DEBOUNCE).await;
            }
            let result = cx
                .background_executor()
                .spawn(async move { produce(&renderer, &file, &root, &text, &cancel) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                match result {
                    Ok(produced) => {
                        this.error = None;
                        this.produced(produced, cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for CommandPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.stale.drain(..) {
            window.drop_image(image).ok();
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let label = self.label.clone();

        let body = match (&self.error, &self.output) {
            (Some(error), _) => div()
                .size_full()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .bg(theme.background)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(SharedString::from(format!("{label} could not show this file"))),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .whitespace_normal()
                        .child(SharedString::from(error.clone())),
                )
                .child(div().text_xs().text_color(muted).child(
                    "Check the renderer's command in Settings → File Renderers, or pick Text above.",
                ))
                .into_any_element(),
            (None, Output::Svg(view)) => view.clone().into_any_element(),
            (None, Output::Image(image)) => div()
                .size_full()
                .p(px(12.))
                .bg(white())
                .child(
                    img(ImageSource::Render(image.clone()))
                        .size_full()
                        .object_fit(ObjectFit::ScaleDown),
                )
                .into_any_element(),
            (None, Output::None) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.background)
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(format!("Running {label}…")))
                .into_any_element(),
        };
        let updating = self.running && !matches!(self.output, Output::None);
        div()
            .size_full()
            .relative()
            .child(body)
            .when(updating, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .left(px(6.))
                        .px_2()
                        .rounded_md()
                        .bg(theme.secondary)
                        .text_xs()
                        .text_color(muted)
                        .child("Updating…"),
                )
            })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    fn preview(
        command: &str,
        output: OutputFormat,
        cx: &mut TestAppContext,
    ) -> Entity<CommandPreview> {
        let dir = std::env::temp_dir();
        let renderer = Arc::new(CommandRenderer {
            command: command.into(),
            output,
        });
        cx.new(|_| CommandPreview::new("Test".into(), renderer, dir.join("missing.txt"), dir))
    }

    /// Run the command (it blocks the test's executor) and its result.
    fn wait_for_run(view: &Entity<CommandPreview>, cx: &mut TestAppContext) {
        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.running));
    }

    #[gpui_kit::test]
    fn svg_output_shows_in_the_svg_view(cx: &mut TestAppContext) {
        // The text arrives on standard input.
        let view = preview("cat", OutputFormat::Svg, cx);
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
        view.update(cx, |view, cx| view.set_text(svg, cx));
        assert!(view.read_with(cx, |view, _| view.running));
        wait_for_run(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            assert!(matches!(view.output, Output::Svg(_)));
        });
    }

    #[gpui_kit::test]
    fn image_output_is_decoded_and_failures_are_shown(cx: &mut TestAppContext) {
        let png = {
            let mut bytes = Vec::new();
            image::RgbaImage::new(3, 2)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        };
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.png");
        std::fs::write(&source, png).unwrap();
        let command = format!("cp '{}' {{output}}", source.display());
        let view = preview(&command, OutputFormat::Image, cx);
        view.update(cx, |view, cx| view.set_text("x", cx));
        wait_for_run(&view, cx);
        view.read_with(cx, |view, _| match &view.output {
            Output::Image(image) => {
                let size = image.size(0);
                assert_eq!((size.width.0, size.height.0), (3, 2));
            }
            _ => panic!("no image: {:?}", view.error),
        });

        // Output that is no image fails with a reason.
        let view = preview("echo nope", OutputFormat::Image, cx);
        view.update(cx, |view, cx| view.set_text("x", cx));
        wait_for_run(&view, cx);
        let error = view.read_with(cx, |view, _| view.error.clone()).unwrap();
        assert!(
            error.starts_with("The command's output is not an image"),
            "{error}"
        );
    }
}
