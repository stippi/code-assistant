//! The session sidebar: an inbox, not a file tree.
//!
//! The top of the sidebar lists every session that is not settled, across
//! projects, newest first. The order is static — activity does not move
//! rows; what a session needs from the user shows as emphasis (see
//! [`SessionListItem`]). Settled sessions wait in a collapsed shelf below.
//! The projects keep their place at the bottom as anchors: a project row
//! starts a new session there, and clicking it narrows the inbox and the
//! shelf to that project.

mod session_item;

pub use session_item::{SessionListItem, SessionListItemEvent};

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::lifecycle::SessionLifecycle;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::{
    AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, SharedString, StatefulInteractiveElement, Styled, Subscription, div,
    prelude::*, px, rems,
};

use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, StyledExt, tooltip::Tooltip};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// Display name of sessions without a project.
const NO_PROJECT: &str = "(no project)";

fn project_name(session: &ChatMetadata) -> String {
    if session.initial_project.is_empty() {
        NO_PROJECT.to_string()
    } else {
        session.initial_project.clone()
    }
}

/// One project in the anchors section at the bottom.
struct ProjectRow {
    name: String,
    /// Sessions of this project in the inbox.
    inbox_count: usize,
    is_hovered: bool,
}

/// Events emitted by the SessionSidebar component
#[derive(Clone, Debug)]
pub enum SessionSidebarEvent {
    /// User selected a specific chat session
    SessionSelected { session_id: String },
    /// User requested deletion of a chat session
    SessionDeleteRequested { session_id: String },
    /// User moved a session into the settled shelf
    SessionSettleRequested { session_id: String },
    /// User pulled a session back into the inbox
    SessionUnsettleRequested { session_id: String },
    /// User requested creation of a new chat session in a specific project
    NewSessionRequested {
        name: Option<String>,
        initial_project: Option<String>,
    },
    /// User clicked the "+" button in the projects header to add a new project
    AddProjectRequested,
    /// User clicked the "pin" icon on a temporary project row to persist it
    PersistProjectRequested { project_name: String },
}

pub struct SessionSidebar {
    sessions: Vec<ChatMetadata>,
    lifecycles: HashMap<String, SessionLifecycle>,
    /// Row entities by session id, reused across rebuilds.
    items: HashMap<String, Entity<SessionListItem>>,
    /// Unsettled sessions in display order (within the project scope).
    inbox: Vec<Entity<SessionListItem>>,
    /// Settled sessions in display order (within the project scope).
    settled: Vec<Entity<SessionListItem>>,
    /// Projects, most recently active first.
    projects: Vec<ProjectRow>,
    /// Project names that are persisted in projects.json.
    /// Projects not in this set are "temporary" and get a pin icon.
    persisted_projects: HashSet<String>,
    /// Narrows the inbox and the settled shelf to one project.
    project_scope: Option<String>,
    settled_expanded: bool,
    projects_expanded: bool,

    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    activity_states: HashMap<String, SessionActivityState>,
    awaiting_permission: HashSet<String>,
    _item_subscriptions: Vec<Subscription>,
}

impl SessionSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            sessions: Vec::new(),
            lifecycles: HashMap::new(),
            items: HashMap::new(),
            inbox: Vec::new(),
            settled: Vec::new(),
            projects: Vec::new(),
            persisted_projects: HashSet::new(),
            project_scope: None,
            settled_expanded: false,
            projects_expanded: true,
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            activity_states: HashMap::new(),
            awaiting_permission: HashSet::new(),
            _item_subscriptions: Vec::new(),
        }
    }

    /// Replace the session list and every lifecycle record.
    pub fn update_sessions(
        &mut self,
        sessions: Vec<ChatMetadata>,
        lifecycles: HashMap<String, SessionLifecycle>,
        cx: &mut Context<Self>,
    ) {
        if self.sessions == sessions && self.lifecycles == lifecycles {
            return;
        }
        self.sessions = sessions;
        self.lifecycles = lifecycles;
        self.sync_items(cx);
        self.relayout(cx);
    }

    /// One session's lifecycle changed (visited, settled, un-settled).
    pub fn update_session_lifecycle(
        &mut self,
        session_id: String,
        lifecycle: SessionLifecycle,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycles.get(&session_id) == Some(&lifecycle) {
            return;
        }
        if let Some(item) = self.items.get(&session_id) {
            let lifecycle = lifecycle.clone();
            item.update(cx, |item, cx| item.update_lifecycle(lifecycle, cx));
        }
        self.lifecycles.insert(session_id, lifecycle);
        // Only this row moved; the other rows' entities are untouched. A
        // startup sweep can settle hundreds of sessions in a row.
        self.relayout(cx);
    }

    /// Bring the row entities in line with the stored sessions, reusing
    /// existing ones.
    fn sync_items(&mut self, cx: &mut Context<Self>) {
        self._item_subscriptions.clear();
        let mut existing = std::mem::take(&mut self.items);

        let mut items = HashMap::new();
        for session in &self.sessions {
            let lifecycle = self
                .lifecycles
                .get(&session.id)
                .cloned()
                .unwrap_or_default();
            let activity = self.activity_states.get(&session.id).cloned();
            let awaiting = self.awaiting_permission.contains(&session.id);
            let entity = match existing.remove(&session.id) {
                Some(entity) => {
                    entity.update(cx, |item, cx| {
                        item.update_metadata(session.clone(), cx);
                        item.update_lifecycle(lifecycle, cx);
                        if let Some(state) = activity {
                            item.update_activity_state(state, cx);
                        }
                        item.set_awaiting_permission(awaiting, cx);
                    });
                    entity
                }
                None => {
                    let is_selected = self.selected_session_id.as_deref() == Some(&session.id);
                    let entity = cx.new(|cx| {
                        SessionListItem::new(session.clone(), lifecycle, is_selected, cx)
                    });
                    entity.update(cx, |item, cx| {
                        if let Some(state) = activity {
                            item.update_activity_state(state, cx);
                        }
                        item.set_awaiting_permission(awaiting, cx);
                    });
                    entity
                }
            };
            self._item_subscriptions
                .push(cx.subscribe(&entity, Self::on_chat_list_item_event));
            items.insert(session.id.clone(), entity);
        }
        self.items = items;
    }

    /// Recompute the inbox, the settled shelf and the project anchors from
    /// the stored sessions and lifecycles. Touches no row entity.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        // Projects: every project with a session plus the persisted ones,
        // most recently active first.
        let mut latest: HashMap<String, std::time::SystemTime> = HashMap::new();
        let mut inbox_counts: HashMap<String, usize> = HashMap::new();
        for session in &self.sessions {
            let project = project_name(session);
            latest
                .entry(project.clone())
                .and_modify(|at| *at = (*at).max(session.updated_at))
                .or_insert(session.updated_at);
            if !self.is_settled(&session.id) {
                *inbox_counts.entry(project).or_default() += 1;
            }
        }
        for project in &self.persisted_projects {
            latest
                .entry(project.clone())
                .or_insert(std::time::UNIX_EPOCH);
        }
        let hovered: HashSet<String> = self
            .projects
            .iter()
            .filter(|row| row.is_hovered)
            .map(|row| row.name.clone())
            .collect();
        let mut projects: Vec<(String, std::time::SystemTime)> = latest.into_iter().collect();
        projects.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.projects = projects
            .into_iter()
            .map(|(name, _)| ProjectRow {
                inbox_count: inbox_counts.get(&name).copied().unwrap_or(0),
                is_hovered: hovered.contains(&name),
                name,
            })
            .collect();
        if let Some(scope) = &self.project_scope
            && !self.projects.iter().any(|row| &row.name == scope)
        {
            self.project_scope = None;
        }

        // Inbox: static order, newest first; an un-settled session surfaces
        // at the top. Settled shelf: most recently settled first.
        let in_scope = |session: &ChatMetadata| match &self.project_scope {
            Some(scope) => &project_name(session) == scope,
            None => true,
        };
        let mut inbox: Vec<(std::time::SystemTime, &ChatMetadata)> = Vec::new();
        let mut settled: Vec<(std::time::SystemTime, &ChatMetadata)> = Vec::new();
        for session in self.sessions.iter().filter(|s| in_scope(s)) {
            let lifecycle = self.lifecycles.get(&session.id);
            match lifecycle.and_then(|l| l.settled) {
                Some(settlement) => settled.push((settlement.at, session)),
                None => {
                    let anchor = lifecycle
                        .map(|l| l.inbox_anchor(session.created_at))
                        .unwrap_or(session.created_at);
                    inbox.push((anchor, session));
                }
            }
        }
        inbox.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.id.cmp(&a.1.id)));
        settled.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.id.cmp(&a.1.id)));
        self.inbox = inbox
            .into_iter()
            .map(|(_, session)| self.items[&session.id].clone())
            .collect();
        self.settled = settled
            .into_iter()
            .map(|(_, session)| self.items[&session.id].clone())
            .collect();
        cx.notify();
    }

    fn is_settled(&self, session_id: &str) -> bool {
        self.lifecycles
            .get(session_id)
            .is_some_and(SessionLifecycle::is_settled)
    }

    pub fn set_selected_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.selected_session_id = session_id.clone();
        for item in self.items.values() {
            item.update(cx, |item, cx| {
                let selected = session_id.as_deref() == Some(&item.metadata.id);
                item.update_selection(selected, cx);
            });
        }
    }

    pub fn update_single_session_activity_state(
        &mut self,
        session_id: String,
        activity_state: SessionActivityState,
        cx: &mut Context<Self>,
    ) {
        if let Some(item) = self.items.get(&session_id) {
            item.update(cx, |item, cx| {
                item.update_activity_state(activity_state.clone(), cx);
            });
        }
        self.activity_states.insert(session_id, activity_state);
        cx.notify();
    }

    /// The sessions with an open permission request.
    pub fn set_awaiting_permission(&mut self, sessions: HashSet<String>, cx: &mut Context<Self>) {
        if self.awaiting_permission == sessions {
            return;
        }
        for (id, item) in &self.items {
            let awaiting = sessions.contains(id);
            item.update(cx, |item, cx| item.set_awaiting_permission(awaiting, cx));
        }
        self.awaiting_permission = sessions;
        cx.notify();
    }

    pub fn set_persisted_projects(&mut self, projects: HashSet<String>) {
        self.persisted_projects = projects;
    }

    /// The project a new session from the header button goes to: the
    /// scoped project, else the selected session's, else none.
    fn project_for_new_session(&self) -> Option<String> {
        if let Some(scope) = &self.project_scope {
            return (scope != NO_PROJECT).then(|| scope.clone());
        }
        let selected = self.selected_session_id.as_deref()?;
        self.sessions
            .iter()
            .find(|s| s.id == selected)
            .map(|s| s.initial_project.clone())
            .filter(|p| !p.is_empty())
    }

    fn on_add_project_click(
        &mut self,
        _: &ClickEvent,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        debug!("Add project button clicked");
        cx.emit(SessionSidebarEvent::AddProjectRequested);
    }

    fn on_new_session_click(
        &mut self,
        _: &ClickEvent,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let initial_project = self.project_for_new_session();
        debug!("New session requested in {:?}", initial_project);
        cx.emit(SessionSidebarEvent::NewSessionRequested {
            name: None,
            initial_project,
        });
    }

    fn on_chat_list_item_event(
        &mut self,
        _item: Entity<SessionListItem>,
        event: &SessionListItemEvent,
        cx: &mut Context<Self>,
    ) {
        let event = match event {
            SessionListItemEvent::SessionClicked { session_id } => {
                SessionSidebarEvent::SessionSelected {
                    session_id: session_id.clone(),
                }
            }
            SessionListItemEvent::DeleteClicked { session_id } => {
                SessionSidebarEvent::SessionDeleteRequested {
                    session_id: session_id.clone(),
                }
            }
            SessionListItemEvent::SettleClicked { session_id } => {
                SessionSidebarEvent::SessionSettleRequested {
                    session_id: session_id.clone(),
                }
            }
            SessionListItemEvent::UnsettleClicked { session_id } => {
                SessionSidebarEvent::SessionUnsettleRequested {
                    session_id: session_id.clone(),
                }
            }
        };
        cx.emit(event);
    }

    // ── rendering helpers ────────────────────────────────────────────────

    /// A collapsible section header with an optional trailing element.
    fn render_section_header(
        &self,
        id: &'static str,
        label: String,
        expanded: bool,
        on_toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        trailing: Option<gpui_kit::AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chevron = if expanded {
            "icons/chevron_down.svg"
        } else {
            "icons/chevron_right.svg"
        };
        div()
            .id(SharedString::from(id))
            .w_full()
            .px_2()
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .rounded_sm()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            .on_click(cx.listener(move |this, _, _, cx| {
                on_toggle(this, cx);
                cx.notify();
            }))
            .child(
                gpui_kit::svg()
                    .flex_none()
                    .size(rems(0.75))
                    .path(chevron)
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .font_medium()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(label)),
            )
            .children(trailing)
    }

    fn render_project_row(
        &self,
        row_idx: usize,
        row: &ProjectRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_name = row.name.clone();
        let is_hovered = row.is_hovered;
        let is_scoped = self.project_scope.as_deref() == Some(project_name.as_str());
        let is_temporary =
            !self.persisted_projects.contains(&project_name) && project_name != NO_PROJECT;
        let project_for_new = project_name.clone();

        div()
            .id(SharedString::from(format!("project-row-{}", row_idx)))
            .w_full()
            .px_2()
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .rounded_sm()
            .when(is_scoped, |el| el.bg(cx.theme().muted.opacity(0.5)))
            .when(!is_scoped, |el| {
                el.hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if let Some(row) = this.projects.get_mut(row_idx)
                    && row.is_hovered != *hovered
                {
                    row.is_hovered = *hovered;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                let Some(row) = this.projects.get(row_idx) else {
                    return;
                };
                let name = row.name.clone();
                this.project_scope = if this.project_scope.as_deref() == Some(name.as_str()) {
                    None
                } else {
                    Some(name)
                };
                this.relayout(cx);
            }))
            .child(
                gpui_kit::svg()
                    .flex_none()
                    .size(rems(0.875))
                    .path("icons/file_icons/folder.svg")
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .font_medium()
                    .text_color(if is_scoped {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(SharedString::from(project_name)),
            )
            .when(row.inbox_count > 0 && !is_hovered, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child(SharedString::from(row.inbox_count.to_string())),
                )
            })
            // Pin button for temporary projects (persist to projects.json)
            .when(is_temporary && is_hovered, |el| {
                let project_for_pin = row.name.clone();
                el.child(
                    div()
                        .id(SharedString::from(format!("pin-project-{}", row_idx)))
                        .flex_none()
                        .size(rems(1.25))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().muted))
                        .tooltip(move |window, cx| {
                            Tooltip::new(
                                "Temporary project — save to make it a first-class project \
                                 that can be referenced by tool calls in other sessions",
                            )
                            .build(window, cx)
                        })
                        .child(
                            gpui_kit::svg()
                                .size(rems(0.75))
                                .path("icons/pin.svg")
                                .text_color(cx.theme().muted_foreground),
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
            // New session in this project
            .when(is_hovered, |el| {
                el.child(
                    div()
                        .id(SharedString::from(format!(
                            "new-session-project-{}",
                            row_idx
                        )))
                        .flex_none()
                        .size(rems(1.25))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().muted))
                        .tooltip(move |window, cx| {
                            Tooltip::new(format!("New session in {}", project_for_new.clone()))
                                .build(window, cx)
                        })
                        .child(
                            gpui_kit::svg()
                                .size(rems(0.75))
                                .path("icons/plus.svg")
                                .text_color(cx.theme().primary),
                        )
                        .on_click({
                            let project = row.name.clone();
                            cx.listener(move |_this, _, _, cx| {
                                cx.stop_propagation();
                                debug!("New session in project: {}", project);
                                let initial_project =
                                    (project != NO_PROJECT).then(|| project.clone());
                                cx.emit(SessionSidebarEvent::NewSessionRequested {
                                    name: None,
                                    initial_project,
                                });
                            })
                        }),
                )
            })
    }

    fn render_empty_hint(&self, text: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_3()
            .text_xs()
            .text_color(cx.theme().muted_foreground.opacity(0.7))
            .child(text)
    }
}

impl EventEmitter<SessionSidebarEvent> for SessionSidebar {}

impl Focusable for SessionSidebar {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionSidebar {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut children: Vec<gpui_kit::AnyElement> = Vec::new();

        // Inbox
        if self.inbox.is_empty() {
            let hint = if self.sessions.is_empty() {
                "No sessions yet"
            } else {
                "Nothing needs you"
            };
            children.push(self.render_empty_hint(hint, cx).into_any_element());
        }
        for item in &self.inbox {
            children.push(item.clone().into_any_element());
        }

        // Settled shelf
        if !self.settled.is_empty() {
            children.push(
                div()
                    .mt(px(6.))
                    .child(self.render_section_header(
                        "settled-header",
                        format!("Settled ({})", self.settled.len()),
                        self.settled_expanded,
                        |this, _| this.settled_expanded = !this.settled_expanded,
                        None,
                        cx,
                    ))
                    .into_any_element(),
            );
            if self.settled_expanded {
                for item in &self.settled {
                    children.push(item.clone().into_any_element());
                }
            }
        }

        // Projects
        let add_project = div()
            .id("add-project-btn")
            .flex_none()
            .size(rems(1.25))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted))
            .tooltip(|window, cx| Tooltip::new("Add project").build(window, cx))
            .child(
                gpui_kit::svg()
                    .size(rems(0.75))
                    .path("icons/plus.svg")
                    .text_color(cx.theme().muted_foreground),
            )
            .on_click(cx.listener(Self::on_add_project_click))
            .into_any_element();
        children.push(
            div()
                .mt(px(10.))
                .child(self.render_section_header(
                    "projects-header",
                    "Projects".to_string(),
                    self.projects_expanded,
                    |this, _| this.projects_expanded = !this.projects_expanded,
                    Some(add_project),
                    cx,
                ))
                .into_any_element(),
        );
        if self.projects_expanded {
            if self.projects.is_empty() {
                children.push(
                    self.render_empty_hint("No projects yet", cx)
                        .into_any_element(),
                );
            }
            for (idx, row) in self.projects.iter().enumerate() {
                children.push(self.render_project_row(idx, row, cx).into_any_element());
            }
        }

        let scale = cx.theme().font_size / px(16.);
        let title = match &self.project_scope {
            Some(scope) => scope.clone(),
            None => "Sessions".to_string(),
        };

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
            // Header: the current scope and the new-session button
            .child(
                div()
                    .flex_none()
                    .pl(px(20.))
                    .pr(px(14.))
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(title)),
                    )
                    .when(self.project_scope.is_some(), |el| {
                        el.child(
                            div()
                                .id("clear-scope-btn")
                                .flex_none()
                                .size(rems(1.5))
                                .rounded_sm()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().muted))
                                .tooltip(|window, cx| {
                                    Tooltip::new("Show all projects").build(window, cx)
                                })
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/close.svg"))
                                        .with_size(Size::Small)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.project_scope = None;
                                    this.relayout(cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .id("new-session-btn")
                            .flex_none()
                            .size(rems(1.5))
                            .rounded_sm()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().muted))
                            .tooltip(|window, cx| Tooltip::new("New session").build(window, cx))
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/plus.svg"))
                                    .with_size(Size::Small)
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .on_click(cx.listener(Self::on_new_session_click)),
                    ),
            )
            // Scrollable list
            .child(
                div().flex_1().min_h(px(0.)).w_full().child(
                    div()
                        .id("chat-items")
                        .px(px(12.))
                        .py(px(6.))
                        .w_full()
                        .h_full()
                        .overflow_y_scrollbar()
                        .flex()
                        .flex_col()
                        .children(children),
                ),
            )
    }
}
