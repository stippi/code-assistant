//! The viewed session's name in the title bar. A click turns it into a text
//! field: Enter renames the session, Escape keeps the old name, and so does
//! clicking elsewhere without a change.

use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{
    Context, Entity, EventEmitter, InteractiveElement, Pixels, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px, rems,
};

/// Shown for a session nobody has named yet.
const UNNAMED: &str = "Unnamed chat";

pub enum SessionTitleEvent {
    /// The user gave the session a new, non-empty name.
    Renamed { session_id: String, name: String },
}

pub struct SessionTitle {
    session_id: Option<String>,
    name: SharedString,
    /// The name given here, shown until the session list carries it.
    renamed_to: Option<SharedString>,
    input: Entity<InputState>,
    editing: bool,
    _input_subscription: Subscription,
}

impl SessionTitle {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(&input, window, Self::on_input_event);
        Self {
            session_id: None,
            name: SharedString::default(),
            renamed_to: None,
            input,
            editing: false,
            _input_subscription: subscription,
        }
    }

    /// Show `name` for the session `session_id`. Switching to another
    /// session drops an unfinished rename.
    pub fn set_session(
        &mut self,
        session_id: Option<String>,
        name: SharedString,
        cx: &mut Context<Self>,
    ) {
        if self.session_id != session_id {
            self.session_id = session_id;
            self.editing = false;
            self.renamed_to = None;
        }
        if let Some(renamed_to) = &self.renamed_to {
            if *renamed_to != name {
                return;
            }
            self.renamed_to = None;
        }
        if self.name != name {
            self.name = name;
            cx.notify();
        }
    }

    pub fn start_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_id.is_none() {
            return;
        }
        self.editing = true;
        let name = self.name.clone();
        self.input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Rename the session to what the field holds, unless that is empty or
    /// the current name.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.editing) {
            return;
        }
        let name = self.input.read(cx).value().trim().to_string();
        if let Some(session_id) = self.session_id.clone()
            && !name.is_empty()
            && name != self.name.as_ref()
        {
            // Shown at once; the stored name follows as session metadata.
            self.name = name.clone().into();
            self.renamed_to = Some(self.name.clone());
            cx.emit(SessionTitleEvent::Renamed { session_id, name });
        }
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
        cx.notify();
    }

    fn on_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => self.commit(cx),
            // The field's width follows its text.
            InputEvent::Change => cx.notify(),
            InputEvent::Focus => {}
        }
    }

    /// Escape reaches the field as the app's `CancelAgent`, or as the
    /// field's own `Escape` when that has nothing to clear. Either cancels
    /// the rename only, never the agent.
    fn on_escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    fn on_cancel_agent(&mut self, _: &crate::CancelAgent, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel(cx);
    }
}

impl EventEmitter<SessionTitleEvent> for SessionTitle {}

/// The text field's size. The title copies its box, so the text stays put
/// when the field opens.
const FIELD_SIZE: Size = Size::Small;
/// The room the field keeps free behind the cursor before it scrolls its
/// text (gpui-base's `RIGHT_MARGIN`, 10px), plus 1px for rounding. With less,
/// the text scrolls left while the field still grows.
const CURSOR_ROOM: Pixels = px(11.);
/// How wide the title and the field may grow.
const MAX_WIDTH: f32 = 480.;

/// The box the text field draws its text in at [`FIELD_SIZE`]: a 1px border,
/// its padding, height, text size and line height.
fn field_box<E: Styled>(element: E) -> E {
    element
        .flex()
        .items_center()
        .h_6()
        .px(FIELD_SIZE.input_px())
        .border_1()
        .text_sm()
        .line_height(rems(1.25))
}

impl Render for SessionTitle {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.session_id.is_none() {
            return div().id("session-title");
        }
        if self.editing {
            // The field grows with its text: an invisible copy of the text
            // in the same box sets the width, the field lies on top of it.
            let text = self.input.read(cx).value();
            return div()
                .id("session-title")
                .debug_selector(|| "session-title-input".into())
                .relative()
                .min_w_0()
                .max_w(px(MAX_WIDTH))
                .on_action(cx.listener(Self::on_escape))
                .on_action(cx.listener(Self::on_cancel_agent))
                .child(
                    field_box(div())
                        .invisible()
                        .whitespace_nowrap()
                        .pr(FIELD_SIZE.input_px() + CURSOR_ROOM)
                        .child(text),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(Input::new(&self.input).with_size(FIELD_SIZE)),
                );
        }
        let unnamed = self.name.is_empty();
        field_box(div())
            .id("session-title")
            .debug_selector(|| "session-title".into())
            .min_w_0()
            .max_w(px(MAX_WIDTH))
            .border_color(gpui_kit::transparent_black())
            .rounded(cx.theme().radius)
            .cursor_text()
            .text_color(if unnamed {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            })
            .hover(|style| style.bg(cx.theme().muted))
            .tooltip(|window, cx| Tooltip::new("Rename").build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.start_editing(window, cx)))
            .child(div().min_w_0().truncate().child(if unnamed {
                SharedString::new_static(UNNAMED)
            } else {
                self.name.clone()
            }))
    }
}
