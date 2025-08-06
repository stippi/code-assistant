use super::attachment::{AttachmentEvent, AttachmentView};
use super::auto_scroll::AutoScrollContainer;
use super::chat_sidebar::ChatSidebar;
use super::file_icons;
use super::memory::MemoryView;
use super::messages::MessagesView;
use super::theme;
use super::{CloseWindow, Gpui, UiEventSender};
use crate::persistence::{ChatMetadata, DraftAttachment};
use crate::ui::ui_events::UiEvent;

use base64::Engine;
use gpui::{
    bounce, div, ease_in_out, percentage, prelude::*, px, rgba, svg, Animation, AnimationExt, App,
    ClipboardEntry, Context, CursorStyle, Entity, FocusHandle, Focusable, MouseButton,
    MouseUpEvent, SharedString, Transformation,
};
use gpui_component::input::TextInput;
use gpui_component::input::{InputEvent, InputState, Paste};
use gpui_component::ActiveTheme;
use std::sync::{Arc, Mutex};
use tracing::{debug, trace, warn};

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
    // Subscription to text input events
    _input_subscription: gpui::Subscription,
    // Attachments for the current message being composed
    attachments: Vec<DraftAttachment>,
    // Attachment view entities
    attachment_views: Vec<Entity<AttachmentView>>,
}

impl RootView {
    pub fn new(
        text_input: Entity<InputState>,
        memory_view: Entity<MemoryView>,
        messages_view: Entity<MessagesView>,
        chat_sidebar: Entity<ChatSidebar>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
        input_value: Arc<Mutex<Option<String>>>,
        input_requested: Arc<Mutex<bool>>,
    ) -> Self {
        // Create the auto-scroll container that wraps the messages view
        let auto_scroll_container =
            cx.new(|_cx| AutoScrollContainer::new("messages", messages_view));

        // Subscribe to text input events
        let input_subscription = cx.subscribe_in(&text_input, window, Self::on_input_event);

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
            _input_subscription: input_subscription,
            attachments: Vec::new(),
            attachment_views: Vec::new(),
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
        self.text_input
            .update(cx, |text_input, cx| text_input.set_value("", window, cx));
        cx.notify();
    }

    fn send_message(
        &mut self,
        session_id: &str,
        content: String,
        text_input: &Entity<InputState>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if content.trim().is_empty() && self.attachments.is_empty() {
            return;
        }

        // V1 mode: Store input in the shared value if input is requested
        let is_input_requested = *self.input_requested.lock().unwrap();
        if is_input_requested {
            let mut input_value = self.input_value.lock().unwrap();
            *input_value = Some(content.clone());
        }

        // V2 mode: Send user message event if we have an active session
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
                    self.attachments.len()
                );
                let _ = sender.0.try_send(UiEvent::QueueUserMessage {
                    message: content.clone(),
                    session_id: session_id.to_string(),
                    attachments: self.attachments.clone(),
                });
            } else {
                // Send message normally (agent is idle)
                tracing::info!(
                    "RootView: Sending user message to session {}: {} (with {} attachments)",
                    session_id,
                    content,
                    self.attachments.len()
                );
                let _ = sender.0.try_send(UiEvent::SendUserMessage {
                    message: content.clone(),
                    session_id: session_id.to_string(),
                    attachments: self.attachments.clone(),
                });
            }
        }

        // Clear draft when message is sent (before clearing input to avoid race condition)
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.clear_draft_for_session(session_id);
        }

        // Clear attachments
        self.attachments.clear();

        // Clear the input field
        text_input.update(cx, |text_input, cx| {
            text_input.set_value("", window, cx);
        });
    }

    fn on_submit_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session_id) = self.current_session_id.clone() {
            let content = self.text_input.read(cx).value().to_string();
            let text_input = self.text_input.clone();
            self.send_message(&session_id, content, &text_input, window, cx);
        }
        cx.notify();
    }

    fn on_stop_click(
        &mut self,
        _: &MouseUpEvent,
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

    fn on_paste(&mut self, _: &Paste, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(clipboard_item) = cx.read_from_clipboard() {
            for entry in clipboard_item.into_entries() {
                if let ClipboardEntry::Image(image) = entry {
                    // Create a DraftAttachment from the image
                    let attachment = DraftAttachment::Image {
                        content: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                        mime_type: image.format.mime_type().to_string(),
                    };

                    self.attachments.push(attachment);

                    // Rebuild attachment views
                    self.rebuild_attachment_views(cx);

                    // Save attachments to draft if we have a current session
                    if let Some(session_id) = &self.current_session_id {
                        self.save_draft_with_attachments(session_id, cx);
                    }

                    cx.notify();
                }
            }
        }
    }

    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.attachments.len() {
            self.attachments.remove(index);

            // Rebuild attachment views with updated indices
            self.rebuild_attachment_views(cx);

            // Update draft
            if let Some(session_id) = &self.current_session_id {
                self.save_draft_with_attachments(session_id, cx);
            }

            cx.notify();
        }
    }

    fn save_draft_with_attachments(&self, session_id: &str, cx: &mut Context<Self>) {
        if let Some(gpui) = cx.try_global::<Gpui>() {
            let text = self.text_input.read(cx).value().to_string();
            gpui.save_draft_for_session(session_id, &text, &self.attachments);
        }
    }

    /// Rebuild attachment views when attachments change
    fn rebuild_attachment_views(&mut self, cx: &mut Context<Self>) {
        self.attachment_views.clear();

        for (index, attachment) in self.attachments.iter().enumerate() {
            let attachment_view = cx.new(|cx| AttachmentView::new(attachment.clone(), index, cx));

            // Subscribe to attachment events
            cx.subscribe(
                &attachment_view,
                |view, _attachment_view, event: &AttachmentEvent, cx| match event {
                    AttachmentEvent::Remove(index) => {
                        view.remove_attachment(*index, cx);
                    }
                },
            )
            .detach();

            self.attachment_views.push(attachment_view);
        }
    }

    // Handle text input events for draft functionality
    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change(text) => {
                if let Some(session_id) = &self.current_session_id {
                    // Save draft with attachments immediately for now (no debouncing for simplicity)
                    if let Some(gpui) = cx.try_global::<Gpui>() {
                        gpui.save_draft_for_session(session_id, text, &self.attachments);
                    }
                }
            }
            InputEvent::Focus => {}
            InputEvent::Blur => {}
            InputEvent::PressEnter { secondary } => {
                // Only send message on plain ENTER (not with modifiers)
                if !secondary {
                    if let Some(session_id) = self.current_session_id.clone() {
                        // IMMEDIATELY clear any existing draft to prevent the newline from being saved
                        if let Some(gpui) = cx.try_global::<Gpui>() {
                            gpui.clear_draft_for_session(&session_id);
                        }

                        // Get current text (this might include the newline that was just added)
                        let current_text = self.text_input.read(cx).value().to_string();
                        // Remove trailing newline if present (from ENTER key press)
                        let cleaned_text = current_text.trim_end_matches('\n').to_string();

                        let text_input = self.text_input.clone();
                        self.send_message(&session_id, cleaned_text, &text_input, window, cx);
                    }
                }
                // If secondary is true, do nothing - modifiers will be handled by InsertLineBreak action
            }
        }
    }

    /// Render the floating status popover if needed
    fn render_status_popover(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        // Get current session activity state from global Gpui
        let current_activity_state = if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.current_session_activity_state.lock().unwrap().clone()
        } else {
            None
        };

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

        // Update attachments and rebuild views
        self.attachments = attachments;
        self.rebuild_attachment_views(cx);

        self.text_input.update(cx, |text_input, cx| {
            text_input.set_value(input_value, window, cx);
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
        // Check if input is requested
        let is_input_requested = *self.input_requested.lock().unwrap();

        // Get current chat state from global Gpui
        let (chat_sessions, current_session_id) = if let Some(gpui) = cx.try_global::<Gpui>() {
            (gpui.get_chat_sessions(), gpui.get_current_session_id())
        } else {
            (Vec::new(), None)
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

        // Main container with titlebar and content
        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .on_action({
                let text_input_handle = self.text_input.clone();
                move |_: &crate::ui::gpui::InsertLineBreak, window, cx| {
                    // Insert a line break at the current cursor position
                    text_input_handle.update(cx, |input_state, cx| {
                        input_state.insert("\n", window, cx);
                    });
                }
            })
            .on_action(cx.listener(Self::on_cancel_agent))
            .on_action(cx.listener(Self::on_paste))
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
                            .bg(cx.theme().card)
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
                            // Input area at the bottom
                            .child(
                                div()
                                    .id("input-area")
                                    .flex_none() // Important: don't grow or shrink
                                    .bg(cx.theme().card)
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .flex()
                                    .flex_col() // Changed to column to accommodate attachments area
                                    .gap_2()
                                    // Attachments area - show image previews when available
                                    .when(!self.attachments.is_empty(), |parent| {
                                        parent.child(
                                            div()
                                                .p_2()
                                                .border_b_1()
                                                .border_color(cx.theme().border)
                                                .flex()
                                                .flex_row()
                                                .gap_2()
                                                .flex_wrap()
                                                .children(
                                                    self.attachment_views.iter().cloned()
                                                )
                                        )
                                    })

                                    // Main input row
                                    .child(
                                        div()
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
                                            .border(if is_focused {
                                                px(2.)
                                            } else {
                                                px(1.)
                                            })
                                            .p(if is_focused {
                                                px(0.)
                                            } else {
                                                px(1.)
                                            })
                                            .border_color(if is_focused {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().sidebar_border
                                            })
                                            .rounded_md()
                                            .track_focus(&text_input_handle)
                                            .child(TextInput::new(&self.text_input).appearance(false))
                                    })
                                    .children({
                                        // Get current session activity state from global Gpui
                                        let current_activity_state = if let Some(gpui) = cx.try_global::<Gpui>() {
                                            gpui.current_session_activity_state.lock().unwrap().clone()
                                        } else {
                                            None
                                        };

                                        // Determine if agent is running
                                        let agent_is_running = if let Some(state) = current_activity_state {
                                            !matches!(state, crate::session::instance::SessionActivityState::Idle)
                                        } else {
                                            false
                                        };

                                        // Check if text input has content
                                        let has_input_content = !self.text_input.read(cx).value().trim().is_empty();

                                        let mut buttons = Vec::new();

                                        // Send button - enabled when input has content
                                        let send_enabled = has_input_content && (is_input_requested || current_session_id.is_some());
                                        let mut send_button = div()
                                            .size(px(40.))
                                            .rounded_sm()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor(if send_enabled {
                                                CursorStyle::PointingHand
                                            } else {
                                                CursorStyle::OperationNotAllowed
                                            })
                                            .child(file_icons::render_icon(
                                                &file_icons::get().get_type_icon(file_icons::SEND),
                                                22.0,
                                                if send_enabled {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().muted_foreground
                                                },
                                                ">",
                                            ));

                                        if send_enabled {
                                            send_button = send_button
                                                .hover(|s| s.bg(cx.theme().muted))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(Self::on_submit_click),
                                                );
                                        }
                                        buttons.push(send_button);

                                        // Cancel button - enabled only when current session's agent is running
                                        let mut cancel_enabled = agent_is_running;

                                        // Check if current session has requested a stop
                                        if let (Some(gpui), Some(session_id)) = (cx.try_global::<Gpui>(), &current_session_id) {
                                            if gpui.session_stop_requests.lock().unwrap().contains(session_id) {
                                                cancel_enabled = false;
                                            }
                                        }

                                        let mut cancel_button = div()
                                            .size(px(40.))
                                            .rounded_sm()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor(if cancel_enabled {
                                                CursorStyle::PointingHand
                                            } else {
                                                CursorStyle::OperationNotAllowed
                                            })
                                            .child(file_icons::render_icon(
                                                &file_icons::get().get_type_icon(file_icons::STOP),
                                                22.0,
                                                if cancel_enabled {
                                                    cx.theme().danger
                                                } else {
                                                    cx.theme().muted_foreground
                                                },
                                                "⬜",
                                            ));

                                        if cancel_enabled {
                                            cancel_button = cancel_button
                                                .hover(|s| s.bg(cx.theme().muted))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(Self::on_stop_click),
                                                );
                                        }
                                        buttons.push(cancel_button);

                                        buttons
                                    }),
                                    ), // Close main input row
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
