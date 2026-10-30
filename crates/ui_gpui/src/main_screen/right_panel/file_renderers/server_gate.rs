//! A plugin renderer that needs a server (a web editor served on
//! localhost, a command calling a local service), shown once the server
//! answers.
//!
//! The gate asks the core to make sure of the server
//! ([`SessionService::start_plugin_server`]): one that answers is used,
//! otherwise the plugin's start command runs and the gate waits. Until then
//! it says so, after a failure it shows why and offers to try again. Then it
//! creates the renderer's own view and passes everything through: the text,
//! comments and reveals in, comment changes and edits out. The server lease
//! lives as long as the gate, so the server stays while the file is shown
//! (and a while after, see `plugin_host::server`).
//!
//! [`SessionService::start_plugin_server`]: code_assistant_core::session::SessionService::start_plugin_server

use super::{OnComment, OnEdit, RendererHandle, RendererView, TextEdit};
use crate::Gpui;
use crate::comments::CommentChange;
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::plugin_host::{ServerLease, ServerTarget};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::{
    App, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, div, prelude::*, px,
};
use std::rc::Rc;

/// Creates the renderer's own view once the server answers.
pub type CreateInner = Box<dyn FnOnce(&mut Window, &mut App) -> Box<dyn RendererHandle>>;

pub fn create(
    label: SharedString,
    server: ServerTarget,
    inner: CreateInner,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|cx| {
        let mut gate = ServerGate::new(label, server, inner);
        gate.start(cx);
        gate
    }))
}

enum State {
    Starting,
    Failed(String),
    /// The server answers; the renderer's view is made in the next render.
    Ready,
}

pub struct ServerGate {
    label: SharedString,
    server: ServerTarget,
    state: State,
    create_inner: Option<CreateInner>,
    inner: Option<Box<dyn RendererHandle>>,
    /// Keeps the server running while the file is shown.
    lease: Option<ServerLease>,
    /// What to hand the renderer's view once it exists.
    text: String,
    comments: Vec<LineComment>,
    _start: Option<Task<()>>,
    _inner_subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommentChange> for ServerGate {}
impl EventEmitter<TextEdit> for ServerGate {}

impl ServerGate {
    fn new(label: SharedString, server: ServerTarget, create_inner: CreateInner) -> Self {
        Self {
            label,
            server,
            state: State::Starting,
            create_inner: Some(create_inner),
            inner: None,
            lease: None,
            text: String::new(),
            comments: Vec::new(),
            _start: None,
            _inner_subscriptions: Vec::new(),
        }
    }

    /// Make sure of the server.
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            self.state = State::Failed("The app's services are not running.".into());
            return;
        };
        self.state = State::Starting;
        let server = self.server.clone();
        self._start = Some(cx.spawn(async move |this, cx| {
            let result = service.start_plugin_server(server).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(lease) => {
                        this.lease = Some(lease);
                        this.state = State::Ready;
                    }
                    Err(e) => this.state = State::Failed(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The server answers: make the renderer's view and hand it what came
    /// so far.
    fn create_inner(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(create) = self.create_inner.take() else {
            return;
        };
        let inner = create(window, cx);
        let gate = cx.entity().downgrade();
        let on_comment: OnComment = {
            let gate = gate.clone();
            Rc::new(move |change, cx| {
                gate.update(cx, |_, cx| cx.emit(change.clone())).ok();
            })
        };
        let on_edit: OnEdit = Rc::new(move |edit, cx| {
            gate.update(cx, |gate, cx| {
                gate.text = edit.0.clone();
                cx.emit(edit.clone());
            })
            .ok();
        });
        self._inner_subscriptions = inner.forward_to(on_comment, on_edit, cx);
        inner.set_text(&self.text, cx);
        inner.set_comments(&self.comments, cx);
        self.inner = Some(inner);
    }

    fn render_status(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let muted = cx.theme().muted_foreground;
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
                            "The server of {} did not start",
                            self.label
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
                    Button::new("server-gate-retry")
                        .label("Try again")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                )
                .into_any_element(),
            State::Starting | State::Ready => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(format!(
                    "Starting the server of {}…",
                    self.label
                )))
                .into_any_element(),
        }
    }
}

impl RendererView for ServerGate {
    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.text = text.to_owned();
        if let Some(inner) = &self.inner {
            inner.set_text(text, cx);
        }
    }

    fn set_comments(&mut self, comments: &[LineComment], cx: &mut Context<Self>) {
        self.comments = comments.to_vec();
        if let Some(inner) = &self.inner {
            inner.set_comments(comments, cx);
        }
    }

    fn reveal_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.reveal_line(line, window, cx))
    }

    fn reveal_comment(
        &mut self,
        comment: LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.reveal_comment(comment, window, cx))
    }

    fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(inner) = &self.inner {
            inner.start_comment(window, cx);
        }
    }
}

impl Render for ServerGate {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.state, State::Ready) && self.inner.is_none() {
            self.create_inner(window, cx);
        }
        match &self.inner {
            Some(inner) => div().size_full().child(inner.view()).into_any_element(),
            None => self.render_status(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::cell::RefCell;

    /// A renderer view that records its text and edits on demand.
    struct Probe {
        text: Rc<RefCell<String>>,
    }

    impl EventEmitter<CommentChange> for Probe {}
    impl EventEmitter<TextEdit> for Probe {}

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    impl RendererView for Probe {
        fn set_text(&mut self, text: &str, _: &mut Context<Self>) {
            *self.text.borrow_mut() = text.to_owned();
        }
    }

    /// Without a server to wait for, the gate makes the renderer's view,
    /// hands it the text that came meanwhile, and passes its edits out.
    #[gpui_kit::test]
    fn the_inner_view_gets_the_text_and_its_edits_come_out(cx: &mut TestAppContext) {
        let text = Rc::new(RefCell::new(String::new()));
        let probe: Rc<RefCell<Option<gpui_kit::Entity<Probe>>>> = Rc::default();
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |_, cx| {
                let (text, probe) = (text.clone(), probe.clone());
                cx.new(|_| {
                    let server = ServerTarget {
                        key: "p/r".into(),
                        url: "http://localhost:1/".into(),
                        root: "/p".into(),
                        start: code_assistant_core::plugin_host::ServerStart {
                            command: "true".into(),
                            url: None,
                            args: vec![],
                            env: Default::default(),
                            timeout_seconds: 1,
                            stop: None,
                        },
                    };
                    ServerGate::new(
                        "R".into(),
                        server,
                        Box::new(move |_, cx| {
                            let entity = cx.new(|_| Probe { text });
                            *probe.borrow_mut() = Some(entity.clone());
                            Box::new(entity)
                        }),
                    )
                })
            })
            .unwrap()
        });
        let gate = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let edits: Rc<RefCell<Vec<String>>> = Rc::default();
        cx.update(|_, cx| {
            let edits = edits.clone();
            cx.subscribe(&gate, move |_, edit: &TextEdit, _| {
                edits.borrow_mut().push(edit.0.clone())
            })
            .detach();
        });

        gate.update(cx, |gate, cx| gate.set_text("<diagram/>", cx));
        assert!(
            probe.borrow().is_none(),
            "no view before the server answers"
        );
        // The server answered (the lease is the service's business).
        gate.update(cx, |gate, cx| {
            gate.state = State::Ready;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(*text.borrow(), "<diagram/>");

        let inner = probe.borrow().clone().expect("the view was made");
        inner.update(cx, |_, cx| cx.emit(TextEdit("<edited/>".into())));
        cx.run_until_parked();
        assert_eq!(*edits.borrow(), ["<edited/>"]);
        gate.read_with(cx, |gate, _| assert_eq!(gate.text, "<edited/>"));
    }
}
