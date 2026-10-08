//! The session sidebar: two views of the same sessions.
//!
//! The inbox holds every session that is not settled, across projects,
//! newest first. Its order is static — activity does not move rows; what a
//! session needs from the user shows as emphasis (see [`SessionListItem`]).
//! The projects view holds every session, settled or not, in its project's
//! folder; folders keep a stable order the user rearranges by dragging. The
//! header switches between the two ("Active" and "Projects"); its buttons start a session (a picker
//! asks where) and add a project.

mod project_order;
mod project_picker;
mod projects_view;
mod session_item;

pub use session_item::{SessionListItem, SessionListItemEvent};

use crate::shared::segmented_switch::segmented_switch;
use crate::shared::settings::SidebarView;
use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::lifecycle::{AwaitingUser, SessionLifecycle};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::*, px,
};
use project_picker::{ProjectPicker, ProjectPickerEvent, animated_surface};
use projects_view::ProjectFolder;
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;
use tracing::debug;

/// Events emitted by the SessionSidebar component
#[derive(Clone, Debug)]
pub enum SessionSidebarEvent {
    /// User selected a specific chat session
    SessionSelected { session_id: String },
    /// User requested deletion of a chat session
    SessionDeleteRequested { session_id: String },
    /// User settled a session: it leaves the inbox
    SessionSettleRequested { session_id: String },
    /// User pulled a settled session back into the inbox
    SessionUnsettleRequested { session_id: String },
    /// User requested creation of a new chat session in a specific project
    NewSessionRequested {
        name: Option<String>,
        initial_project: Option<String>,
    },
    /// User clicked "New project" in the header
    AddProjectRequested,
    /// User saved a temporary project to projects.json
    PersistProjectRequested { project_name: String },
}

pub struct SessionSidebar {
    sessions: Vec<ChatMetadata>,
    /// Whether a session list arrived yet; until then no project is new.
    sessions_loaded: bool,
    lifecycles: HashMap<String, SessionLifecycle>,
    /// Row entities by session id, shared by both views.
    items: HashMap<String, Entity<SessionListItem>>,
    /// Unsettled sessions in display order.
    inbox: Vec<Entity<SessionListItem>>,
    /// Project folders in display order, "No project" last.
    folders: Vec<ProjectFolder>,
    view: SidebarView,
    /// Stored folder order; may name projects that are gone.
    project_order: Vec<String>,
    collapsed_projects: HashSet<String>,
    /// Folders showing all their sessions after "Show more".
    expanded_lists: HashSet<String>,
    hovered_folder: Option<String>,
    /// Project names that are persisted in projects.json.
    /// Projects not in this set are "temporary".
    persisted_projects: HashSet<String>,
    /// The header's project picker for new sessions.
    project_picker: Entity<ProjectPicker>,
    picker_open: bool,

    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    activity_states: HashMap<String, SessionActivityState>,
    awaiting_user: HashMap<String, AwaitingUser>,
    _item_subscriptions: Vec<Subscription>,
    _picker_subscription: Subscription,
}

impl SessionSidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project_picker = cx.new(|cx| ProjectPicker::new(window, cx));
        let picker_subscription = cx.subscribe(&project_picker, Self::on_project_picker_event);
        let settings = cx
            .try_global::<crate::UiSettingsGlobal>()
            .map(|settings| settings.0.sidebar.clone())
            .unwrap_or_default();
        Self {
            sessions: Vec::new(),
            sessions_loaded: false,
            lifecycles: HashMap::new(),
            items: HashMap::new(),
            inbox: Vec::new(),
            folders: Vec::new(),
            view: settings.view,
            project_order: settings.project_order,
            collapsed_projects: settings.collapsed_projects.into_iter().collect(),
            expanded_lists: HashSet::new(),
            hovered_folder: None,
            persisted_projects: HashSet::new(),
            project_picker,
            picker_open: false,
            selected_session_id: None,
            focus_handle: cx.focus_handle(),
            activity_states: HashMap::new(),
            awaiting_user: HashMap::new(),
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
        if self.sessions_loaded && self.sessions == sessions && self.lifecycles == lifecycles {
            return;
        }
        self.sessions = sessions;
        self.sessions_loaded = true;
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
        let show_project = self.view == SidebarView::Inbox;

        let mut items = HashMap::new();
        for session in &self.sessions {
            let lifecycle = self
                .lifecycles
                .get(&session.id)
                .cloned()
                .unwrap_or_default();
            let activity = self.activity_states.get(&session.id).cloned();
            let awaiting = self.awaiting_user.get(&session.id).copied();
            let entity = match existing.remove(&session.id) {
                Some(entity) => {
                    entity.update(cx, |item, cx| {
                        item.update_metadata(session.clone(), cx);
                        item.update_lifecycle(lifecycle, cx);
                    });
                    entity
                }
                None => {
                    let is_selected = self.selected_session_id.as_deref() == Some(&session.id);
                    cx.new(|cx| SessionListItem::new(session.clone(), lifecycle, is_selected, cx))
                }
            };
            entity.update(cx, |item, cx| {
                if let Some(state) = activity {
                    item.update_activity_state(state, cx);
                }
                item.set_awaiting_user(awaiting, cx);
                item.set_show_project(show_project, cx);
            });
            self._item_subscriptions
                .push(cx.subscribe(&entity, Self::on_chat_list_item_event));
            items.insert(session.id.clone(), entity);
        }
        self.items = items;
    }

    /// Recompute the inbox, the project folders and the picker's projects
    /// from the stored sessions and lifecycles. Touches no row entity.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        // Inbox: static order, newest first; an un-settled session surfaces
        // at the top.
        let mut inbox: Vec<(SystemTime, &ChatMetadata)> = self
            .sessions
            .iter()
            .filter_map(|session| {
                let lifecycle = self.lifecycles.get(&session.id);
                if lifecycle.is_some_and(SessionLifecycle::is_settled) {
                    return None;
                }
                let anchor = lifecycle
                    .map(|l| l.inbox_anchor(session.created_at))
                    .unwrap_or(session.created_at);
                Some((anchor, session))
            })
            .collect();
        inbox.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.id.cmp(&a.1.id)));
        self.inbox = inbox
            .into_iter()
            .map(|(_, session)| self.items[&session.id].clone())
            .collect();

        self.relayout_folders(cx);
        cx.notify();
    }

    /// The projects' latest activity: those with sessions and those saved in
    /// projects.json.
    fn known_projects(&self) -> Vec<(String, SystemTime)> {
        let mut latest: HashMap<String, SystemTime> = HashMap::new();
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
        latest.into_iter().collect()
    }

    fn set_view(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        self.view = view;
        let show_project = view == SidebarView::Inbox;
        for item in self.items.values() {
            item.update(cx, |item, cx| item.set_show_project(show_project, cx));
        }
        crate::update_ui_settings(cx, |settings| settings.sidebar.view = view);
        cx.notify();
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
        // A collapsed folder's attention mark may change.
        cx.notify();
    }

    /// The sessions blocked on the user (open permission request or
    /// questions), and what for.
    pub fn set_awaiting_user(
        &mut self,
        sessions: HashMap<String, AwaitingUser>,
        cx: &mut Context<Self>,
    ) {
        if self.awaiting_user == sessions {
            return;
        }
        for (id, item) in &self.items {
            let awaiting = sessions.get(id).copied();
            item.update(cx, |item, cx| item.set_awaiting_user(awaiting, cx));
        }
        self.awaiting_user = sessions;
        cx.notify();
    }

    pub fn set_persisted_projects(&mut self, projects: HashSet<String>, cx: &mut Context<Self>) {
        if self.persisted_projects != projects {
            self.persisted_projects = projects;
            self.relayout_folders(cx);
            cx.notify();
        }
    }

    fn set_picker_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker_open == open {
            return;
        }
        self.picker_open = open;
        if open {
            self.project_picker
                .update(cx, |picker, cx| picker.reset(window, cx));
        }
        cx.notify();
    }

    fn on_project_picker_event(
        &mut self,
        _: Entity<ProjectPicker>,
        event: &ProjectPickerEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ProjectPickerEvent::Picked { project } => {
                debug!("New session in {:?}", project);
                cx.emit(SessionSidebarEvent::NewSessionRequested {
                    name: None,
                    initial_project: project.clone(),
                });
            }
            ProjectPickerEvent::Dismissed => {}
        }
        self.picker_open = false;
        cx.notify();
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

    /// The view switch and the two buttons: new session, new project.
    fn render_header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picker = self.project_picker.clone();
        let input_focus = picker.read(cx).input_focus_handle(cx);
        let sidebar = cx.entity().downgrade();
        div()
            .flex_none()
            // In line with the rows below: the list's 12px plus a row's 2px.
            .px(px(14.))
            .py(px(8.))
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child({
                let sidebar = cx.entity().downgrade();
                segmented_switch(
                    "sidebar-view",
                    &[
                        (SidebarView::Inbox, "Active"),
                        (SidebarView::Projects, "Projects"),
                    ],
                    self.view,
                    move |view, _, cx| {
                        sidebar
                            .update(cx, |sidebar, cx| sidebar.set_view(view, cx))
                            .ok();
                    },
                    window,
                    cx,
                )
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Popover::new("new-session-popover")
                            .anchor(gpui_kit::Anchor::TopRight)
                            .trigger(
                                Button::new("new-session")
                                    .icon(
                                        Icon::default()
                                            .path(SharedString::from(
                                                "icons/message_circle_plus.svg",
                                            ))
                                            .with_size(Size::Medium),
                                    )
                                    .ghost()
                                    .small()
                                    .tooltip("New session"),
                            )
                            .open(self.picker_open)
                            .on_open_change(move |open, window, cx| {
                                let open = *open;
                                let _ = sidebar
                                    .update(cx, |this, cx| this.set_picker_open(open, window, cx));
                            })
                            .track_focus(&input_focus)
                            .appearance(false)
                            .content(move |_, _, cx| animated_surface(picker.clone(), cx)),
                    )
                    .child(
                        Button::new("new-project")
                            .icon(
                                Icon::default()
                                    .path(SharedString::from("icons/folder_plus.svg"))
                                    .with_size(Size::Medium),
                            )
                            .ghost()
                            .small()
                            .tooltip("New project")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(SessionSidebarEvent::AddProjectRequested)
                            })),
                    ),
            )
    }

    fn render_inbox(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !self.inbox.is_empty() {
            return self
                .inbox
                .iter()
                .map(|item| item.clone().into_any_element())
                .collect();
        }
        if self.sessions.is_empty() {
            return vec![render_hint("No sessions yet", cx).into_any_element()];
        }
        vec![
            div()
                .px_2()
                .py_3()
                .flex()
                .flex_col()
                .gap_1()
                .text_xs()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child("Nothing needs you"),
                )
                .child(
                    div()
                        .id("inbox-browse-projects")
                        .cursor_pointer()
                        .text_color(cx.theme().link)
                        .hover(|s| s.underline())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.set_view(SidebarView::Projects, cx)),
                        )
                        .child("Browse projects"),
                )
                .into_any_element(),
        ]
    }
}

fn render_hint(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .px_2()
        .py_3()
        .text_xs()
        .text_color(cx.theme().muted_foreground.opacity(0.7))
        .child(text)
}

impl EventEmitter<SessionSidebarEvent> for SessionSidebar {}

impl Focusable for SessionSidebar {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let children = match self.view {
            SidebarView::Inbox => self.render_inbox(cx),
            SidebarView::Projects => self.render_folders(cx),
        };
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
            .child(self.render_header(window, cx))
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
