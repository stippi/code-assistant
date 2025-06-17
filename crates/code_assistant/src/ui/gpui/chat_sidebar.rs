use super::file_icons;
use crate::persistence::ChatMetadata;
use crate::ui::gpui::{ui_events::UiEvent, UiEventSender};
use gpui::{
    div, prelude::*, px, Context, FocusHandle, Focusable, MouseButton, MouseUpEvent, SharedString,
};
use gpui_component::{ActiveTheme, StyledExt};
use std::time::SystemTime;

/// Individual chat list item component
pub struct ChatListItem {
    metadata: ChatMetadata,
    is_selected: bool,
    focus_handle: FocusHandle,
}

impl ChatListItem {
    pub fn new(metadata: ChatMetadata, is_selected: bool, cx: &mut Context<Self>) -> Self {
        Self {
            metadata,
            is_selected,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn update_selection(&mut self, is_selected: bool, cx: &mut Context<Self>) {
        self.is_selected = is_selected;
        cx.notify();
    }

    /// Format the creation date for display
    fn format_date(timestamp: SystemTime) -> String {
        // Simple date formatting - could be improved with chrono if needed
        match timestamp.elapsed() {
            Ok(duration) => {
                let secs = duration.as_secs();
                if secs < 60 {
                    "Just now".to_string()
                } else if secs < 3600 {
                    format!("{}m ago", secs / 60)
                } else if secs < 86400 {
                    format!("{}h ago", secs / 3600)
                } else {
                    format!("{}d ago", secs / 86400)
                }
            }
            Err(_) => "Unknown".to_string(),
        }
    }
}

impl Focusable for ChatListItem {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatListItem {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.metadata.id.clone();
        let name = self.metadata.name.clone();
        let formatted_date = Self::format_date(self.metadata.created_at);

        div()
            .id(SharedString::from(format!(
                "chat-item-{}",
                self.metadata.id
            )))
            .w_full()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .rounded_md()
            .bg(if self.is_selected {
                cx.theme().primary.opacity(0.1)
            } else {
                cx.theme().transparent
            })
            .border_1()
            .border_color(if self.is_selected {
                cx.theme().primary.opacity(0.3)
            } else {
                cx.theme().transparent
            })
            .hover(|s| {
                if !self.is_selected {
                    s.bg(cx.theme().muted.opacity(0.5))
                } else {
                    s
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let session_id = session_id.clone();
                move |_, _window, cx| {
                    // Emit event to load this chat session
                    if let Some(sender) = cx.try_global::<UiEventSender>() {
                        let _ = sender.0.try_send(UiEvent::LoadChatSession {
                            session_id: session_id.clone(),
                        });
                    }
                }
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(name)),
                    )
                    .when(self.is_selected, |s| {
                        s.child(div().size(px(6.)).rounded_full().bg(cx.theme().primary))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(formatted_date)),
            )
    }
}

/// Main chat sidebar component
pub struct ChatSidebar {
    sessions: Vec<ChatMetadata>,
    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    is_collapsed: bool,
}

impl ChatSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            sessions: Vec::new(),
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            is_collapsed: false,
        }
    }

    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>, cx: &mut Context<Self>) {
        self.sessions = sessions;
        cx.notify();
    }

    pub fn set_selected_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.selected_session_id = session_id;
        cx.notify();
    }

    pub fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.is_collapsed = !self.is_collapsed;
        cx.notify();
    }

    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    fn on_new_chat_click(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        tracing::info!("ChatSidebar: New chat button clicked");
        // Emit event to create a new chat session
        if let Some(sender) = cx.try_global::<UiEventSender>() {
            tracing::info!("ChatSidebar: Sending CreateNewChatSession event");
            let _ = sender
                .0
                .try_send(UiEvent::CreateNewChatSession { name: None });
        } else {
            tracing::warn!("ChatSidebar: No UiEventSender global available");
        }
    }
}

impl Focusable for ChatSidebar {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatSidebar {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_collapsed {
            // Collapsed view - narrow bar with toggle button
            div()
                .id("collapsed-chat-sidebar")
                .flex_none()
                .w(px(40.))
                .h_full()
                .bg(cx.theme().sidebar)
                .border_r_1()
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
                            &file_icons::get().get_type_icon(file_icons::MESSAGE_BUBBLES),
                            16.0,
                            cx.theme().muted_foreground,
                            "💬",
                        )),
                )
        } else {
            // Full sidebar view
            div()
                .id("chat-sidebar")
                .flex_none()
                .w(px(260.))
                .h_full()
                .bg(cx.theme().sidebar)
                .border_r_1()
                .border_color(cx.theme().sidebar_border)
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    // Header with title and new chat button
                    div()
                        .flex_none()
                        .p_3()
                        .border_b_1()
                        .border_color(cx.theme().sidebar_border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(cx.theme().foreground)
                                .child("Chats"),
                        )
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
                                    &file_icons::get().get_type_icon(file_icons::PLUS),
                                    14.0,
                                    cx.theme().muted_foreground,
                                    "+",
                                ))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::on_new_chat_click),
                                ),
                        ),
                )
                .child(
                    // Chat list area
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .children(
                            self.sessions
                                .iter()
                                .map(|session| {
                                    let is_selected =
                                        self.selected_session_id.as_ref() == Some(&session.id);

                                    // Create a simple div for each chat item instead of a component
                                    // to avoid complex entity management
                                    let session_id = session.id.clone();
                                    let name = session.name.clone();
                                    let formatted_date =
                                        ChatListItem::format_date(session.created_at);

                                    div()
                                        .id(SharedString::from(format!("chat-item-{}", session.id)))
                                        .w_full()
                                        .px_3()
                                        .py_2()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .cursor_pointer()
                                        .rounded_md()
                                        .bg(if is_selected {
                                            cx.theme().primary.opacity(0.1)
                                        } else {
                                            cx.theme().transparent
                                        })
                                        .border_1()
                                        .border_color(if is_selected {
                                            cx.theme().primary.opacity(0.3)
                                        } else {
                                            cx.theme().transparent
                                        })
                                        .hover(|s| {
                                            if !is_selected {
                                                s.bg(cx.theme().muted.opacity(0.5))
                                            } else {
                                                s
                                            }
                                        })
                                        .on_mouse_up(MouseButton::Left, {
                                            let session_id = session_id.clone();
                                            move |_, _window, cx| {
                                                // Emit event to load this chat session
                                                if let Some(sender) =
                                                    cx.try_global::<UiEventSender>()
                                                {
                                                    let _ = sender.0.try_send(
                                                        UiEvent::LoadChatSession {
                                                            session_id: session_id.clone(),
                                                        },
                                                    );
                                                }
                                            }
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .font_medium()
                                                        .text_color(cx.theme().foreground)
                                                        .child(SharedString::from(name)),
                                                )
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .when(is_selected, |s| {
                                                            s.child(
                                                                div()
                                                                    .size(px(6.))
                                                                    .rounded_full()
                                                                    .bg(cx.theme().primary),
                                                            )
                                                        })
                                                        .child(
                                                            // Delete button for chat options
                                                            div()
                                                                .size(px(20.))
                                                                .rounded_sm()
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .cursor_pointer()
                                                                .hover(|s| s.bg(cx.theme().danger.opacity(0.1)))
                                                                .child(file_icons::render_icon(
                                                                    &file_icons::get().get_type_icon("trash"),
                                                                    12.0,
                                                                    cx.theme().danger,
                                                                    "🗑",
                                                                ))
                                                                .on_mouse_up(MouseButton::Left, {
                                                                    let session_id_for_delete = session_id.clone();
                                                                    move |_, _window, cx| {
                                                                        // Emit delete event
                                                                        if let Some(sender) = cx.try_global::<UiEventSender>() {
                                                                            let _ = sender.0.try_send(UiEvent::DeleteChatSession {
                                                                                session_id: session_id_for_delete.clone(),
                                                                            });
                                                                        }
                                                                    }
                                                                })
                                                        )
                                                ),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(SharedString::from(formatted_date)),
                                        )
                                })
                                .collect::<Vec<_>>(),
                        )
                        .when(self.sessions.is_empty(), |s| {
                            s.child(
                                div()
                                    .px_3()
                                    .py_4()
                                    .text_center()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("No chats yet"),
                            )
                        }),
                )
        }
    }
}
