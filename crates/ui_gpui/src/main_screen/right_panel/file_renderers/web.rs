//! A plugin's file viewer: a web page showing the file, live.
//!
//! The core opens the page in a headless Chrome tab of its own (starting
//! the plugin's server first when it is not running) and connects it to the
//! file's text ([`code_assistant_core::plugin_host::viewer`]). This view
//! draws the tab's screencast, lays the page out for the view's size, and
//! forwards mouse and keyboard to it, always: unlike the agent's browser,
//! nobody else acts on this page. Text the page writes back is reported as
//! a [`RendererEdit`]; the file viewer saves it like an edit in the editor.

use super::super::browser::{geometry, keys};
use super::{RendererEdit, RendererHandle, RendererView};
use crate::Gpui;
use crate::comments::CommentChange;
use code_assistant_core::plugin_host::PluginFileViewer;
use code_assistant_core::plugin_host::viewer::{
    ViewerControl, ViewerEvent, ViewerRequest, WebViewer,
};
use futures::FutureExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    App, Bounds, ClipboardItem, Context, Div, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, ParentElement, Pixels, Render, RenderImage, ScrollWheelEvent,
    SharedString, Stateful, Styled, StyledImage, Task, Window, canvas, div, img, prelude::*, px,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use web::{Button as PageButton, FrameMetadata, Point, ScreencastFrame, UserInput};

/// The largest frame asked for.
const MAX_FRAME: (u32, u32) = (2560, 2560);
/// The page's size until the view has been measured.
const FIRST_VIEWPORT: (u32, u32) = (1024, 768);
/// Quiet period after a size change before the page is laid out again.
const RESIZE_DELAY: Duration = Duration::from_millis(150);

pub fn create(
    viewer: Arc<PluginFileViewer>,
    file: PathBuf,
    root: PathBuf,
    _window: &mut Window,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|cx| WebViewerView::new(viewer, file, root, cx)))
}

enum State {
    /// Waiting for the first text.
    Idle,
    Opening,
    Open,
    Failed(String),
}

/// A decoded frame, ready to draw.
struct ShownFrame {
    image: Arc<RenderImage>,
    meta: FrameMetadata,
}

pub struct WebViewerView {
    viewer: Arc<PluginFileViewer>,
    file: PathBuf,
    root: PathBuf,
    state: State,
    /// The file's text as last given to or received from the page.
    text: String,
    control: Option<ViewerControl>,
    frame: Option<ShownFrame>,
    /// Where the frame was drawn last, for mapping the mouse onto the page.
    frame_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The view's size in the last frame, in px.
    measured: Rc<Cell<Option<(u32, u32)>>>,
    /// The size the page was last laid out for.
    viewport: (u32, u32),
    held_buttons: i64,
    keys_down: HashSet<String>,
    focus_handle: FocusHandle,
    open_task: Option<Task<()>>,
    resize_task: Option<Task<()>>,
}

impl EventEmitter<CommentChange> for WebViewerView {}
impl EventEmitter<RendererEdit> for WebViewerView {}

impl WebViewerView {
    fn new(
        viewer: Arc<PluginFileViewer>,
        file: PathBuf,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            viewer,
            file,
            root,
            state: State::Idle,
            text: String::new(),
            control: None,
            frame: None,
            frame_bounds: Rc::default(),
            measured: Rc::default(),
            viewport: FIRST_VIEWPORT,
            held_buttons: 0,
            keys_down: HashSet::new(),
            focus_handle: cx.focus_handle(),
            open_task: None,
            resize_task: None,
        }
    }

    /// Open the page through the core, then show its frames and pass its
    /// text changes on until the view goes away.
    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            self.state = State::Failed("The app's services are not running.".into());
            return;
        };
        self.state = State::Opening;
        self.control = None;
        let viewport = self.measured.get().unwrap_or(self.viewport);
        self.viewport = viewport;
        let request = ViewerRequest {
            viewer: self.viewer.key(),
            file: self.file.clone(),
            root: self.root.clone(),
            text: self.text.clone(),
            viewport,
            max_frame: MAX_FRAME,
        };
        let sent_text = request.text.clone();
        self.open_task =
            Some(cx.spawn(
                async move |this, cx| match service.open_file_viewer(request).await {
                    Ok(viewer) => Self::pump(viewer, sent_text, this, cx).await,
                    Err(e) => {
                        this.update(cx, |this, cx| {
                            this.state = State::Failed(format!("{e:#}"));
                            cx.notify();
                        })
                        .ok();
                    }
                },
            ));
        cx.notify();
    }

    async fn pump(
        viewer: WebViewer,
        sent_text: String,
        this: gpui_kit::WeakEntity<Self>,
        cx: &mut gpui_kit::AsyncApp,
    ) {
        let WebViewer {
            mut frames,
            control,
            mut events,
        } = viewer;
        let opened = this.update(cx, |this, cx| {
            // The text may have changed while the page opened.
            if this.text != sent_text {
                control.set_text(this.text.clone());
            }
            this.control = Some(control);
            this.state = State::Open;
            cx.notify();
        });
        if opened.is_err() {
            return;
        }
        loop {
            futures::select_biased! {
                event = events.recv().fuse() => {
                    let Some(event) = event else { return };
                    let handled = this.update(cx, |this, cx| match event {
                        ViewerEvent::TextChanged(text) => {
                            this.text = text.clone();
                            cx.emit(RendererEdit(text));
                        }
                        ViewerEvent::Failed(error) => {
                            this.state = State::Failed(error);
                            cx.notify();
                        }
                    });
                    if handled.is_err() {
                        return;
                    }
                }
                frame = frames.next().fuse() => {
                    let Some(frame) = frame else { return };
                    let decoded = cx.background_spawn(async move { decode(&frame) }).await;
                    let shown = match decoded {
                        Ok(shown) => this.update(cx, |this, cx| this.show_frame(shown, cx)),
                        Err(e) => {
                            tracing::debug!("File viewer: undecodable frame: {e:#}");
                            Ok(())
                        }
                    };
                    if shown.is_err() {
                        return;
                    }
                }
            }
        }
    }

    fn show_frame(&mut self, frame: ShownFrame, cx: &mut Context<Self>) {
        // A frame's texture stays in the sprite atlas until dropped.
        if let Some(old) = self.frame.replace(frame) {
            cx.drop_image(old.image, None);
        }
        cx.notify();
    }

    /// Lay the page out again once the view's size settles.
    fn follow_size(&mut self, cx: &mut Context<Self>) {
        let Some(size) = self.measured.get() else {
            return;
        };
        if size == self.viewport || self.control.is_none() {
            return;
        }
        self.viewport = size;
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_DELAY).await;
            this.update(cx, |this, _| {
                if this.viewport == size
                    && let Some(control) = &this.control
                {
                    control.resize(size.0, size.1);
                }
            })
            .ok();
        }));
    }

    fn send(&self, input: UserInput) {
        if let Some(control) = &self.control {
            control.input(input);
        }
    }

    /// The page point under a window position, if the frame shows one there.
    fn page_point(&self, position: gpui_kit::Point<Pixels>) -> Option<Point> {
        let meta = self.frame.as_ref()?.meta;
        let bounds = self.frame_bounds.get()?;
        let local = position - bounds.origin;
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (x, y) = geometry::to_page(&meta, view, (f32::from(local.x), f32::from(local.y)))?;
        Some(Point { x, y })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent) {
        let (Some(at), Some((button, mask))) =
            (self.page_point(event.position), page_button(event.button))
        else {
            return;
        };
        self.held_buttons |= mask;
        self.send(UserInput::MouseDown {
            at,
            button,
            click_count: event.click_count as u32,
            buttons: self.held_buttons,
            modifiers: keys::modifier_mask(&event.modifiers),
        });
    }

    fn mouse_up(&mut self, event: &MouseUpEvent) {
        let Some((button, mask)) = page_button(event.button) else {
            return;
        };
        if self.held_buttons & mask == 0 {
            return;
        }
        self.held_buttons &= !mask;
        // A release outside the frame lands on its nearest edge.
        let at = self
            .page_point(event.position)
            .or_else(|| {
                let bounds = self.frame_bounds.get()?;
                let inner = |v: Pixels, lo: Pixels, len: Pixels| v.clamp(lo, lo + len - px(1.));
                self.page_point(gpui_kit::point(
                    inner(event.position.x, bounds.origin.x, bounds.size.width),
                    inner(event.position.y, bounds.origin.y, bounds.size.height),
                ))
            })
            .unwrap_or(Point { x: 0.0, y: 0.0 });
        self.send(UserInput::MouseUp {
            at,
            button,
            click_count: event.click_count as u32,
            buttons: self.held_buttons,
            modifiers: keys::modifier_mask(&event.modifiers),
        });
    }

    fn with_mouse_input(&self, surface: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let surface = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .into_iter()
            .fold(surface, |surface, button| {
                surface
                    .on_mouse_down(
                        button,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus_handle, cx);
                            this.mouse_down(event);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_up(
                        button,
                        cx.listener(|this, event: &MouseUpEvent, _, _| this.mouse_up(event)),
                    )
                    .on_mouse_up_out(
                        button,
                        cx.listener(|this, event: &MouseUpEvent, _, _| this.mouse_up(event)),
                    )
            });
        surface
            .cursor_default()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                if let Some(at) = this.page_point(event.position) {
                    this.send(UserInput::MouseMove {
                        at,
                        buttons: this.held_buttons,
                        modifiers: keys::modifier_mask(&event.modifiers),
                    });
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let Some(at) = this.page_point(event.position) else {
                    return;
                };
                // GPUI's delta moves the content; the page's moves the view.
                let delta = event.delta.pixel_delta(window.line_height());
                this.send(UserInput::Wheel {
                    at,
                    dx: -f64::from(f32::from(delta.x)),
                    dy: -f64::from(f32::from(delta.y)),
                    modifiers: keys::modifier_mask(&event.modifiers),
                });
                cx.stop_propagation();
            }))
    }

    /// Handle a key press; `false` leaves it to the app.
    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        match keys::key_down(keystroke) {
            keys::KeyAction::Pass => false,
            keys::KeyAction::Send {
                key,
                text,
                commands,
            } => {
                self.keys_down.insert(key.clone());
                self.send(UserInput::KeyDown {
                    key,
                    text,
                    modifiers: keys::modifier_mask(&keystroke.modifiers),
                    commands,
                });
                true
            }
            keys::KeyAction::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.send(UserInput::InsertText(text));
                }
                true
            }
            action @ (keys::KeyAction::Copy | keys::KeyAction::Cut) => {
                let cut = action == keys::KeyAction::Cut;
                self.copy_selection(cut, cx);
                true
            }
        }
    }

    /// Put the page's selection on the clipboard; `cut` removes it after.
    fn copy_selection(&self, cut: bool, cx: &mut Context<Self>) {
        let Some(control) = self.control.clone() else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let Ok(text) = control.selected_text().await else {
                return;
            };
            if text.is_empty() {
                return;
            }
            cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
            if cut {
                for input in [
                    UserInput::KeyDown {
                        key: "Delete".into(),
                        text: None,
                        modifiers: 0,
                        commands: vec![],
                    },
                    UserInput::KeyUp {
                        key: "Delete".into(),
                        modifiers: 0,
                    },
                ] {
                    control.input(input);
                }
            }
        })
        .detach();
    }

    fn render_status(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let muted = cx.theme().muted_foreground;
        let label = &self.viewer.spec.label;
        let note = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(text)
        };
        match &self.state {
            State::Failed(error) => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(format!(
                            "{label} could not open the file"
                        ))),
                )
                .child(
                    div()
                        .max_w(px(520.))
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(SharedString::from(error.clone())),
                )
                .child(
                    Button::new("web-viewer-retry")
                        .label("Try again")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| this.open(cx))),
                )
                .into_any_element(),
            State::Idle | State::Opening if self.viewer.spec.start.is_some() => note(format!(
                "Opening {label}… (starting its server can take a moment)"
            ))
            .into_any_element(),
            _ => note(format!("Opening {label}…")).into_any_element(),
        }
    }

    fn render_frame(&self, frame: &ShownFrame, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let meta = frame.meta;
        let aspect = (meta.device_width / meta.device_height.max(1.0)) as f32;
        let frame_bounds = self.frame_bounds.clone();
        let surface = div()
            .id("web-viewer-frame")
            .relative()
            .w_full()
            .aspect_ratio(aspect)
            .child(
                img(frame.image.clone())
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
            .child(
                canvas(
                    move |bounds, _, _| frame_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        self.with_mouse_input(surface, cx).into_any_element()
    }
}

impl RendererView for WebViewerView {
    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if matches!(self.state, State::Idle) {
            self.text = text.to_string();
            self.open(cx);
            return;
        }
        if self.text == text {
            return;
        }
        self.text = text.to_string();
        if let Some(control) = &self.control {
            control.set_text(self.text.clone());
        }
    }
}

impl Render for WebViewerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_size(cx);
        let measured = self.measured.clone();
        let content = match (&self.state, &self.frame) {
            (State::Failed(_), _) | (_, None) => self.render_status(cx),
            (_, Some(frame)) => self.render_frame(frame, cx),
        };
        div()
            .id("web-viewer")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(cx.theme().background)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.key_down(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                let key = &event.keystroke.key;
                if this.keys_down.remove(key) {
                    this.send(UserInput::KeyUp {
                        key: key.clone(),
                        modifiers: keys::modifier_mask(&event.keystroke.modifiers),
                    });
                }
            }))
            .child(content)
            // Measures the view, for laying the page out to fit it.
            .child(
                canvas(
                    move |bounds, _, _| {
                        let width = f32::from(bounds.size.width).round().max(1.0) as u32;
                        let height = f32::from(bounds.size.height).round().max(1.0) as u32;
                        measured.set(Some((width, height)));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

/// The page's button and its mask bit for a GPUI button.
fn page_button(button: MouseButton) -> Option<(PageButton, i64)> {
    match button {
        MouseButton::Left => Some((PageButton::Left, 1)),
        MouseButton::Right => Some((PageButton::Right, 2)),
        MouseButton::Middle => Some((PageButton::Middle, 4)),
        _ => None,
    }
}

/// Decode a JPEG frame into GPUI's BGRA image.
fn decode(frame: &ScreencastFrame) -> anyhow::Result<ShownFrame> {
    let mut rgba =
        image::load_from_memory_with_format(&frame.jpeg, image::ImageFormat::Jpeg)?.to_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(ShownFrame {
        image: Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])),
        meta: frame.metadata,
    })
}
