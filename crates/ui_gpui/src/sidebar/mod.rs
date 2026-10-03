//! The session sidebar: an inbox, not a file tree.
//!
//! The list holds every session that is not settled, across projects,
//! newest first. The order is static — activity does not move rows; what a
//! session needs from the user shows as emphasis (see [`SessionListItem`]).
//! Settled sessions wait in a collapsed shelf below. Projects are not a
//! structure of the list: each row names its project, and the header's
//! "+" opens a picker to choose where a new session starts.

mod project_picker;
mod session_item;

pub use session_item::{SessionListItem, SessionListItemEvent};

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::lifecycle::SessionLifecycle;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, AppContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, canvas, div, ease_out_quint, prelude::*, px, rems,
};
use project_picker::{ProjectEntry, ProjectPicker, ProjectPickerEvent};
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, StyledExt};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// Settled rows rendered at once; "Show more" adds another page.
const SETTLED_PAGE: usize = 30;
/// How long the settled shelf takes to open or close.
const SHELF_ANIMATION: Duration = Duration::from_millis(180);
/// Height of a settled row before the first layout measured it.
const ROW_HEIGHT_ESTIMATE: f32 = 46.;
/// The shelf opens upward at most this far into the window.
const SHELF_MAX_VIEWPORT_SHARE: f32 = 0.5;

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
    /// Bumped per toggle so the shelf animation replays.
    settled_toggles: u32,
    settled_toggled_at: Option<Instant>,
    /// Height of the rendered settled rows as last laid out; the shelf's
    /// target height. Reset whenever the rows change.
    settled_content_height: Option<Pixels>,
    /// How many settled rows are rendered; grows with "Show more".
    settled_shown: usize,
    /// The header's project picker for new sessions.
    project_picker: Entity<ProjectPicker>,
    picker_open: bool,

    selected_session_id: Option<String>,
    focus_handle: FocusHandle,
    activity_states: HashMap<String, SessionActivityState>,
    awaiting_permission: HashSet<String>,
    _item_subscriptions: Vec<Subscription>,
    _picker_subscription: Subscription,
}

impl SessionSidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project_picker = cx.new(|cx| ProjectPicker::new(window, cx));
        let picker_subscription = cx.subscribe(&project_picker, Self::on_project_picker_event);
        Self {
            sessions: Vec::new(),
            lifecycles: HashMap::new(),
            items: HashMap::new(),
            inbox: Vec::new(),
            settled: Vec::new(),
            persisted_projects: HashSet::new(),
            settled_expanded: false,
            settled_toggles: 0,
            settled_toggled_at: None,
            settled_content_height: None,
            settled_shown: SETTLED_PAGE,
            project_picker,
            picker_open: false,
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
        self.refresh_picker(cx);
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
        self.settled_content_height = None;
        cx.notify();
    }

    fn toggle_settled(&mut self, cx: &mut Context<Self>) {
        self.settled_expanded = !self.settled_expanded;
        self.settled_toggles = self.settled_toggles.wrapping_add(1);
        self.settled_toggled_at = Some(Instant::now());
        if !self.settled_expanded {
            self.settled_shown = SETTLED_PAGE;
        }
        // Render once more when the animation is over: a closed shelf drops
        // its rows.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHELF_ANIMATION).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    fn show_more_settled(&mut self, cx: &mut Context<Self>) {
        self.settled_shown = (self.settled_shown + SETTLED_PAGE).min(self.settled.len());
        self.settled_content_height = None;
        cx.notify();
    }

    /// The picker's choices: projects most recently active first.
    fn project_entries(&self) -> Vec<ProjectEntry> {
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
        projects
            .into_iter()
            .map(|(name, _)| ProjectEntry {
                temporary: !self.persisted_projects.contains(&name),
                name,
            })
            .collect()
    }

    fn refresh_picker(&mut self, cx: &mut Context<Self>) {
        let entries = self.project_entries();
        self.project_picker
            .update(cx, |picker, cx| picker.set_projects(entries, cx));
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

    pub fn set_persisted_projects(&mut self, projects: HashSet<String>, cx: &mut Context<Self>) {
        if self.persisted_projects != projects {
            self.persisted_projects = projects;
            self.refresh_picker(cx);
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
            ProjectPickerEvent::AddProjectRequested => {
                cx.emit(SessionSidebarEvent::AddProjectRequested)
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

    /// The shelf's header row, docked at the bottom of the sidebar.
    fn render_settled_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chevron = if self.settled_expanded {
            "icons/chevron_down.svg"
        } else {
            "icons/chevron_up.svg"
        };
        div()
            .id("settled-header")
            .w_full()
            .px(px(20.))
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_settled(cx)))
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

    /// The settled rows, opening upward from the header. The panel animates
    /// between closed and its content height, capped to a share of the
    /// window; beyond that the rows scroll.
    fn render_settled_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.settled_shown.min(self.settled.len());
        let hidden = self.settled.len() - shown;
        let estimate = px(ROW_HEIGHT_ESTIMATE * shown as f32 + if hidden > 0 { 28. } else { 0. });
        let content_height = self.settled_content_height.unwrap_or(estimate) + px(8.);
        let max_height = window.viewport_size().height * SHELF_MAX_VIEWPORT_SHARE;
        let target = content_height.min(max_height);
        let (from, to) = if self.settled_expanded {
            (px(0.), target)
        } else {
            (target, px(0.))
        };

        let measure = {
            let sidebar = cx.entity().downgrade();
            canvas(
                move |bounds, _, cx| {
                    let _ = sidebar.update(cx, |this, cx| {
                        if this.settled_content_height != Some(bounds.size.height) {
                            this.settled_content_height = Some(bounds.size.height);
                            cx.notify();
                        }
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };

        let mut rows: Vec<AnyElement> = self.settled[..shown]
            .iter()
            .map(|item| item.clone().into_any_element())
            .collect();
        if hidden > 0 {
            rows.push(
                div()
                    .id("settled-show-more")
                    .w_full()
                    .pl(px(26.))
                    .pr_2()
                    .py(px(4.))
                    .cursor_pointer()
                    .rounded_sm()
                    .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
                    .on_click(cx.listener(|this, _, _, cx| this.show_more_settled(cx)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.8))
                            .child(SharedString::from(format!(
                                "Show {} more",
                                hidden.min(SETTLED_PAGE)
                            ))),
                    )
                    .into_any_element(),
            );
        }

        div()
            .id("settled-panel")
            .w_full()
            .flex_none()
            .overflow_hidden()
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                div()
                    .id("settled-items")
                    .px(px(12.))
                    .py(px(4.))
                    .w_full()
                    .h_full()
                    .overflow_y_scrollbar()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .flex()
                            .flex_col()
                            .children(rows)
                            .child(measure),
                    ),
            )
            .with_animation(
                SharedString::from(format!("settled-shelf-{}", self.settled_toggles)),
                Animation::new(SHELF_ANIMATION).with_easing(ease_out_quint()),
                move |el, delta| el.h(from + (to - from) * delta),
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

        let shelf_animating = self
            .settled_toggled_at
            .is_some_and(|at| at.elapsed() < SHELF_ANIMATION);
        let show_shelf = !self.settled.is_empty();
        let show_panel = show_shelf && (self.settled_expanded || shelf_animating);

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
                    .child({
                        let picker = self.project_picker.clone();
                        let input_focus = picker.read(cx).input_focus_handle(cx);
                        let sidebar = cx.entity().downgrade();
                        Popover::new("new-session-popover")
                            .anchor(gpui_kit::Anchor::TopRight)
                            .trigger(
                                Button::new("new-session")
                                    .icon(
                                        Icon::default()
                                            .path(SharedString::from("icons/plus.svg"))
                                            .with_size(Size::Small),
                                    )
                                    .ghost()
                                    .xsmall(),
                            )
                            .open(self.picker_open)
                            .on_open_change(move |open, window, cx| {
                                let open = *open;
                                let _ = sidebar
                                    .update(cx, |this, cx| this.set_picker_open(open, window, cx));
                            })
                            .track_focus(&input_focus)
                            .content(move |_, _, _| picker.clone())
                    }),
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
            // Settled shelf: docked at the bottom, opening upward
            .when(show_panel, |el| {
                el.child(self.render_settled_panel(window, cx))
            })
            .when(show_shelf, |el| el.child(self.render_settled_header(cx)))
    }
}
