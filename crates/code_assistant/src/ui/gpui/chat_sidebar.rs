use super::file_icons;
use crate::persistence::ChatMetadata;
use crate::session::instance::SessionActivityState;
use gpui::{
    div, prelude::*, px, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, MouseButton, MouseUpEvent, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window,
};
use gpui_component::scroll::ScrollbarAxis;

use gpui_component::{tooltip::Tooltip, ActiveTheme, Icon, Sizable, Size, StyledExt};
use std::collections::HashMap;
use std::time::SystemTime;
use tracing::debug;

/// Maximum number of sessions shown per project before "Show more" appears
const DEFAULT_VISIBLE_LIMIT: usize = 5;

// ─── ChatListItem ────────────────────────────────────────────────────────────

/// Events emitted by individual ChatListItem components
#[derive(Clone, Debug)]
pub enum ChatListItemEvent {
    /// User clicked to select this chat session
    SessionClicked { session_id: String },
    /// User clicked to delete this chat session
    DeleteClicked { session_id: String },
}

/// Individual chat list item component — simplified to title + date.
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

    fn format_relative_date(timestamp: SystemTime) -> String {
        match timestamp.elapsed() {
            Ok(duration) => {
                let secs = duration.as_secs();
                if secs < 60 {
                    "Just now".to_string()
                } else if secs < 3600 {
                    format!("{}m", secs / 60)
                } else if secs < 86400 {
                    format!("{}h", secs / 3600)
                } else if secs < 86400 * 30 {
                    format!("{}d", secs / 86400)
                } else if secs < 86400 * 365 {
                    format!("{}mo", secs / (86400 * 30))
                } else {
                    format!("{}y", secs / (86400 * 365))
                }
            }
            Err(_) => "?".to_string(),
        }
    }

    fn on_hover(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if *hovered != self.is_hovered {
            self.is_hovered = *hovered;
            cx.notify();
        }
    }

    fn on_session_click(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ChatListItemEvent::SessionClicked {
            session_id: self.metadata.id.clone(),
        });
    }

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
        let date = Self::format_relative_date(self.metadata.updated_at);

        let is_active = !matches!(self.activity_state, SessionActivityState::Idle);
        let activity_color = match &self.activity_state {
            SessionActivityState::AgentRunning => cx.theme().info,
            SessionActivityState::WaitingForResponse => cx.theme().primary,
            SessionActivityState::RateLimited { .. } => cx.theme().warning,
            SessionActivityState::Idle => cx.theme().muted,
        };

        // Indent to align with project name text:
        // px_2(8) + folder(14) + gap(4) = 26px
        let left_indent = px(26.);

        // Fixed-width right column so trash + date don't shift around
        let date_col_width = px(50.);

        div()
            .id(SharedString::from(format!(
                "chat-item-{}",
                self.metadata.id
            )))
            .mx(px(2.))
            .pl(left_indent - px(2.)) // compensate mx so text stays aligned
            .pr_1()
            .py(px(5.))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .rounded_md()
            .border_1()
            .border_color(if self.is_selected {
                cx.theme().primary.opacity(0.3)
            } else {
                cx.theme().transparent
            })
            .bg(if self.is_selected {
                cx.theme().primary.opacity(0.1)
            } else {
                cx.theme().transparent
            })
            .on_hover(cx.listener(Self::on_hover))
            .when(!self.is_selected, |el| {
                el.hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
            })
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_session_click))
            // Activity dot
            .when(is_active, |el| {
                el.child(
                    div()
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(activity_color),
                )
            })
            // Session name (truncated — shrinks when trash icon appears)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(if self.is_selected {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .font_medium()
                    .child(SharedString::from(name)),
            )
            // Right column: fixed width, contains trash icon + date
            .child(
                div()
                    .flex_none()
                    .w(date_col_width)
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    // Trash icon (red, shown on hover)
                    .when(self.is_hovered, |el| {
                        el.child(
                            div()
                                .id(SharedString::from(format!("delete-{}", self.metadata.id)))
                                .flex_none()
                                .size(px(18.))
                                .rounded_sm()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().danger.opacity(0.15)))
                                .child(
                                    gpui::svg()
                                        .size(px(12.))
                                        .path("icons/trash.svg")
                                        .text_color(cx.theme().danger),
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::on_session_delete),
                                ),
                        )
                    })
                    // Date
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(SharedString::from(date)),
                    ),
            )
    }
}

// ─── ProjectGroup ────────────────────────────────────────────────────────────

/// Tracks the UI state for one project group in the sidebar.
struct ProjectGroup {
    /// Project name (derived from session metadata)
    name: String,
    /// Session entities in this group, sorted by updated_at desc
    items: Vec<Entity<ChatListItem>>,
    /// Whether this group is expanded
    is_expanded: bool,
    /// Whether "Show more" has been clicked (show all items)
    show_all: bool,
    /// Whether the project header is hovered
    is_hovered: bool,
}

impl ProjectGroup {
    fn visible_items(&self) -> &[Entity<ChatListItem>] {
        if self.show_all || self.items.len() <= DEFAULT_VISIBLE_LIMIT {
            &self.items
        } else {
            &self.items[..DEFAULT_VISIBLE_LIMIT]
        }
    }

    fn has_more(&self) -> bool {
        !self.show_all && self.items.len() > DEFAULT_VISIBLE_LIMIT
    }

    fn hidden_count(&self) -> usize {
        if self.has_more() {
            self.items.len() - DEFAULT_VISIBLE_LIMIT
        } else {
            0
        }
    }
}

// ─── ChatSidebar ─────────────────────────────────────────────────────────────

/// Events emitted by the ChatSidebar component
#[derive(Clone, Debug)]
pub enum ChatSidebarEvent {
    /// User selected a specific chat session
    SessionSelected { session_id: String },
    /// User requested deletion of a chat session
    SessionDeleteRequested { session_id: String },
    /// User requested creation of a new chat session in a specific project
    NewSessionRequested {
        name: Option<String>,
        initial_project: Option<String>,
    },
}

/// Main chat sidebar component — groups sessions by project.
pub struct ChatSidebar {
    /// Project groups, sorted by most-recently-updated session
    groups: Vec<ProjectGroup>,
    /// Preserved UI state per project: (is_expanded, show_all)
    group_ui_state: HashMap<String, (bool, bool)>,
    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    is_collapsed: bool,
    activity_states: HashMap<String, SessionActivityState>,
    _item_subscriptions: Vec<Subscription>,
}

impl ChatSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            groups: Vec::new(),
            group_ui_state: HashMap::new(),
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            is_collapsed: false,
            activity_states: HashMap::new(),
            _item_subscriptions: Vec::new(),
        }
    }

    /// Rebuild all groups from a flat list of session metadata.
    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>, cx: &mut Context<Self>) {
        self._item_subscriptions.clear();

        // Collect existing item entities for reuse (keyed by session id)
        let mut existing_items: HashMap<String, Entity<ChatListItem>> = HashMap::new();
        for group in self.groups.drain(..) {
            for item in group.items {
                let id = cx.read_entity(&item, |item, _| item.metadata.id.clone());
                existing_items.insert(id, item);
            }
        }

        // Group sessions by project
        let mut project_sessions: HashMap<String, Vec<ChatMetadata>> = HashMap::new();
        for session in sessions {
            let project = if session.initial_project.is_empty() {
                "(no project)".to_string()
            } else {
                session.initial_project.clone()
            };
            project_sessions.entry(project).or_default().push(session);
        }

        // Sort projects by the most recent session's updated_at
        let mut project_order: Vec<(String, Vec<ChatMetadata>)> =
            project_sessions.into_iter().collect();
        project_order.sort_by(|a, b| {
            let a_latest = a.1.iter().map(|s| s.updated_at).max();
            let b_latest = b.1.iter().map(|s| s.updated_at).max();
            b_latest.cmp(&a_latest)
        });

        // Build groups
        let mut new_groups: Vec<ProjectGroup> = Vec::new();

        for (project_name, sessions) in project_order {
            let mut items: Vec<Entity<ChatListItem>> = Vec::new();

            for session in &sessions {
                let entity = if let Some(existing) = existing_items.remove(&session.id) {
                    existing.update(cx, |item, cx| {
                        item.update_metadata(session.clone(), cx);
                        if let Some(state) = self.activity_states.get(&session.id) {
                            item.update_activity_state(state.clone(), cx);
                        }
                    });
                    existing
                } else {
                    let is_selected = self.selected_session_id.as_deref() == Some(&session.id);
                    let new_item = cx.new(|cx| ChatListItem::new(session.clone(), is_selected, cx));
                    if let Some(state) = self.activity_states.get(&session.id) {
                        new_item.update(cx, |item, cx| {
                            item.update_activity_state(state.clone(), cx);
                        });
                    }
                    new_item
                };

                self._item_subscriptions
                    .push(cx.subscribe(&entity, Self::on_chat_list_item_event));
                items.push(entity);
            }

            // Inherit UI state from the old group with the same name
            let (is_expanded, show_all) = self
                .group_ui_state
                .get(&project_name)
                .copied()
                .unwrap_or((true, false));

            new_groups.push(ProjectGroup {
                name: project_name,
                items,
                is_expanded,
                show_all,
                is_hovered: false,
            });
        }

        // Save UI state for next rebuild
        self.group_ui_state = new_groups
            .iter()
            .map(|g| (g.name.clone(), (g.is_expanded, g.show_all)))
            .collect();

        self.groups = new_groups;
        cx.notify();
    }

    pub fn set_selected_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.selected_session_id = session_id.clone();
        for group in &self.groups {
            for item in &group.items {
                item.update(cx, |item, cx| {
                    let selected = session_id.as_deref() == Some(&item.metadata.id);
                    item.update_selection(selected, cx);
                });
            }
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
        self.activity_states
            .insert(session_id.clone(), activity_state.clone());

        for group in &self.groups {
            for item_entity in &group.items {
                cx.update_entity(item_entity, |item, cx| {
                    if item.metadata.id == session_id {
                        item.update_activity_state(activity_state.clone(), cx);
                    }
                });
            }
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
        cx.emit(ChatSidebarEvent::NewSessionRequested {
            name: None,
            initial_project: None,
        });
    }

    #[allow(dead_code)]
    pub fn request_new_session(&mut self, cx: &mut Context<Self>) {
        debug!("Requesting new chat session");
        cx.emit(ChatSidebarEvent::NewSessionRequested {
            name: None,
            initial_project: None,
        });
    }

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

    // ── rendering helpers ────────────────────────────────────────────────

    fn render_project_header(
        &self,
        group_idx: usize,
        group: &ProjectGroup,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_name = group.name.clone();
        let is_expanded = group.is_expanded;
        let is_hovered = group.is_hovered;
        let project_for_new = project_name.clone();

        let folder_icon = if is_expanded {
            "icons/file_icons/folder_open.svg"
        } else {
            "icons/file_icons/folder.svg"
        };

        // Fixed height so the plus icon appearing on hover doesn't shift layout
        let header_height = px(28.);

        div()
            .id(SharedString::from(format!("project-hdr-{}", group_idx)))
            .w_full()
            .px_2()
            .h(header_height)
            .mt(if group_idx > 0 { px(6.) } else { px(0.) })
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .rounded_sm()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if let Some(g) = this.groups.get_mut(group_idx) {
                    if g.is_hovered != *hovered {
                        g.is_hovered = *hovered;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if let Some(g) = this.groups.get_mut(group_idx) {
                        g.is_expanded = !g.is_expanded;
                        this.group_ui_state
                            .insert(g.name.clone(), (g.is_expanded, g.show_all));
                        cx.notify();
                    }
                }),
            )
            // Folder icon
            .child(
                gpui::svg()
                    .flex_none()
                    .size(px(14.))
                    .path(folder_icon)
                    .text_color(cx.theme().muted_foreground),
            )
            // Project name
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .font_medium()
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(project_name)),
            )
            // New session button (always present but invisible when not hovered,
            // so it doesn't change layout)
            .child(
                div()
                    .id(SharedString::from(format!(
                        "new-session-project-{}",
                        group_idx
                    )))
                    .flex_none()
                    .size(px(20.))
                    .rounded_sm()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(is_hovered, |el| el.hover(|s| s.bg(cx.theme().muted)))
                    .tooltip(move |window, cx| {
                        Tooltip::new(format!("New chat in {}", project_for_new.clone()))
                            .build(window, cx)
                    })
                    .child(gpui::svg().size(px(12.)).path("icons/plus.svg").text_color(
                        if is_hovered {
                            cx.theme().primary
                        } else {
                            cx.theme().transparent
                        },
                    ))
                    .on_mouse_up(MouseButton::Left, {
                        let project = group.name.clone();
                        cx.listener(move |_this, _, _, cx| {
                            debug!("New session in project: {}", project);
                            cx.emit(ChatSidebarEvent::NewSessionRequested {
                                name: None,
                                initial_project: Some(project.clone()),
                            });
                        })
                    }),
            )
    }

    fn render_show_more(
        &self,
        group_idx: usize,
        hidden_count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("show-more-{}", group_idx)))
            .w_full()
            .pl(px(26.))
            .pr_2()
            .py(px(4.))
            .cursor_pointer()
            .rounded_sm()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if let Some(g) = this.groups.get_mut(group_idx) {
                        g.show_all = true;
                        this.group_ui_state
                            .insert(g.name.clone(), (g.is_expanded, g.show_all));
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(SharedString::from(format!("Show {} more", hidden_count))),
            )
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
            return div()
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
                        .size(px(28.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().muted))
                        .child(
                            Icon::default()
                                .path(SharedString::from("icons/plus.svg"))
                                .with_size(Size::Small)
                                .text_color(cx.theme().muted_foreground),
                        )
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_new_chat_click)),
                );
        }

        // Build the list of project groups with their items
        let mut children: Vec<gpui::AnyElement> = Vec::new();

        for (idx, group) in self.groups.iter().enumerate() {
            // Project header
            children.push(
                self.render_project_header(idx, group, cx)
                    .into_any_element(),
            );

            // Items (if expanded)
            if group.is_expanded {
                for item in group.visible_items() {
                    children.push(item.clone().into_any_element());
                }
                // "Show more" link
                if group.has_more() {
                    children.push(
                        self.render_show_more(idx, group.hidden_count(), cx)
                            .into_any_element(),
                    );
                }
            }
        }

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
            // Header
            .child(
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
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_new_chat_click)),
                    ),
            )
            // Scrollable list
            .child(
                div().flex_1().min_h(px(0.)).w_full().child(
                    div()
                        .id("chat-items")
                        .py_1()
                        .w_full()
                        .h_full()
                        .scrollable(ScrollbarAxis::Vertical)
                        .flex()
                        .flex_col()
                        .children(children)
                        .when(self.groups.is_empty(), |s| {
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
