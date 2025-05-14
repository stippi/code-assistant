use super::file_icons;
use super::memory::MemoryView;
use super::messages::MessagesView;
use super::theme;
use super::CloseWindow;
use gpui::{
    div, prelude::*, px, white, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    MouseButton, MouseUpEvent,
};
use gpui_component::{input::TextInput, ActiveTheme, Icon, IconName};
use std::sync::{Arc, Mutex};

// Root View - handles overall layout and coordination
pub struct RootView {
    pub text_input: Entity<TextInput>,
    memory_view: Entity<MemoryView>,
    messages_view: Entity<MessagesView>,
    recent_keystrokes: Vec<gpui::Keystroke>,
    focus_handle: FocusHandle,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    // Memory view state
    memory_collapsed: bool,
}

impl RootView {
    pub fn new(
        text_input: Entity<TextInput>,
        memory_view: Entity<MemoryView>,
        messages_view: Entity<MessagesView>,
        cx: &mut Context<Self>,
        input_value: Arc<Mutex<Option<String>>>,
        input_requested: Arc<Mutex<bool>>,
    ) -> Self {
        Self {
            text_input,
            memory_view,
            messages_view,
            recent_keystrokes: vec![],
            focus_handle: cx.focus_handle(),
            input_value,
            input_requested,
            memory_collapsed: false,
        }
    }

    pub fn on_toggle_memory(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.memory_collapsed = !self.memory_collapsed;
        cx.notify();
    }

    fn on_toggle_theme(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        theme::toggle_theme(Some(window), cx);
        cx.notify();
    }

    fn on_reset_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.recent_keystrokes.clear();
        self.text_input
            .update(cx, |text_input, cx| text_input.set_text("", window, cx));
        cx.notify();
    }

    fn on_submit_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.update(cx, |text_input, cx| {
            let content = text_input.text().to_string();
            if !content.is_empty() {
                // Store input in the shared value
                let mut input_value = self.input_value.lock().unwrap();
                *input_value = Some(content);

                // Clear the input field
                text_input.set_text("", window, cx);
            }
        });
        cx.notify();
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Check if input is requested
        let is_input_requested = *self.input_requested.lock().unwrap();

        // Main container with titlebar and content
        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .bg(cx.theme().background)
            .track_focus(&self.focus_handle(cx))
            .flex()
            .flex_col() // Main container as column layout
            .size_full() // Constrain to window size
            // Custom titlebar
            .child(
                div()
                    .id("custom-titlebar")
                    .flex_none()
                    .h(px(30.))
                    .w_full()
                    .bg(cx.theme().title_bar)
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_2()
                    // Left side - title
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .text_color(cx.theme().muted_foreground)
                            .gap_2()
                            .pl_16()
                            .child("Code Assistant"),
                    )
                    // Right side - controls
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            // Theme toggle button
                            .child(
                                div()
                                    .size(px(24.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(
                                        // Show sun icon for dark mode (to switch to light)
                                        // Show moon icon for light mode (to switch to dark)
                                        if cx.theme().is_dark() {
                                            Icon::new(IconName::Sun)
                                                .text_color(cx.theme().muted_foreground)
                                        } else {
                                            Icon::new(IconName::Moon)
                                                .text_color(cx.theme().muted_foreground)
                                        },
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::on_toggle_theme),
                                    ),
                            )
                            // Memory toggle button
                            .child(
                                div()
                                    .size(px(24.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(file_icons::render_icon(
                                        &file_icons::get().get_type_icon(
                                            if self.memory_collapsed {
                                                file_icons::CHEVRON_LEFT
                                            } else {
                                                file_icons::CHEVRON_RIGHT
                                            },
                                        ),
                                        16.0,
                                        cx.theme().muted_foreground,
                                        "<>",
                                    ))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::on_toggle_memory),
                                    ),
                            ),
                    ),
            )
            // Main content area with messages, input, and sidebar
            .child(
                div()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_row() // Content as row layout for main + sidebar
                    .child(
                        // Left side with messages and input (content area)
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow() // Grow to take available space
                            .flex_shrink() // Allow shrinking if needed
                            .overflow_hidden() // Prevent overflow
                            .child(
                                // Messages display area - use the MessagesView
                                self.messages_view.clone(),
                            )
                            // Input area at the bottom
                            .child(
                                div()
                                    .id("input-area")
                                    .flex_none() // Important: don't grow or shrink
                                    .bg(cx.theme().card)
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .flex()
                                    .flex_row()
                                    .justify_between()
                                    .items_center()
                                    .p_2()
                                    .gap_2()
                                    .child(div().flex_1().child(self.text_input.clone()))
                                    .child(
                                        div()
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .rounded_md()
                                            .px_3()
                                            .py_1()
                                            .bg(if is_input_requested {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().muted
                                            })
                                            .cursor(if is_input_requested {
                                                CursorStyle::PointingHand
                                            } else {
                                                CursorStyle::OperationNotAllowed
                                            })
                                            .text_color(white())
                                            .font_weight(gpui::FontWeight(600.0))
                                            .child("Submit")
                                            .when(is_input_requested, |style| {
                                                style
                                                    .hover(|s| s.bg(cx.theme().primary_hover))
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::on_submit_click),
                                                    )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .rounded_md()
                                            .px_3()
                                            .py_1()
                                            .bg(cx.theme().danger)
                                            .text_color(white())
                                            .cursor_pointer()
                                            .font_weight(gpui::FontWeight(600.0))
                                            .child("Clear")
                                            .hover(|style| style.bg(cx.theme().danger_hover))
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::on_reset_click),
                                            ),
                                    ),
                            ),
                    )
                    // Right sidebar with memory view - only show if not collapsed
                    .when(!self.memory_collapsed, |s| {
                        s.child(
                            div()
                                .id("memory-sidebar")
                                .flex_none()
                                .w(px(260.))
                                .h_full()
                                .bg(cx.theme().sidebar)
                                .border_l_1()
                                .border_color(cx.theme().sidebar_border)
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(self.memory_view.clone()),
                        )
                    })
                    // When memory view is collapsed, show only a narrow bar
                    .when(self.memory_collapsed, |s| {
                        s.child(
                            div()
                                .id("collapsed-memory-sidebar")
                                .flex_none()
                                .w(px(40.))
                                .h_full()
                                .bg(cx.theme().sidebar)
                                .border_l_1()
                                .border_color(cx.theme().sidebar_border)
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_2()
                                .py_2()
                                .child(
                                    div()
                                        .size(px(24.))
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(file_icons::render_icon(
                                            &file_icons::get()
                                                .get_type_icon(file_icons::WORKING_MEMORY),
                                            16.0,
                                            cx.theme().muted_foreground,
                                            "🧠",
                                        )),
                                ),
                        )
                    }),
            )
    }
}
