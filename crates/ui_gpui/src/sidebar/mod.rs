mod session_item;

pub use session_item::{SessionListItem, SessionListItemEvent};

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use gpui::{
    div, prelude::*, px, rems, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, SharedString, StatefulInteractiveElement, Styled, Subscription,
};
use gpui_component::scroll::ScrollableElement;

use gpui_component::{tooltip::Tooltip, ActiveTheme, Icon, Sizable, Size, StyledExt};
use std::collections::HashMap;
use tracing::debug;

/// Maximum number of sessions shown per project before "Show more" appears
const DEFAULT_VISIBLE_LIMIT: usize = 5;

// ─── ProjectGroup ────────────────────────────────────────────────────────────

/// Tracks the UI state for one project group in the sidebar.
struct ProjectGroup {
    /// Project name (derived from session metadata)
    name: String,
    /// Session entities in this group, sorted by updated_at desc
    items: Vec<Entity<SessionListItem>>,
    /// Whether this group is expanded
    is_expanded: bool,
    /// Whether "Show more" has been clicked (show all items)
    show_all: bool,
    /// Whether the project header is hovered
    is_hovered: bool,
}

impl ProjectGroup {
    fn visible_items(&self) -> &[Entity<SessionListItem>] {
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

// ─── SessionSidebar ─────────────────────────────────────────────────────────────

/// Events emitted by the SessionSidebar component
#[derive(Clone, Debug)]
pub enum SessionSidebarEvent {
    /// User selected a specific chat session
    SessionSelected { session_id: String },
    /// User requested deletion of a chat session
    SessionDeleteRequested { session_id: String },
    /// User requested creation of a new chat session in a specific project
    NewSessionRequested {
        name: Option<String>,
        initial_project: Option<String>,
    },
    /// User clicked the "+" button in the sidebar header to add a new project
    AddProjectRequested,
    /// User clicked the "pin" icon on a temporary project header to persist it
    PersistProjectRequested { project_name: String },
}

/// Main project sidebar component — groups sessions by project.
pub struct SessionSidebar {
    /// Project groups, sorted by most-recently-updated session
    groups: Vec<ProjectGroup>,
    /// Preserved UI state per project: (is_expanded, show_all)
    group_ui_state: HashMap<String, (bool, bool)>,
    /// Project names that are persisted in projects.json.
    /// Projects not in this set are "temporary" and get a pin icon.
    persisted_projects: std::collections::HashSet<String>,

    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    activity_states: HashMap<String, SessionActivityState>,
    _item_subscriptions: Vec<Subscription>,
}

impl SessionSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            groups: Vec::new(),
            group_ui_state: HashMap::new(),
            persisted_projects: std::collections::HashSet::new(),

            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            activity_states: HashMap::new(),
            _item_subscriptions: Vec::new(),
        }
    }

    /// Rebuild all groups from a flat list of session metadata.
    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>, cx: &mut Context<Self>) {
        self._item_subscriptions.clear();

        // Collect existing item entities for reuse (keyed by session id)
        let mut existing_items: HashMap<String, Entity<SessionListItem>> = HashMap::new();
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

        // Ensure persisted projects appear even when they have zero sessions
        for project_name in &self.persisted_projects {
            project_sessions.entry(project_name.clone()).or_default();
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
            let mut items: Vec<Entity<SessionListItem>> = Vec::new();

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
                    let new_item =
                        cx.new(|cx| SessionListItem::new(session.clone(), is_selected, cx));
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

    pub fn set_persisted_projects(&mut self, projects: std::collections::HashSet<String>) {
        self.persisted_projects = projects;
    }

    fn on_add_project_click(
        &mut self,
        _: &ClickEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        debug!("Add project button clicked");
        cx.emit(SessionSidebarEvent::AddProjectRequested);
    }

    #[allow(dead_code)]
    pub fn request_new_session(&mut self, cx: &mut Context<Self>) {
        debug!("Requesting new chat session");
        cx.emit(SessionSidebarEvent::NewSessionRequested {
            name: None,
            initial_project: None,
        });
    }

    fn on_chat_list_item_event(
        &mut self,
        _item: Entity<SessionListItem>,
        event: &SessionListItemEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionListItemEvent::SessionClicked { session_id } => {
                cx.emit(SessionSidebarEvent::SessionSelected {
                    session_id: session_id.clone(),
                });
            }
            SessionListItemEvent::DeleteClicked { session_id } => {
                cx.emit(SessionSidebarEvent::SessionDeleteRequested {
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
        let is_temporary =
            !self.persisted_projects.contains(&project_name) && project_name != "(no project)";

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
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(g) = this.groups.get_mut(group_idx) {
                    g.is_expanded = !g.is_expanded;
                    this.group_ui_state
                        .insert(g.name.clone(), (g.is_expanded, g.show_all));
                    cx.notify();
                }
            }))
            // Folder icon
            .child(
                gpui::svg()
                    .flex_none()
                    .size(rems(0.875))
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
            // Pin button for temporary projects (persist to projects.json)
            .when(is_temporary, |el| {
                let project_for_pin = group.name.clone();
                el.child(
                    div()
                        .id(SharedString::from(format!("pin-project-{}", group_idx)))
                        .flex_none()
                        .size(rems(1.25))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .when(is_hovered, |el| el.hover(|s| s.bg(cx.theme().muted)))
                        .tooltip(move |window, cx| {
                            Tooltip::new(
                                "Temporary project — save to make it a first-class project \
                                 that can be referenced by tool calls in other sessions",
                            )
                            .build(window, cx)
                        })
                        .child(
                            gpui::svg()
                                .size(rems(0.75))
                                .path("icons/pin.svg")
                                .text_color(if is_hovered {
                                    cx.theme().muted_foreground
                                } else {
                                    cx.theme().transparent
                                }),
                        )
                        .on_click(cx.listener(move |_this, _, _, cx| {
                            cx.stop_propagation();
                            debug!("Persist project: {}", project_for_pin);
                            cx.emit(SessionSidebarEvent::PersistProjectRequested {
                                project_name: project_for_pin.clone(),
                            });
                        })),
                )
            })
            // New session button (always present but invisible when not hovered,
            // so it doesn't change layout)
            .child(
                div()
                    .id(SharedString::from(format!(
                        "new-session-project-{}",
                        group_idx
                    )))
                    .flex_none()
                    .size(rems(1.25))
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
                    .child(
                        gpui::svg()
                            .size(rems(0.75))
                            .path("icons/plus.svg")
                            .text_color(if is_hovered {
                                cx.theme().primary
                            } else {
                                cx.theme().transparent
                            }),
                    )
                    .on_click({
                        let project = group.name.clone();
                        cx.listener(move |_this, _, _, cx| {
                            cx.stop_propagation();
                            debug!("New session in project: {}", project);
                            cx.emit(SessionSidebarEvent::NewSessionRequested {
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
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(g) = this.groups.get_mut(group_idx) {
                    g.show_all = true;
                    this.group_ui_state
                        .insert(g.name.clone(), (g.is_expanded, g.show_all));
                    cx.notify();
                }
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(SharedString::from(format!("Show {} more", hidden_count))),
            )
    }
}

impl EventEmitter<SessionSidebarEvent> for SessionSidebar {}

impl Focusable for SessionSidebar {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionSidebar {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let scale = cx.theme().font_size / px(16.);

        // Full sidebar view
        div()
            .id("chat-sidebar")
            .flex_none()
            .w(px(scale * 260.))
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
                    .px(px(20.))
                    .py_3()
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
                            .child("Projects"),
                    )
                    .child(
                        div()
                            .id("add-project-btn")
                            .size(rems(1.5))
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
                            .on_click(cx.listener(Self::on_add_project_click)),
                    ),
            )
            // Scrollable list
            .child(
                div().flex_1().min_h(px(0.)).w_full().child(
                    div()
                        .id("chat-items")
                        .px(px(12.))
                        .w_full()
                        .h_full()
                        .overflow_y_scrollbar()
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
                                    .child("No projects yet"),
                            )
                        }),
                ),
            )
    }
}
