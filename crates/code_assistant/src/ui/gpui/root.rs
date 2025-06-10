use super::auto_scroll::AutoScrollContainer;
use super::chat_sidebar::ChatSidebar;
use super::file_icons;
use super::memory::MemoryView;
use super::messages::MessagesView;
use super::theme;
use super::{CloseWindow, Gpui, UiEventSender};
use crate::persistence::ChatMetadata;
use crate::ui::gpui::ui_events::UiEvent;
use crate::ui::StreamingState;
use gpui::{
    div, prelude::*, px, rgba, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    MouseButton, MouseUpEvent,
};
use gpui_component::input::InputState;
use gpui_component::input::TextInput;
use gpui_component::ActiveTheme;
use std::sync::{Arc, Mutex};

// Root View - handles overall layout and coordination
pub struct RootView {
    pub text_input: Entity<InputState>,
    memory_view: Entity<MemoryView>,
    chat_sidebar: Entity<ChatSidebar>,
    auto_scroll_container: Entity<AutoScrollContainer<MessagesView>>,
    recent_keystrokes: Vec<gpui::Keystroke>,
    focus_handle: FocusHandle,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    // Memory view state
    memory_collapsed: bool,
    // Chat sidebar state
    chat_collapsed: bool,
    current_session_id: Option<String>,
    chat_sessions: Vec<ChatMetadata>,
    // Streaming state - shared with Gpui
    streaming_state: Arc<Mutex<StreamingState>>,
}

impl RootView {
    pub fn new(
        text_input: Entity<InputState>,
        memory_view: Entity<MemoryView>,
        messages_view: Entity<MessagesView>,
        cx: &mut Context<Self>,
        input_value: Arc<Mutex<Option<String>>>,
        input_requested: Arc<Mutex<bool>>,
        streaming_state: Arc<Mutex<StreamingState>>,
    ) -> Self {
        // Create the auto-scroll container that wraps the messages view
        let auto_scroll_container =
            cx.new(|_cx| AutoScrollContainer::new("messages", messages_view));

        // Create the chat sidebar
        let chat_sidebar = cx.new(|cx| ChatSidebar::new(cx));

        let mut root_view = Self {
            text_input,
            memory_view,
            chat_sidebar,
            auto_scroll_container,
            recent_keystrokes: vec![],
            focus_handle: cx.focus_handle(),
            input_value,
            input_requested,
            memory_collapsed: false,
            chat_collapsed: false, // Chat sidebar is visible by default
            current_session_id: None,
            chat_sessions: Vec::new(),
            streaming_state,
        };

        // Request initial chat session list
        root_view.refresh_chat_list(cx);

        root_view
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

    pub fn on_toggle_chat_sidebar(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_collapsed = !self.chat_collapsed;
        self.chat_sidebar.update(cx, |sidebar, cx| {
            sidebar.toggle_collapsed(cx);
        });
        cx.notify();
    }

    // Update chat sidebar with new sessions
    pub fn update_chat_sessions(
        &mut self,
        sessions: Vec<ChatMetadata>,
        current_session_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.chat_sessions = sessions.clone();
        self.current_session_id = current_session_id.clone();

        self.chat_sidebar.update(cx, |sidebar, cx| {
            sidebar.update_sessions(sessions, cx);
            sidebar.set_selected_session(current_session_id, cx);
        });
        cx.notify();
    }

    // Trigger refresh of chat list on startup
    pub fn refresh_chat_list(&mut self, cx: &mut Context<Self>) {
        // Request session list from agent via Gpui global
        if let Some(sender) = cx.try_global::<UiEventSender>() {
            let _ = sender.0.send(UiEvent::RefreshChatList);
        }
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

    #[allow(dead_code)]
    fn on_reset_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.recent_keystrokes.clear();
        self.text_input
            .update(cx, |text_input, cx| text_input.set_value("", window, cx));
        cx.notify();
    }

    fn on_submit_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.update(cx, |text_input, cx| {
            let content = text_input.value().to_string();
            if !content.is_empty() {
                // Store input in the shared value
                let mut input_value = self.input_value.lock().unwrap();
                *input_value = Some(content);

                // Clear the input field
                text_input.set_value("", window, cx);
            }
        });
        cx.notify();
    }

    fn on_stop_click(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Set streaming state to StopRequested
        *self.streaming_state.lock().unwrap() = StreamingState::StopRequested;
        cx.notify();
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Check if input is requested and current streaming state
        let is_input_requested = *self.input_requested.lock().unwrap();
        let current_streaming_state = *self.streaming_state.lock().unwrap();

        // Get current chat state from global Gpui
        let (chat_sessions, current_session_id) = if let Some(gpui) = cx.try_global::<Gpui>() {
            (gpui.get_chat_sessions(), gpui.get_current_session_id())
        } else {
            (Vec::new(), None)
        };

        // Update chat sidebar if needed
        if self.chat_sessions != chat_sessions || self.current_session_id != current_session_id {
            self.chat_sessions = chat_sessions.clone();
            self.current_session_id = current_session_id.clone();

            self.chat_sidebar.update(cx, |sidebar, cx| {
                sidebar.update_sessions(chat_sessions.clone(), cx);
                sidebar.set_selected_session(current_session_id.clone(), cx);
            });
        }

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
                    .h(px(48.))
                    .w_full()
                    .bg(cx.theme().title_bar)
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_4()
                    // Left side - title
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .text_color(cx.theme().muted_foreground)
                            .gap_2()
                            .pl(px(80.))
                            .child("Code Assistant"),
                    )
                    // Right side - controls
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            // Chat sidebar toggle button
                            .child(
                                div()
                                    .size(px(32.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(file_icons::render_icon(
                                        &file_icons::get()
                                            .get_type_icon(file_icons::MESSAGE_BUBBLES),
                                        18.0,
                                        cx.theme().muted_foreground,
                                        "💬",
                                    ))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::on_toggle_chat_sidebar),
                                    ),
                            )
                            // Theme toggle button
                            .child(
                                div()
                                    .size(px(32.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(file_icons::render_icon(
                                        &file_icons::get().get_type_icon(if cx.theme().is_dark() {
                                            file_icons::THEME_LIGHT
                                        } else {
                                            file_icons::THEME_DARK
                                        }),
                                        18.0,
                                        cx.theme().muted_foreground,
                                        if cx.theme().is_dark() { "*" } else { "c" },
                                    ))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::on_toggle_theme),
                                    ),
                            )
                            // Memory toggle button
                            .child(
                                div()
                                    .size(px(32.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(file_icons::render_icon(
                                        &file_icons::get().get_type_icon(
                                            if self.memory_collapsed {
                                                file_icons::PANEL_RIGHT_OPEN
                                            } else {
                                                file_icons::PANEL_RIGHT_CLOSE
                                            },
                                        ),
                                        18.0,
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
            // Main content area with chat sidebar, messages+input, and memory sidebar (3-column layout)
            .child(
                div()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_row() // 3-column layout: chat | messages+input | memory
                    // Left sidebar: Chat sessions
                    .child(self.chat_sidebar.clone())
                    .child(
                        // Center: Messages and input (content area)
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow() // Grow to take available space
                            .flex_shrink() // Allow shrinking if needed
                            .overflow_hidden() // Prevent overflow
                            .child(
                                // Messages display area - use the AutoScrollContainer wrapping MessagesView
                                self.auto_scroll_container.clone(),
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
                                    .child({
                                        let text_input_handle =
                                            self.text_input.read(cx).focus_handle(cx);
                                        let is_focused = text_input_handle.is_focused(window);

                                        div()
                                            .flex_1()
                                            .border_1()
                                            .border_color(if is_focused {
                                                cx.theme().primary // Blue border when focused
                                            } else if cx.theme().is_dark() {
                                                rgba(0x555555FF).into() // Brighter border for dark theme
                                            } else {
                                                rgba(0x999999FF).into() // Darker border for light theme
                                            })
                                            .rounded_md()
                                            .track_focus(&text_input_handle)
                                            .child(TextInput::new(&self.text_input))
                                    })
                                    .child({
                                        // Create button based on streaming state
                                        match current_streaming_state {
                                            StreamingState::Idle => {
                                                // Show send button, enabled only if input is requested
                                                let mut button = div()
                                                    .size(px(40.))
                                                    .rounded_sm()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .cursor(if is_input_requested {
                                                        CursorStyle::PointingHand
                                                    } else {
                                                        CursorStyle::OperationNotAllowed
                                                    })
                                                    .child(file_icons::render_icon(
                                                        &file_icons::get()
                                                            .get_type_icon(file_icons::SEND),
                                                        22.0,
                                                        if is_input_requested {
                                                            cx.theme().primary
                                                        } else {
                                                            cx.theme().muted_foreground
                                                        },
                                                        ">",
                                                    ));

                                                if is_input_requested {
                                                    button = button
                                                        .hover(|s| s.bg(cx.theme().muted))
                                                        .on_mouse_up(
                                                            MouseButton::Left,
                                                            cx.listener(Self::on_submit_click),
                                                        );
                                                }

                                                button
                                            }
                                            StreamingState::Streaming => {
                                                // Show stop button, enabled
                                                div()
                                                    .size(px(40.))
                                                    .rounded_sm()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .cursor(CursorStyle::PointingHand)
                                                    .hover(|s| s.bg(cx.theme().muted))
                                                    .child(file_icons::render_icon(
                                                        &file_icons::get()
                                                            .get_type_icon(file_icons::STOP),
                                                        22.0,
                                                        cx.theme().danger,
                                                        "⬜",
                                                    ))
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::on_stop_click),
                                                    )
                                            }
                                            StreamingState::StopRequested => {
                                                // Show stop button, disabled/grayed out
                                                div()
                                                    .size(px(40.))
                                                    .rounded_sm()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .cursor(CursorStyle::OperationNotAllowed)
                                                    .child(file_icons::render_icon(
                                                        &file_icons::get()
                                                            .get_type_icon(file_icons::STOP),
                                                        22.0,
                                                        cx.theme().muted_foreground,
                                                        "⬜",
                                                    ))
                                            }
                                        }
                                    }),
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
