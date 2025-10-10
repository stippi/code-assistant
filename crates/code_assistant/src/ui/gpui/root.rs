use super::auto_scroll::AutoScrollContainer;
use super::chat_sidebar::{ChatSidebar, ChatSidebarEvent};
use super::file_icons;
use super::input_area::{InputArea, InputAreaEvent};
use super::memory::MemoryView;
use super::messages::MessagesView;
use super::model_selector::{ModelSelector, ModelSelectorEvent};
use super::theme;
use super::BackendEvent;
use super::{CloseWindow, Gpui, UiEventSender};
use crate::persistence::ChatMetadata;
use crate::ui::ui_events::UiEvent;
use gpui::{
    bounce, div, ease_in_out, percentage, prelude::*, px, rgba, svg, Animation, AnimationExt, App,
    Context, Entity, FocusHandle, Focusable, MouseButton, MouseUpEvent, SharedString, Subscription,
    Transformation,
};
use gpui_component::ActiveTheme;
use tracing::{debug, error, trace, warn};

// Root View - handles overall layout and coordination
pub struct RootView {
    input_area: Entity<InputArea>,
    memory_view: Entity<MemoryView>,
    chat_sidebar: Entity<ChatSidebar>,
    auto_scroll_container: Entity<AutoScrollContainer<MessagesView>>,
    model_selector: Entity<ModelSelector>,
    recent_keystrokes: Vec<gpui::Keystroke>,
    focus_handle: FocusHandle,
    // Memory view state
    memory_collapsed: bool,
    // Chat sidebar state
    chat_collapsed: bool,
    current_session_id: Option<String>,
    chat_sessions: Vec<ChatMetadata>,
    // Subscription to input area events
    _input_area_subscription: Subscription,
    _chat_sidebar_subscription: Subscription,
    _model_selector_subscription: Subscription,
}

impl RootView {
    pub fn new(
        memory_view: Entity<MemoryView>,
        messages_view: Entity<MessagesView>,
        chat_sidebar: Entity<ChatSidebar>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Create the auto-scroll container that wraps the messages view
        let auto_scroll_container =
            cx.new(|_cx| AutoScrollContainer::new("messages", messages_view));

        // Create the input area
        let input_area = cx.new(|cx| InputArea::new(window, cx));

        // Create the model selector
        let model_selector = cx.new(ModelSelector::new);

        // Subscribe to input area events
        let input_area_subscription =
            cx.subscribe_in(&input_area, window, Self::on_input_area_event);

        // Subscribe to chat sidebar events
        let chat_sidebar_subscription =
            cx.subscribe_in(&chat_sidebar, window, Self::on_chat_sidebar_event);

        // Subscribe to model selector events
        let model_selector_subscription =
            cx.subscribe_in(&model_selector, window, Self::on_model_selector_event);

        let mut root_view = Self {
            input_area,
            memory_view,
            chat_sidebar,
            auto_scroll_container,
            model_selector,
            recent_keystrokes: vec![],
            focus_handle: cx.focus_handle(),
            memory_collapsed: false,
            chat_collapsed: false, // Chat sidebar is visible by default
            current_session_id: None,
            chat_sessions: Vec::new(),
            _input_area_subscription: input_area_subscription,
            _chat_sidebar_subscription: chat_sidebar_subscription,
            _model_selector_subscription: model_selector_subscription,
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

    // Trigger refresh of chat list on startup
    pub fn refresh_chat_list(&mut self, cx: &mut Context<Self>) {
        debug!("Requesting chat list refresh");
        // Request session list from agent via Gpui global
        if let Some(sender) = cx.try_global::<UiEventSender>() {
            trace!("Sending RefreshChatList event");
            let _ = sender.0.try_send(UiEvent::RefreshChatList);
        } else {
            warn!("No UiEventSender global available");
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
        self.input_area
            .update(cx, |input_area, cx| input_area.clear(window, cx));
        cx.notify();
    }

    /// Handle InputArea events
    fn on_input_area_event(
        &mut self,
        _input_area: &Entity<InputArea>,
        event: &InputAreaEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputAreaEvent::MessageSubmitted {
                content,
                attachments,
            } => {
                if let Some(session_id) = self.current_session_id.clone() {
                    self.send_message(&session_id, content.clone(), attachments.clone(), cx);
                }
            }
            InputAreaEvent::ContentChanged {
                content,
                attachments,
            } => {
                if let Some(session_id) = &self.current_session_id {
                    self.save_draft_for_session(session_id, content, attachments, cx);
                }
            }
            InputAreaEvent::FocusRequested => {
                // Handle focus request if needed
            }
            InputAreaEvent::CancelRequested => {
                // Handle cancel/stop request
                if let Some(session_id) = &self.current_session_id {
                    if let Some(gpui) = cx.try_global::<Gpui>() {
                        gpui.session_stop_requests
                            .lock()
                            .unwrap()
                            .insert(session_id.clone());
                    }
                }
                cx.notify();
            }
            InputAreaEvent::ClearDraftRequested => {
                // Clear draft immediately and synchronously
                if let Some(session_id) = &self.current_session_id {
                    if let Some(gpui) = cx.try_global::<Gpui>() {
                        gpui.clear_draft_for_session(session_id);
                    }
                }
            }
        }
    }

    /// Handle ChatSidebar events
    fn on_chat_sidebar_event(
        &mut self,
        _chat_sidebar: &Entity<ChatSidebar>,
        event: &ChatSidebarEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let gpui = cx
            .try_global::<Gpui>()
            .expect("Failed to obtain Gpui global");
        if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
            match event {
                ChatSidebarEvent::SessionSelected { session_id } => {
                    let _ = sender.try_send(BackendEvent::LoadSession {
                        session_id: session_id.clone(),
                    });
                }
                ChatSidebarEvent::SessionDeleteRequested { session_id } => {
                    let _ = sender.try_send(BackendEvent::DeleteSession {
                        session_id: session_id.clone(),
                    });
                }
                ChatSidebarEvent::NewSessionRequested { name } => {
                    let _ = sender.try_send(BackendEvent::CreateNewSession { name: name.clone() });
                }
            }
        } else {
            error!("Failed to lock backend event sender");
        }
    }

    /// Handle ModelSelector events
    fn on_model_selector_event(
        &mut self,
        _model_selector: &Entity<ModelSelector>,
        event: &ModelSelectorEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ModelSelectorEvent::ModelChanged { model_name } => {
                debug!("Model selection changed to: {}", model_name);

                // Send model switch event if we have an active session
                if let Some(session_id) = &self.current_session_id {
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::SwitchModel {
                            session_id: session_id.clone(),
                            model_name: model_name.clone(),
                        });
                    } else {
                        error!("Failed to lock backend event sender");
                    }
                }
            }
            ModelSelectorEvent::DropdownToggled { is_open: _ } => {
                // Handle dropdown state changes if needed
            }
        }
    }

    fn send_message(
        &mut self,
        session_id: &str,
        content: String,
        attachments: Vec<crate::persistence::DraftAttachment>,
        cx: &mut Context<Self>,
    ) {
        if content.trim().is_empty() && attachments.is_empty() {
            return;
        }

        // Send user message event if we have an active session
        if let Some(sender) = cx.try_global::<UiEventSender>() {
            // Check if agent is running by looking at activity state
            let current_activity_state = if let Some(gpui) = cx.try_global::<Gpui>() {
                gpui.current_session_activity_state.lock().unwrap().clone()
            } else {
                None
            };

            let agent_is_running = if let Some(state) = current_activity_state {
                !matches!(state, crate::session::instance::SessionActivityState::Idle)
            } else {
                false
            };

            if agent_is_running {
                // Queue the message for the running agent
                tracing::info!(
                    "RootView: Queuing user message for running agent in session {}: {} (with {} attachments)",
                    session_id,
                    content,
                    attachments.len()
                );
                let _ = sender.0.try_send(UiEvent::QueueUserMessage {
                    message: content.clone(),
                    session_id: session_id.to_string(),
                    attachments: attachments.clone(),
                });
            } else {
                // Send message normally (agent is idle)
                tracing::info!(
                    "RootView: Sending user message to session {}: {} (with {} attachments)",
                    session_id,
                    content,
                    attachments.len()
                );
                let _ = sender.0.try_send(UiEvent::SendUserMessage {
                    message: content.clone(),
                    session_id: session_id.to_string(),
                    attachments: attachments.clone(),
                });
            }
        }
    }

    fn save_draft_for_session(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[crate::persistence::DraftAttachment],
        cx: &mut Context<Self>,
    ) {
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.save_draft_for_session(session_id, content, attachments);
        }
    }

    fn on_cancel_agent(
        &mut self,
        _: &crate::ui::gpui::CancelAgent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Add current session to stop requests
        if let Some(session_id) = &self.current_session_id {
            if let Some(gpui) = cx.try_global::<Gpui>() {
                gpui.session_stop_requests
                    .lock()
                    .unwrap()
                    .insert(session_id.clone());
            }
        }
        cx.notify();
    }

    /// Render the floating status popover if needed
    fn render_status_popover(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        // Get current error and session activity state from global Gpui
        let (current_error, current_activity_state) = if let Some(gpui) = cx.try_global::<Gpui>() {
            (
                gpui.get_current_error(),
                gpui.current_session_activity_state.lock().unwrap().clone(),
            )
        } else {
            (None, None)
        };

        // Check for error first (higher priority than activity states)
        if let Some(error_message) = current_error {
            let (bg_color, border_color, text_color) = if cx.theme().is_dark() {
                (
                    rgba(0x7F1D1D80), // Dark red background with transparency
                    rgba(0xEF4444FF), // Red border
                    rgba(0xFCA5A5FF), // Light red text
                )
            } else {
                (
                    rgba(0xFEF2F2FF), // Light red background
                    rgba(0xF87171FF), // Red border
                    rgba(0xDC2626FF), // Dark red text
                )
            };

            // Return the error popover positioned at bottom
            return vec![div()
                .absolute()
                .bottom(px(80.)) // Above input area (input is ~76px tall)
                .left(px(0.))
                .right(px(0.))
                .flex()
                .justify_center() // Center the content horizontally
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .bg(bg_color)
                        .border_1()
                        .border_color(border_color)
                        .rounded_lg()
                        .shadow_lg()
                        .flex()
                        .items_start() // Align items to top for multi-line text
                        .gap_2()
                        .max_w(px(600.)) // Limit width for long error messages
                        .min_w(px(200.)) // Ensure minimum width
                        .child(
                            div()
                                .flex_none()
                                .mt(px(1.)) // Slight top margin to align with first line of text
                                .child(
                                    svg()
                                        .size(px(14.))
                                        .path(SharedString::from("icons/circle_stop.svg"))
                                        .text_color(text_color),
                                ),
                        )
                        .child(
                            div()
                                .text_color(text_color)
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight(500.0))
                                .flex_grow()
                                .whitespace_normal() // Enable text wrapping
                                .line_height(px(14.)) // Set line height for better readability
                                .child(error_message),
                        )
                        .child(
                            // Add a close button
                            div()
                                .flex_none()
                                .size(px(20.))
                                .rounded_sm()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().muted.opacity(0.3))) // Match other icon button hover effects
                                .child(
                                    svg()
                                        .size(px(12.))
                                        .path(SharedString::from("icons/close.svg"))
                                        .text_color(text_color),
                                )
                                .on_mouse_up(gpui::MouseButton::Left, |_, _, cx| {
                                    // Clear the error when close button is clicked
                                    if let Some(sender) = cx.try_global::<UiEventSender>() {
                                        let _ = sender.0.try_send(UiEvent::ClearError);
                                    }
                                }),
                        ),
                )
                .into_any_element()];
        }

        if let Some(activity_state) = current_activity_state {
            if matches!(
                activity_state,
                crate::session::instance::SessionActivityState::WaitingForResponse
                    | crate::session::instance::SessionActivityState::RateLimited { .. }
            ) {
                let (message_text, bg_color, border_color, text_color) = match activity_state {
                    crate::session::instance::SessionActivityState::RateLimited {
                        seconds_remaining,
                    } => (
                        format!("Rate limited - retrying in {seconds_remaining}s..."),
                        if cx.theme().is_dark() {
                            rgba(0x43140780) // Dark orange background with transparency
                        } else {
                            rgba(0xFFF7EDFF) // Light orange background
                        },
                        if cx.theme().is_dark() {
                            rgba(0xF97316FF) // Orange border
                        } else {
                            rgba(0xFB923CFF) // Stronger orange border
                        },
                        if cx.theme().is_dark() {
                            rgba(0xFB923CFF) // Orange text
                        } else {
                            rgba(0xEA580CFF) // Full orange text
                        },
                    ),
                    crate::session::instance::SessionActivityState::WaitingForResponse => (
                        "Waiting for response...".to_string(),
                        if cx.theme().is_dark() {
                            rgba(0x1E3A8A80) // Dark blue background with transparency
                        } else {
                            rgba(0xEFF6FFFF) // Light blue background
                        },
                        if cx.theme().is_dark() {
                            rgba(0x3B82F6FF) // Blue border
                        } else {
                            rgba(0x60A5FAFF) // Stronger blue border
                        },
                        if cx.theme().is_dark() {
                            rgba(0x60A5FAFF) // Blue text
                        } else {
                            rgba(0x2563EBFF) // Full blue text
                        },
                    ),
                    _ => unreachable!(),
                };

                // Return the floating popover positioned at bottom
                return vec![div()
                    .absolute()
                    .bottom(px(80.)) // Above input area (input is ~76px tall)
                    .left(px(0.))
                    .right(px(0.))
                    .flex()
                    .justify_center() // Center the content horizontally
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .bg(bg_color)
                            .border_1()
                            .border_color(border_color)
                            .rounded_lg()
                            .shadow_lg()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                svg()
                                    .size(px(14.))
                                    .path(SharedString::from("icons/arrow_circle.svg"))
                                    .text_color(text_color)
                                    .with_animation(
                                        "floating_loading_indicator",
                                        Animation::new(std::time::Duration::from_secs(2))
                                            .repeat()
                                            .with_easing(bounce(ease_in_out)),
                                        |svg, delta| {
                                            svg.with_transformation(Transformation::rotate(
                                                percentage(delta),
                                            ))
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(text_color)
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight(500.0))
                                    .child(message_text),
                            ),
                    )
                    .into_any_element()];
            }
        }

        vec![] // No popover to show
    }

    // Handle session change: load new draft (no need to save current - already saved on every change)
    fn handle_session_change(
        &mut self,
        _previous_session_id: Option<String>,
        new_session_id: Option<String>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let gpui = cx.try_global::<Gpui>();

        // Determine what value to set in the input field and load attachments
        let (input_value, attachments) = if let (Some(new_id), Some(gpui)) = (new_session_id, &gpui)
        {
            if let Some((draft_text, draft_attachments)) = gpui.load_draft_for_session(&new_id) {
                debug!(
                    "Loading draft for new session {}: {} characters, {} attachments",
                    new_id,
                    draft_text.len(),
                    draft_attachments.len()
                );
                (draft_text, draft_attachments)
            } else {
                debug!("No draft found for new session: {}", new_id);
                ("".to_string(), Vec::new())
            }
        } else {
            // No new session, clear the text input and attachments
            debug!("No new session, clearing text input and attachments");
            ("".to_string(), Vec::new())
        };

        // Update the input area with the new content
        self.input_area.update(cx, |input_area, cx| {
            input_area.set_content(input_value, attachments, window, cx);
        });
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get current chat state from global Gpui
        let (chat_sessions, current_session_id, current_activity_state) =
            if let Some(gpui) = cx.try_global::<Gpui>() {
                (
                    gpui.get_chat_sessions(),
                    gpui.get_current_session_id(),
                    gpui.current_session_activity_state.lock().unwrap().clone(),
                )
            } else {
                (Vec::new(), None, None)
            };

        // Update chat sidebar if needed
        if self.chat_sessions != chat_sessions || self.current_session_id != current_session_id {
            let previous_session_id = self.current_session_id.clone();
            self.chat_sessions = chat_sessions.clone();
            self.current_session_id = current_session_id.clone();

            self.chat_sidebar.update(cx, |sidebar, cx| {
                sidebar.update_sessions(chat_sessions.clone(), cx);
                sidebar.set_selected_session(current_session_id.clone(), cx);
            });

            // Handle session change: load draft for new session
            if previous_session_id != current_session_id {
                self.handle_session_change(
                    previous_session_id,
                    current_session_id.clone(),
                    window,
                    cx,
                );
            }
        }

        // Update InputArea with current agent state
        let agent_is_running = if let Some(state) = &current_activity_state {
            !matches!(state, crate::session::instance::SessionActivityState::Idle)
        } else {
            false
        };

        let cancel_enabled = if agent_is_running {
            if let (Some(gpui), Some(session_id)) = (cx.try_global::<Gpui>(), &current_session_id) {
                !gpui
                    .session_stop_requests
                    .lock()
                    .unwrap()
                    .contains(session_id)
            } else {
                true
            }
        } else {
            false
        };

        self.input_area.update(cx, |input_area, _cx| {
            input_area.set_agent_state(agent_is_running, cancel_enabled);
        });

        // Main container with titlebar and content
        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .on_action(cx.listener(Self::on_cancel_agent))
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
                        // Center: Messages and input (content area) with floating popover
                        div()
                            .relative() // For popover positioning
                            .bg(cx.theme().popover)
                            .flex()
                            .flex_col()
                            .flex_grow() // Grow to take available space
                            .flex_shrink() // Allow shrinking if needed
                            .overflow_hidden() // Prevent overflow
                            .child(
                                // Messages display area - use the AutoScrollContainer wrapping MessagesView
                                self.auto_scroll_container.clone(),
                            )
                            // Status popover - positioned at bottom center
                            .children(self.render_status_popover(cx))
                            // Model selector above input area
                            .child(
                                div()
                                    .flex_none()
                                    .px_4()
                                    .py_2()
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().background)
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Model:"),
                                            )
                                            .child(
                                                div()
                                                    .w(px(200.))
                                                    .child(self.model_selector.clone()),
                                            ),
                                    ),
                            )
                            // Input area at the bottom - now using the InputArea component
                            .child(self.input_area.clone()),
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
