use super::file_icons;
use crate::persistence::ChatMetadata;
use crate::session::instance::SessionActivityState;
use gpui::{
    div, prelude::*, px, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, MouseButton, MouseUpEvent, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window,
};
use gpui_component::scroll::ScrollbarAxis;
use gpui_component::{tooltip::Tooltip, ActiveTheme, Icon, StyledExt};
use std::time::SystemTime;
use tracing::debug;

/// Events emitted by individual ChatListItem components
#[derive(Clone, Debug)]
pub enum ChatListItemEvent {
    /// User clicked to select this chat session
    SessionClicked { session_id: String },
    /// User clicked to delete this chat session
    DeleteClicked { session_id: String },
}

/// Events emitted by the ChatSidebar component
#[derive(Clone, Debug)]
pub enum ChatSidebarEvent {
    /// User selected a specific chat session
    SessionSelected { session_id: String },
    /// User requested deletion of a chat session
    SessionDeleteRequested { session_id: String },
    /// User requested creation of a new chat session
    NewSessionRequested { name: Option<String> },
}

/// Individual chat list item component
pub struct ChatListItem {
    metadata: ChatMetadata,
    is_selected: bool,
    is_hovered: bool,
    activity_state: SessionActivityState,
    focus_handle: FocusHandle,
}

impl ChatListItem {
    pub fn new(metadata: ChatMetadata, is_selected: bool, cx: &mut Context<Self>) -> Self {
        Self {
            metadata,
            is_selected,
            is_hovered: false,
            activity_state: SessionActivityState::Idle,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn update_selection(&mut self, is_selected: bool, cx: &mut Context<Self>) {
        if self.is_selected != is_selected {
            self.is_selected = is_selected;
            cx.notify();
        }
    }

    pub fn update_metadata(&mut self, metadata: ChatMetadata, cx: &mut Context<Self>) {
        // Check if metadata has actually changed to avoid unnecessary updates
        if self.metadata != metadata {
            self.metadata = metadata;
            cx.notify();
        }
    }

    pub fn update_activity_state(
        &mut self,
        activity_state: SessionActivityState,
        cx: &mut Context<Self>,
    ) {
        if self.activity_state != activity_state {
            self.activity_state = activity_state;
            cx.notify();
        }
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

    fn on_hover(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if *hovered != self.is_hovered {
            self.is_hovered = *hovered;
            cx.notify();
        }
    }

    /// Handle session selection click - emits event to parent
    fn on_session_click(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ChatListItemEvent::SessionClicked {
            session_id: self.metadata.id.clone(),
        });
    }

    /// Handle session deletion click - emits event to parent
    fn on_session_delete(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ChatListItemEvent::DeleteClicked {
            session_id: self.metadata.id.clone(),
        });
    }
}

impl EventEmitter<ChatListItemEvent> for ChatListItem {}

impl Focusable for ChatListItem {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatListItem {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = if self.metadata.name.is_empty() {
            "Unnamed chat".to_string()
        } else {
            self.metadata.name.clone()
        };
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
            .on_hover(cx.listener(Self::on_hover))
            .hover(|s| {
                if !self.is_selected {
                    s.bg(cx.theme().muted.opacity(0.5))
                } else {
                    s
                }
            })
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_session_click))
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
                            // Show blue indicator for sessions with active agents
                            .when(
                                !matches!(self.activity_state, SessionActivityState::Idle),
                                |s| {
                                    let color = match &self.activity_state {
                                        SessionActivityState::AgentRunning => cx.theme().info,
                                        SessionActivityState::WaitingForResponse => {
                                            cx.theme().primary
                                        }
                                        SessionActivityState::RateLimited { .. } => {
                                            cx.theme().warning
                                        }
                                        SessionActivityState::Idle => cx.theme().muted, // Won't be reached due to when condition
                                    };
                                    s.child(div().size(px(8.)).rounded_full().bg(color))
                                },
                            )
                            .when(self.is_selected && self.is_hovered, |s| {
                                s.child(
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
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::on_session_delete),
                                        ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(formatted_date))
                    .when(
                        self.metadata.last_usage.input_tokens > 0
                            || self.metadata.last_usage.cache_read_input_tokens > 0,
                        |d| {
                            let mut token_elements = Vec::new();

                            // Tool syntax icon - show which syntax this session uses
                            let tool_icon_path = match self.metadata.tool_syntax {
                                crate::types::ToolSyntax::Native => "icons/braces.svg",
                                crate::types::ToolSyntax::Caret => "icons/chevron_up.svg",
                                crate::types::ToolSyntax::Xml => "icons/code-xml.svg",
                            };

                            let tooltip_text = match self.metadata.tool_syntax {
                                crate::types::ToolSyntax::Native => {
                                    "Native tool use via provider API"
                                }
                                crate::types::ToolSyntax::Caret => {
                                    "Custom tool use blocks with triple caret fence"
                                }
                                crate::types::ToolSyntax::Xml => {
                                    "Custom tool use blocks with pseudo-XML tags"
                                }
                            };

                            token_elements.push(
                                div()
                                    .id(SharedString::from(format!(
                                        "tool-syntax-{}",
                                        self.metadata.id
                                    )))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_color(cx.theme().muted_foreground)
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(tooltip_text).build(window, cx)
                                    })
                                    .child(
                                        Icon::default()
                                            .path(SharedString::from(tool_icon_path))
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .into_any(),
                            );

                            // Input tokens from last request with arrow_up icon
                            if self.metadata.last_usage.input_tokens > 0 {
                                token_elements.push(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            Icon::default()
                                                .path(SharedString::from("icons/arrow_up.svg"))
                                                .text_color(cx.theme().muted_foreground),
                                        )
                                        .child(SharedString::from(format!(
                                            "{}",
                                            self.metadata.last_usage.input_tokens
                                                + self.metadata.last_usage.cache_read_input_tokens
                                        )))
                                        .into_any(),
                                );
                            }

                            // Cache read tokens from last request with arrow_circle icon
                            if self.metadata.last_usage.cache_read_input_tokens > 0 {
                                token_elements.push(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            Icon::default()
                                                .path(SharedString::from("icons/arrow_circle.svg"))
                                                .text_color(cx.theme().muted_foreground),
                                        )
                                        .child(SharedString::from(format!(
                                            "{}",
                                            self.metadata.last_usage.cache_read_input_tokens
                                        )))
                                        .into_any(),
                                );
                            }

                            d.child(div().flex().gap_2().children(token_elements))
                        },
                    ),
            )
    }
}

/// Main chat sidebar component
pub struct ChatSidebar {
    items: Vec<Entity<ChatListItem>>,
    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    is_collapsed: bool,
    activity_states: std::collections::HashMap<String, SessionActivityState>,
    _item_subscriptions: Vec<Subscription>,
}

impl ChatSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            items: Vec::new(),
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            is_collapsed: false,
            activity_states: std::collections::HashMap::new(),
            _item_subscriptions: Vec::new(),
        }
    }

    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>, cx: &mut Context<Self>) {
        // Clear existing subscriptions
        self._item_subscriptions.clear();

        // Create a map of existing items by their metadata ID for quick lookup
        let mut existing_items: std::collections::HashMap<String, Entity<ChatListItem>> =
            std::collections::HashMap::new();

        // Extract existing items and map them by ID
        for item in self.items.drain(..) {
            let id = cx.read_entity(&item, |item, _| item.metadata.id.clone());
            existing_items.insert(id, item);
        }

        // Build new items vector, reusing existing items where possible
        self.items = sessions
            .into_iter()
            .map(|session| {
                let item = if let Some(existing_item) = existing_items.remove(&session.id) {
                    // Reuse existing item but update its metadata and activity state
                    cx.update_entity(&existing_item, |item, cx| {
                        item.update_metadata(session.clone(), cx);
                        // Update activity state if we have it
                        if let Some(activity_state) = self.activity_states.get(&session.id) {
                            item.update_activity_state(activity_state.clone(), cx);
                        }
                    });
                    existing_item
                } else {
                    // Create new item
                    let new_item = cx.new(|cx| ChatListItem::new(session.clone(), false, cx));
                    // Set activity state if we have it
                    if let Some(activity_state) = self.activity_states.get(&session.id) {
                        cx.update_entity(&new_item, |item, cx| {
                            item.update_activity_state(activity_state.clone(), cx);
                        });
                    }
                    new_item
                };

                // Subscribe to item events
                self._item_subscriptions
                    .push(cx.subscribe(&item, Self::on_chat_list_item_event));

                item
            })
            .collect();

        cx.notify();
    }

    pub fn set_selected_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.selected_session_id = session_id;
        //cx.notify();
        if let Some(session_id) = self.selected_session_id.clone() {
            self.items.iter().for_each(|entity| {
                cx.update_entity(entity, |item, cx| {
                    item.update_selection(session_id == item.metadata.id, cx)
                })
            });
        } else {
            self.items.iter().for_each(|entity| {
                cx.update_entity(entity, |item, cx| item.update_selection(false, cx))
            });
        }
    }

    pub fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.is_collapsed = !self.is_collapsed;
        cx.notify();
    }

    pub fn update_single_session_activity_state(
        &mut self,
        session_id: String,
        activity_state: SessionActivityState,
        cx: &mut Context<Self>,
    ) {
        // Update our local state
        self.activity_states
            .insert(session_id.clone(), activity_state.clone());

        // Find and update the specific item
        for item_entity in &self.items {
            cx.update_entity(item_entity, |item, cx| {
                if item.metadata.id == session_id {
                    item.update_activity_state(activity_state.clone(), cx);
                }
            });
        }

        cx.notify();
    }

    fn on_new_chat_click(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        debug!("New chat button clicked");
        // Emit event to parent to create a new chat session
        cx.emit(ChatSidebarEvent::NewSessionRequested { name: None });
    }

    /// Handle events from ChatListItem components
    fn on_chat_list_item_event(
        &mut self,
        _item: Entity<ChatListItem>,
        event: &ChatListItemEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChatListItemEvent::SessionClicked { session_id } => {
                cx.emit(ChatSidebarEvent::SessionSelected {
                    session_id: session_id.clone(),
                });
            }
            ChatListItemEvent::DeleteClicked { session_id } => {
                cx.emit(ChatSidebarEvent::SessionDeleteRequested {
                    session_id: session_id.clone(),
                });
            }
        }
    }
}

impl EventEmitter<ChatSidebarEvent> for ChatSidebar {}

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
                    // Chat list area - outer container with padding
                    div().flex_1().min_h(px(0.)).child(
                        div()
                            .id("chat-items")
                            .p_2()
                            .h_full()
                            .scrollable(ScrollbarAxis::Vertical)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(self.items.clone())
                            .when(self.items.is_empty(), |s| {
                                s.child(
                                    div()
                                        .px_1()
                                        .py_4()
                                        .text_center()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No chats yet"),
                                )
                            }),
                    ),
                )
        }
    }
}
