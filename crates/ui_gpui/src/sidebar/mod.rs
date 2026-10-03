//! The session sidebar: an inbox, not a file tree.
//!
//! The list holds every session that is not settled, across projects,
//! newest first. The order is static — activity does not move rows; what a
//! session needs from the user shows as emphasis (see [`SessionListItem`]).
//! Settled sessions wait in a collapsed shelf below. Projects are not a
//! structure of the list: each row names its project, and the header's
//! "+" opens a picker to choose where a new session starts.

mod session_item;

pub use session_item::{SessionListItem, SessionListItemEvent};

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::lifecycle::SessionLifecycle;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::*, px, rems,
};

use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, StyledExt, tooltip::Tooltip};
use std::collections::{HashMap, HashSet};
use tracing::debug;

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
    /// User chose "Add project…" in the project picker
    AddProjectRequested,
}

/// What the project picker offers: a project, no project, or adding one.
#[derive(Clone, Debug, PartialEq)]
enum ProjectPickValue {
    Project(String),
    NoProject,
    AddProject,
}

#[derive(Clone)]
struct ProjectPick {
    title: SharedString,
    value: ProjectPickValue,
    /// Known only from sessions, not saved in projects.json.
    temporary: bool,
}

impl SelectItem for ProjectPick {
    type Value = ProjectPickValue;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .child(
                gpui_kit::svg()
                    .flex_none()
                    .size(rems(0.75))
                    .path(match self.value {
                        ProjectPickValue::Project(_) => "icons/file_icons/folder.svg",
                        ProjectPickValue::NoProject => "icons/file_generic.svg",
                        ProjectPickValue::AddProject => "icons/plus.svg",
                    })
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.title.clone()),
            )
            .when(self.temporary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child("temporary"),
                )
            })
    }
}

pub struct SessionSidebar {
    sessions: Vec<ChatMetadata>,
    lifecycles: HashMap<String, SessionLifecycle>,
    /// Row entities by session id, reused across rebuilds.
    items: HashMap<String, Entity<SessionListItem>>,
    /// Unsettled sessions in display order.
    inbox: Vec<Entity<SessionListItem>>,
    /// Settled sessions in display order.
    settled: Vec<Entity<SessionListItem>>,
    /// Project names that are persisted in projects.json.
    /// Projects not in this set are "temporary".
    persisted_projects: HashSet<String>,
    settled_expanded: bool,
    /// The header's project picker for new sessions.
    project_picker: Entity<SelectState<SearchableVec<ProjectPick>>>,
    /// The picker's items follow the sessions; they are refreshed on the
    /// next render because updating them needs the window.
    picker_dirty: bool,

    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    activity_states: HashMap<String, SessionActivityState>,
    awaiting_permission: HashSet<String>,
    _item_subscriptions: Vec<Subscription>,
    _picker_subscription: Subscription,
}

impl SessionSidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project_picker = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<ProjectPick>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let picker_subscription = cx.subscribe_in(&project_picker, window, Self::on_project_picked);
        Self {
            sessions: Vec::new(),
            lifecycles: HashMap::new(),
            items: HashMap::new(),
            inbox: Vec::new(),
            settled: Vec::new(),
            persisted_projects: HashSet::new(),
            settled_expanded: false,
            project_picker,
            picker_dirty: true,
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            activity_states: HashMap::new(),
            awaiting_permission: HashSet::new(),
            _item_subscriptions: Vec::new(),
            _picker_subscription: picker_subscription,
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
        self.picker_dirty = true;
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

    /// Recompute the inbox and the settled shelf from the stored sessions
    /// and lifecycles. Touches no row entity.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        // Inbox: static order, newest first; an un-settled session surfaces
        // at the top. Settled shelf: most recently settled first.
        let mut inbox: Vec<(std::time::SystemTime, &ChatMetadata)> = Vec::new();
        let mut settled: Vec<(std::time::SystemTime, &ChatMetadata)> = Vec::new();
        for session in &self.sessions {
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

    /// The picker's choices: projects most recently active first, then no
    /// project, then adding one.
    fn project_picks(&self) -> Vec<ProjectPick> {
        let mut latest: HashMap<String, std::time::SystemTime> = HashMap::new();
        for session in &self.sessions {
            if session.initial_project.is_empty() {
                continue;
            }
            latest
                .entry(session.initial_project.clone())
                .and_modify(|at| *at = (*at).max(session.updated_at))
                .or_insert(session.updated_at);
        }
        for project in &self.persisted_projects {
            latest
                .entry(project.clone())
                .or_insert(std::time::UNIX_EPOCH);
        }
        let mut projects: Vec<(String, std::time::SystemTime)> = latest.into_iter().collect();
        projects.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut picks: Vec<ProjectPick> = projects
            .into_iter()
            .map(|(name, _)| ProjectPick {
                title: name.clone().into(),
                temporary: !self.persisted_projects.contains(&name),
                value: ProjectPickValue::Project(name),
            })
            .collect();
        picks.push(ProjectPick {
            title: "No project".into(),
            value: ProjectPickValue::NoProject,
            temporary: false,
        });
        picks.push(ProjectPick {
            title: "Add project…".into(),
            value: ProjectPickValue::AddProject,
            temporary: false,
        });
        picks
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
        if self.persisted_projects != projects {
            self.persisted_projects = projects;
            self.picker_dirty = true;
        }
    }

    fn on_project_picked(
        &mut self,
        picker: &Entity<SelectState<SearchableVec<ProjectPick>>>,
        event: &SelectEvent<SearchableVec<ProjectPick>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        match value {
            ProjectPickValue::Project(project) => {
                debug!("New session in project: {project}");
                cx.emit(SessionSidebarEvent::NewSessionRequested {
                    name: None,
                    initial_project: Some(project.clone()),
                });
            }
            ProjectPickValue::NoProject => {
                debug!("New session without project");
                cx.emit(SessionSidebarEvent::NewSessionRequested {
                    name: None,
                    initial_project: None,
                });
            }
            ProjectPickValue::AddProject => cx.emit(SessionSidebarEvent::AddProjectRequested),
        }
        // The picker launches; it does not hold a selection.
        picker.update(cx, |state, cx| state.set_selected_index(None, window, cx));
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

    fn render_settled_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chevron = if self.settled_expanded {
            "icons/chevron_down.svg"
        } else {
            "icons/chevron_right.svg"
        };
        div()
            .id("settled-header")
            .w_full()
            .px_2()
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .rounded_sm()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.settled_expanded = !this.settled_expanded;
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
                    .text_xs()
                    .font_medium()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!(
                        "Settled ({})",
                        self.settled.len()
                    ))),
            )
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.picker_dirty {
            let picks = self.project_picks();
            self.project_picker.update(cx, |state, cx| {
                state.set_items(SearchableVec::new(picks), window, cx)
            });
            self.picker_dirty = false;
        }

        let mut children: Vec<AnyElement> = Vec::new();

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
                    .child(self.render_settled_header(cx))
                    .into_any_element(),
            );
            if self.settled_expanded {
                for item in &self.settled {
                    children.push(item.clone().into_any_element());
                }
            }
        }

        let scale = cx.theme().font_size / px(16.);

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
            // Header: title and the project picker that starts a session
            .child(
                div()
                    .flex_none()
                    .pl(px(20.))
                    .pr(px(10.))
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
                            .text_sm()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child("Sessions"),
                    )
                    .child(
                        div()
                            .id("new-session-picker")
                            .flex_none()
                            .w(px(30.))
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().muted))
                            .tooltip(|window, cx| Tooltip::new("New session in…").build(window, cx))
                            .child(
                                Select::new(&self.project_picker)
                                    .placeholder("")
                                    .search_placeholder("Project")
                                    .with_size(Size::XSmall)
                                    .appearance(false)
                                    .menu_width(px(230.))
                                    .icon(
                                        Icon::default()
                                            .path(SharedString::from("icons/plus.svg"))
                                            .with_size(Size::Small)
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            ),
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
