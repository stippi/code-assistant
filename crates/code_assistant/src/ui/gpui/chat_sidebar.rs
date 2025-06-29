use super::file_icons;
use super::UiEventSender;
use crate::persistence::ChatMetadata;
use crate::ui::ui_events::UiEvent;
use gpui::{
    actions, div, prelude::*, px, AnyElement, App, AppContext, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, MouseButton, MouseUpEvent, SharedString, Styled, Window,
};
use gpui_component::{popup_menu::PopupMenuExt, ActiveTheme, Selectable, StyledExt};
use std::time::SystemTime;
use tracing::{debug, trace, warn};

/// Individual chat list item component
pub struct ChatListItem {
    metadata: ChatMetadata,
    is_selected: bool,
    is_hovered: bool,
    focus_handle: FocusHandle,
}

impl ChatListItem {
    pub fn new(metadata: ChatMetadata, is_selected: bool, cx: &mut Context<Self>) -> Self {
        Self {
            metadata,
            is_selected,
            is_hovered: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn update_selection(&mut self, is_selected: bool, cx: &mut Context<Self>) {
        if self.is_selected != is_selected {
            self.is_selected = is_selected;
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
            .on_hover(cx.listener(Self::on_hover))
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(self.is_selected, |s| {
                                s.child(div().size(px(6.)).rounded_full().bg(cx.theme().primary))
                            }) /*
                            .when(self.is_hovered, |s| {
                                s.child(ItemMenu::new("popup-menu").popup_menu(
                                    move |this, _window, _cx| {
                                        this.menu("Rename", Box::new(Rename))
                                            .separator()
                                            .menu("Delete", Box::new(Delete))
                                    },
                                ))
                            }),
                            */
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
                                        .on_mouse_up(MouseButton::Left, {
                                            let session_id_for_delete = session_id.clone();
                                            move |_, _window, cx| {
                                                // Emit delete event
                                                if let Some(sender) =
                                                    cx.try_global::<UiEventSender>()
                                                {
                                                    let _ = sender.0.try_send(
                                                        UiEvent::DeleteChatSession {
                                                            session_id: session_id_for_delete
                                                                .clone(),
                                                        },
                                                    );
                                                }
                                            }
                                        }),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(formatted_date)),
            )
            .when(
                self.metadata.total_usage.input_tokens > 0
                    || self.metadata.total_usage.output_tokens > 0,
                |s| {
                    let mut token_elements = Vec::new();

                    // Input tokens (blue)
                    token_elements.push(div().text_color(cx.theme().info).child(
                        SharedString::from(format!("{}", self.metadata.total_usage.input_tokens)),
                    ));

                    // Cache reads (cyan) - only if > 0
                    if self.metadata.total_usage.cache_read_input_tokens > 0 {
                        token_elements.push(div().text_color(cx.theme().info.opacity(0.7)).child(
                            SharedString::from(format!(
                                "{}",
                                self.metadata.total_usage.cache_read_input_tokens
                            )),
                        ));
                    }

                    // Output tokens (green)
                    token_elements.push(div().text_color(cx.theme().success).child(
                        SharedString::from(format!("{}", self.metadata.total_usage.output_tokens)),
                    ));

                    // Context size (yellow) - only if > 0
                    if self.metadata.current_context_size > 0 {
                        token_elements.push(div().text_color(cx.theme().warning).child(
                            SharedString::from(format!("/{}", self.metadata.current_context_size)),
                        ));
                    }

                    s.child(div().flex().gap_1().text_xs().children(token_elements))
                },
            )
    }
}

#[derive(IntoElement)]
pub struct ItemMenu {
    pub base: Div,
    id: ElementId,
    selected: bool,
}

impl ItemMenu {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().flex_shrink_0(),
            id: id.into(),
            selected: false,
        }
    }
}

impl Selectable for ItemMenu {
    fn element_id(&self) -> &ElementId {
        &self.id
    }

    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl From<ItemMenu> for AnyElement {
    fn from(menu: ItemMenu) -> Self {
        menu.into_any_element()
    }
}

impl Styled for ItemMenu {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.base.style()
    }
}

impl PopupMenuExt for ItemMenu {}

impl RenderOnce for ItemMenu {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .size(px(20.))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.8)))
            .child(file_icons::render_icon(
                &file_icons::get().get_type_icon("menu"),
                12.0,
                cx.theme().muted_foreground,
                "...",
            ))
    }
}

actions!(chat_sidebar, [Rename, Delete]);

/// Main chat sidebar component
pub struct ChatSidebar {
    items: Vec<Entity<ChatListItem>>,
    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    is_collapsed: bool,
}

impl ChatSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            items: Vec::new(),
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            is_collapsed: false,
        }
    }

    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>, cx: &mut Context<Self>) {
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
                if let Some(existing_item) = existing_items.remove(&session.id) {
                    // Reuse existing item
                    existing_item
                } else {
                    // Create new item
                    cx.new(|cx| ChatListItem::new(session, false, cx))
                }
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

    fn on_new_chat_click(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        debug!("New chat button clicked");
        // Emit event to create a new chat session
        if let Some(sender) = cx.try_global::<UiEventSender>() {
            trace!("ChatSidebar: Sending CreateNewChatSession event");
            let _ = sender
                .0
                .try_send(UiEvent::CreateNewChatSession { name: None });
        } else {
            warn!("ChatSidebar: No UiEventSender global available");
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
                        .children(self.items.clone())
                        .when(self.items.is_empty(), |s| {
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
