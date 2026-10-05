//! The right panel's browser view: the agent's browser, live.
//!
//! The panel lists the session's browsers from the core's metadata events
//! ([`UiEvent::BrowsersChanged`]) and watches one tab through a
//! [`BrowserView`]: screencast frames, newest first, decoded off the main
//! thread, plus a short pulse where the agent clicks. It follows the agent's
//! active tab unless the user picked another one.
//!
//! The agent is in control by default. The user can take over: then the
//! panel forwards mouse, wheel and keyboard ([`input`]) and the toolbar
//! navigates ([`toolbar`]), while the agent's browser tools refuse to act.

mod geometry;
mod input;
mod keys;
mod toolbar;

use crate::Gpui;
use code_assistant_core::session::browsers::{BrowserEntry, BrowserKey, BrowserView, ViewInput};
use code_assistant_core::session::{EventPayload, StreamError};
use code_assistant_core::ui::UiEvent;
use futures::FutureExt as _;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::InputState;
use gpui_kit::{
    App, Bounds, Context, ElementInputHandler, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Pixels, Render, RenderImage, Styled,
    StyledImage, Subscription, Task, Window, div, img, prelude::*, px,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use web::{FrameMetadata, Point, ScreencastFrame};

/// The largest frame the panel asks for: the agent's viewport at 1×, which
/// a retina panel of up to 640 pt shows sharp.
const MAX_FRAME: (u32, u32) = (1280, 1280);
/// How long the pulse at an agent click stays visible.
const PULSE: Duration = Duration::from_millis(700);
const PULSE_SIZE: f32 = 28.0;

pub enum BrowserPanelEvent {
    /// The agent opened its first browser in this session.
    BrowserOpened,
}

/// A decoded frame, ready to draw.
struct ShownFrame {
    image: Arc<RenderImage>,
    meta: FrameMetadata,
}

pub struct BrowserPanel {
    session_id: Option<String>,
    browsers: Vec<BrowserEntry>,
    /// The browser the user picked; the first listed when unset or gone.
    picked_browser: Option<BrowserKey>,
    /// The tab the user picked; the browser's active tab when unset or gone.
    picked_tab: Option<String>,
    /// The browser and tab the frames come from.
    watching: Option<(BrowserKey, String)>,
    frame: Option<ShownFrame>,
    /// Where the frame was drawn last, for mapping the mouse onto the page.
    frame_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Agent presses (CSS px) and when they happened.
    presses: Vec<(Point, Instant)>,
    /// Acts on the watched tab while the user has control.
    input: Option<ViewInput>,
    /// Mouse buttons the user holds (CDP mask) and keys sent down.
    held_buttons: i64,
    keys_down: HashSet<String>,
    /// Text an input method is composing, not sent yet.
    marked_text: Option<String>,
    /// The address field while the user edits it.
    address: Option<(Entity<InputState>, Subscription)>,
    listing_tasks: Vec<Task<()>>,
    view_task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl EventEmitter<BrowserPanelEvent> for BrowserPanel {}

impl BrowserPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            session_id: None,
            browsers: Vec::new(),
            picked_browser: None,
            picked_tab: None,
            watching: None,
            frame: None,
            frame_bounds: Rc::default(),
            presses: Vec::new(),
            input: None,
            held_buttons: 0,
            keys_down: HashSet::new(),
            marked_text: None,
            address: None,
            listing_tasks: Vec::new(),
            view_task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Show the browsers of `session_id`; `None` detaches the panel (no
    /// subscription, no screencast).
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id == session_id {
            return;
        }
        self.session_id = session_id.clone();
        self.browsers.clear();
        self.picked_browser = None;
        self.picked_tab = None;
        self.listing_tasks.clear();
        self.stop_watching(cx);
        cx.notify();
        let Some(session_id) = session_id else {
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };

        // Filter the stream off the main thread: it carries every streaming
        // fragment, and only this session's browser listings matter here.
        let (tx, rx) = async_channel::unbounded::<Vec<BrowserEntry>>();
        let filter = cx.background_spawn(async move {
            let mut subscription = service.subscribe();
            let mut resync = true;
            loop {
                if resync {
                    resync = false;
                    match service.watch_browsers(session_id.clone()).await {
                        Ok(listing) => {
                            if tx.send(listing).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => tracing::warn!("Browser panel: cannot list browsers: {e:#}"),
                    }
                }
                match subscription.recv().await {
                    Ok(event) if event.session_id.as_deref() == Some(&session_id) => {
                        if let EventPayload::Ui(UiEvent::BrowsersChanged { browsers }) =
                            event.payload
                            && tx.send(browsers).await.is_err()
                        {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(StreamError::Lagged { .. }) => resync = true,
                    Err(StreamError::Closed) => return,
                }
            }
        });
        let apply = cx.spawn(async move |this, cx| {
            while let Ok(listing) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.set_browsers(listing, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.listing_tasks = vec![filter, apply];
    }

    fn set_browsers(&mut self, browsers: Vec<BrowserEntry>, cx: &mut Context<Self>) {
        let opened = self.browsers.is_empty() && !browsers.is_empty();
        self.browsers = browsers;
        self.sync_view(cx);
        if opened {
            cx.emit(BrowserPanelEvent::BrowserOpened);
        }
        cx.notify();
    }

    /// The browser shown: the picked one while it exists, else the first.
    fn shown_browser(&self) -> Option<&BrowserEntry> {
        self.picked_browser
            .as_ref()
            .and_then(|key| self.browsers.iter().find(|b| b.key == *key))
            .or_else(|| self.browsers.first())
    }

    /// Whether the user controls the shown browser.
    fn user_in_control(&self) -> bool {
        self.shown_browser().is_some_and(|b| b.user_control)
    }

    /// The tab shown: the picked one while it exists, else the active one.
    fn shown_tab(browser: &BrowserEntry, picked: Option<&str>) -> Option<String> {
        let tabs = &browser.tabs;
        picked
            .and_then(|id| tabs.iter().find(|t| t.id == id))
            .or_else(|| tabs.iter().find(|t| t.active))
            .or_else(|| tabs.first())
            .map(|t| t.id.clone())
    }

    /// Watch what should be shown, if that is not what is watched.
    fn sync_view(&mut self, cx: &mut Context<Self>) {
        let target = self.shown_browser().and_then(|browser| {
            Self::shown_tab(browser, self.picked_tab.as_deref())
                .map(|tab| (browser.key.clone(), tab))
        });
        if target == self.watching {
            return;
        }
        self.stop_watching(cx);
        let (Some((key, tab_id)), Some(session_id)) = (target, self.session_id.clone()) else {
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        self.watching = Some((key.clone(), tab_id.clone()));
        self.view_task = Some(cx.spawn(async move |this, cx| {
            let view = service
                .browser_view(session_id, key, Some(tab_id), MAX_FRAME)
                .await;
            match view {
                Ok(view) => {
                    tracing::debug!("Browser panel: watching {:?} {}", view.key, view.tab_id);
                    Self::pump(view, this, cx).await;
                    tracing::debug!("Browser panel: the view ended");
                }
                Err(e) => tracing::debug!("Browser panel: cannot watch the tab: {e:#}"),
            }
        }));
    }

    /// Show the view's frames and the agent's presses until the view ends.
    async fn pump(
        view: BrowserView,
        this: gpui_kit::WeakEntity<Self>,
        cx: &mut gpui_kit::AsyncApp,
    ) {
        let BrowserView {
            mut frames,
            mut presses,
            input,
            ..
        } = view;
        if this.update(cx, |this, _| this.input = Some(input)).is_err() {
            return;
        }
        let mut shown_count = 0usize;
        loop {
            futures::select_biased! {
                press = presses.recv().fuse() => match press {
                    Ok(at) => {
                        let shown = this.update(cx, |this, cx| {
                            this.presses.push((at, Instant::now()));
                            cx.notify();
                        });
                        if shown.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                frame = frames.next().fuse() => {
                    let Some(frame) = frame else { return };
                    shown_count += 1;
                    let started = Instant::now();
                    let age = frame.received.elapsed();
                    let decoded = cx.background_spawn(async move { decode(&frame) }).await;
                    if shown_count % 30 == 1 {
                        tracing::debug!(
                            "Browser panel: frame {shown_count}, {age:?} old, decoded in {:?}",
                            started.elapsed()
                        );
                    }
                    let shown = match decoded {
                        Ok(shown) => this.update(cx, |this, cx| this.show_frame(shown, cx)),
                        Err(e) => {
                            tracing::debug!("Browser panel: undecodable frame: {e:#}");
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

    fn stop_watching(&mut self, cx: &mut Context<Self>) {
        self.view_task = None;
        self.watching = None;
        self.input = None;
        self.presses.clear();
        self.held_buttons = 0;
        self.keys_down.clear();
        if let Some(old) = self.frame.take() {
            cx.drop_image(old.image, None);
        }
    }

    fn render_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(frame) = &self.frame else {
            let note = if self.browsers.is_empty() {
                "The agent has no browser open in this session."
            } else {
                "Waiting for the page…"
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(note)
                .into_any_element();
        };
        let meta = frame.meta;
        let image = frame.image.clone();

        self.presses.retain(|(_, at)| at.elapsed() < PULSE);
        if !self.presses.is_empty() {
            window.request_animation_frame();
        }
        let presses = self.presses.clone();
        let accent = cx.theme().primary;
        let aspect = (meta.device_width / meta.device_height.max(1.0)) as f32;
        let frame_bounds = self.frame_bounds.clone();
        // While the user has control, the frame takes text input (an input
        // method composes into it) for the focused panel.
        let input_handler = self
            .user_in_control()
            .then(|| (self.focus_handle.clone(), cx.entity()));

        let surface = div()
            .id("browser-frame")
            .relative()
            .w_full()
            .aspect_ratio(aspect)
            .child(img(image).size_full().object_fit(ObjectFit::Fill))
            // Records where the frame is drawn, takes text input and paints
            // a pulse where the agent clicked.
            .child(
                gpui_kit::canvas(
                    move |bounds, _, _| {
                        frame_bounds.set(Some(bounds));
                        bounds
                    },
                    move |bounds, _, window, cx| {
                        if let Some((focus, entity)) = input_handler {
                            window.handle_input(
                                &focus,
                                ElementInputHandler::new(bounds, entity),
                                cx,
                            );
                        }
                        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        for (at, when) in &presses {
                            let t = when.elapsed().as_secs_f32() / PULSE.as_secs_f32();
                            let (x, y) = geometry::to_view(&meta, view, (at.x, at.y));
                            let size = PULSE_SIZE * (0.4 + 0.6 * t);
                            let origin = bounds.origin
                                + gpui_kit::point(px(x - size / 2.), px(y - size / 2.));
                            window.paint_quad(gpui_kit::quad(
                                Bounds::new(origin, gpui_kit::size(px(size), px(size))),
                                px(size / 2.),
                                accent.opacity(0.35 * (1. - t)),
                                px(2.),
                                accent.opacity(1. - t),
                                Default::default(),
                            ));
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );

        div()
            .size_full()
            .overflow_hidden()
            .child(self.with_mouse_input(surface, cx))
            .into_any_element()
    }
}

impl Focusable for BrowserPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .when(!self.browsers.is_empty(), |el| {
                el.child(self.render_toolbar(cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_frame(window, cx)),
            );
        self.with_key_input(panel, cx)
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
