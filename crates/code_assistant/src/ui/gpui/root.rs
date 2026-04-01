use super::chat_sidebar::{ChatSidebar, ChatSidebarEvent};

use super::input_area::{InputArea, InputAreaEvent};
use super::messages::MessagesView;
use super::new_project_dialog::{NewProjectDialog, NewProjectDialogEvent};
use super::plan_banner;
use super::theme;
use super::BackendEvent;
use super::{CloseWindow, Gpui, UiEventSender, WorktreeData};
use crate::persistence::ChatMetadata;
use crate::ui::ui_events::UiEvent;
use gpui::{
    div, prelude::*, px, rgba, svg, App, ClickEvent, Context, Entity, FocusHandle, Focusable,
    PathPromptOptions, SharedString, Subscription,
};

use gpui_component::{ActiveTheme, Icon, Sizable, Size};
use std::collections::HashMap;
use tracing::{debug, error, trace, warn};

// Root View - handles overall layout and coordination
pub struct RootView {
    input_area: Entity<InputArea>,
    chat_sidebar: Entity<ChatSidebar>,
    messages_view: Entity<MessagesView>,
    plan_banner: Entity<plan_banner::PlanBanner>,
    recent_keystrokes: Vec<gpui::Keystroke>,
    focus_handle: FocusHandle,
    // Chat sidebar state
    chat_collapsed: bool,
    current_session_id: Option<String>,
    chat_sessions: Vec<ChatMetadata>,
    plan_collapsed_sessions: HashMap<String, bool>,
    plan_collapsed: bool,
    /// Last worktree data synced to the selector (for change detection).
    last_worktree_data: Option<WorktreeData>,
    /// Modal dialog for creating a new project (shown as overlay when Some)
    new_project_dialog: Option<Entity<NewProjectDialog>>,
    /// Pending folder path from the file picker, waiting to create the dialog in render
    pending_project_path: Option<std::path::PathBuf>,
    /// UI zoom scale factor (1.0 = 100%, multiplied with the base font size)
    ui_scale: f32,
    // Subscription to input area events
    _input_area_subscription: Subscription,
    _plan_banner_subscription: Subscription,
    _chat_sidebar_subscription: Subscription,
    _new_project_dialog_subscription: Option<Subscription>,
}

impl RootView {
    pub fn new(
        messages_view: Entity<MessagesView>,
        chat_sidebar: Entity<ChatSidebar>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Create the plan banner
        let plan_banner = cx.new(plan_banner::PlanBanner::new);

        // Create the input area
        let input_area = cx.new(|cx| InputArea::new(window, cx));

        // Subscribe to input area events
        let input_area_subscription =
            cx.subscribe_in(&input_area, window, Self::on_input_area_event);

        // Subscribe to chat sidebar events
        let chat_sidebar_subscription =
            cx.subscribe_in(&chat_sidebar, window, Self::on_chat_sidebar_event);

        // Subscribe to plan banner events
        let plan_banner_subscription =
            cx.subscribe_in(&plan_banner, window, Self::on_plan_banner_event);

        let mut root_view = Self {
            input_area,
            chat_sidebar,
            messages_view,
            plan_banner,
            recent_keystrokes: vec![],
            focus_handle: cx.focus_handle(),
            chat_collapsed: false, // Chat sidebar is visible by default
            current_session_id: None,
            chat_sessions: Vec::new(),

            plan_collapsed_sessions: HashMap::new(),
            plan_collapsed: false,
            last_worktree_data: None,
            new_project_dialog: None,
            pending_project_path: None,
            ui_scale: 1.0,
            _input_area_subscription: input_area_subscription,
            _plan_banner_subscription: plan_banner_subscription,
            _chat_sidebar_subscription: chat_sidebar_subscription,
            _new_project_dialog_subscription: None,
        };

        // Request initial chat session list
        root_view.refresh_chat_list(cx);

        root_view
    }

    pub fn on_toggle_chat_sidebar(
        &mut self,
        _: &ClickEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_collapsed = !self.chat_collapsed;
        self.chat_sidebar.update(cx, |sidebar, cx| {
            sidebar.toggle_collapsed(cx);
        });
        cx.notify();
    }

    fn on_plan_banner_event(
        &mut self,
        _: &Entity<plan_banner::PlanBanner>,
        event: &plan_banner::PlanBannerEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            plan_banner::PlanBannerEvent::Toggle { collapsed } => {
                self.plan_collapsed = *collapsed;
                if let Some(session_id) = &self.current_session_id {
                    self.plan_collapsed_sessions
                        .insert(session_id.clone(), self.plan_collapsed);

                    // Persist to disk so the state survives app restarts
                    if let Some(gpui) = cx.try_global::<Gpui>() {
                        if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                            let _ = sender.try_send(BackendEvent::SetPlanCollapsed {
                                session_id: session_id.clone(),
                                collapsed: self.plan_collapsed,
                            });
                        }
                    }
                }
                cx.notify();
            }
        }
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
        _: &ClickEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        theme::toggle_theme(Some(window), cx);
        cx.notify();
    }

    /// Minimum allowed UI scale factor.
    const MIN_UI_SCALE: f32 = 0.6;
    /// Maximum allowed UI scale factor.
    const MAX_UI_SCALE: f32 = 2.0;
    /// Step size for each zoom in/out click.
    const UI_SCALE_STEP: f32 = 0.1;
    /// Base font size in pixels (matches gpui-component default).
    const BASE_FONT_SIZE: f32 = 16.0;

    fn on_zoom_in(&mut self, _: &ClickEvent, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.ui_scale = (self.ui_scale + Self::UI_SCALE_STEP).min(Self::MAX_UI_SCALE);
        self.apply_ui_scale(cx);
    }

    fn on_zoom_out(&mut self, _: &ClickEvent, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.ui_scale = (self.ui_scale - Self::UI_SCALE_STEP).max(Self::MIN_UI_SCALE);
        self.apply_ui_scale(cx);
    }

    fn apply_ui_scale(&self, cx: &mut Context<Self>) {
        let scaled = px(Self::BASE_FONT_SIZE * self.ui_scale);
        cx.global_mut::<gpui_component::theme::Theme>().font_size = scaled;
        cx.notify();
    }

    #[allow(dead_code)]
    fn on_reset_click(
        &mut self,
        _: &ClickEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.recent_keystrokes.clear();
        self.input_area
            .update(cx, |input_area, cx| input_area.clear(window, cx));
        cx.notify();
    }

    /// Handle InputArea events
    fn on_input_area_event(
        &mut self,
        _input_area: &Entity<InputArea>,
        event: &InputAreaEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputAreaEvent::MessageSubmitted {
                content,
                attachments,
                branch_parent_id,
            } => {
                if let Some(session_id) = self.current_session_id.clone() {
                    self.send_message(
                        &session_id,
                        content.clone(),
                        attachments.clone(),
                        *branch_parent_id,
                        cx,
                    );
                }
            }
            InputAreaEvent::ContentChanged {
                content,
                attachments,
            } => {
                if let Some(session_id) = &self.current_session_id {
                    self.save_draft_for_session(session_id, content, attachments, cx);
                }
            }
            InputAreaEvent::FocusRequested => {
                // Handle focus request if needed
            }
            InputAreaEvent::CancelEditRequested => {
                // Cancel edit mode - reload original messages for this session
                if let Some(session_id) = &self.current_session_id {
                    if let Some(gpui) = cx.try_global::<Gpui>() {
                        if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                            let _ = sender.try_send(BackendEvent::CancelMessageEdit {
                                session_id: session_id.clone(),
                            });
                        }
                    }
                }
            }
            InputAreaEvent::CancelRequested => {
                // Handle cancel/stop request
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
            InputAreaEvent::ClearDraftRequested => {
                // Clear draft immediately and synchronously
                if let Some(session_id) = &self.current_session_id {
                    if let Some(gpui) = cx.try_global::<Gpui>() {
                        gpui.clear_draft_for_session(session_id);
                    }
                }
            }
            InputAreaEvent::ModelChanged { model_name } => {
                debug!("Model selection changed to: {}", model_name);

                if let Some(session_id) = &self.current_session_id {
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::SwitchModel {
                            session_id: session_id.clone(),
                            model_name: model_name.clone(),
                        });
                    } else {
                        error!("Failed to lock backend event sender");
                    }
                }
            }

            InputAreaEvent::SandboxChanged { policy } => {
                if let Some(session_id) = &self.current_session_id {
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::ChangeSandboxPolicy {
                            session_id: session_id.clone(),
                            policy: policy.clone(),
                        });
                    } else {
                        error!("Failed to lock backend event sender");
                    }
                }
            }
            InputAreaEvent::WorktreeSwitchedToLocal => {
                if let Some(session_id) = &self.current_session_id {
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::SwitchWorktree {
                            session_id: session_id.clone(),
                            worktree_path: None,
                            branch: None,
                        });
                    }
                }
            }
            InputAreaEvent::WorktreeSwitched {
                worktree_path,
                branch,
            } => {
                if let Some(session_id) = &self.current_session_id {
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::SwitchWorktree {
                            session_id: session_id.clone(),
                            worktree_path: Some(worktree_path.clone()),
                            branch: Some(branch.clone()),
                        });
                    }
                }
            }

            InputAreaEvent::WorktreeCreateRequested => {
                if let Some(session_id) = &self.current_session_id {
                    // Generate a branch name from the session id (last 8 chars)
                    let short_id = if session_id.len() > 8 {
                        &session_id[session_id.len() - 8..]
                    } else {
                        session_id.as_str()
                    };
                    let branch_name = format!("worktree-{short_id}");
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::CreateWorktree {
                            session_id: session_id.clone(),
                            branch_name,
                            base_branch: None,
                        });
                    }
                }
            }
            InputAreaEvent::WorktreeRefreshRequested => {
                if let Some(session_id) = &self.current_session_id {
                    let gpui = cx
                        .try_global::<Gpui>()
                        .expect("Failed to obtain Gpui global");
                    if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::ListBranchesAndWorktrees {
                            session_id: session_id.clone(),
                        });
                    }
                }
            }
        }
    }

    /// Handle ChatSidebar events
    fn on_chat_sidebar_event(
        &mut self,
        _chat_sidebar: &Entity<ChatSidebar>,
        event: &ChatSidebarEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let ChatSidebarEvent::AddProjectRequested = event {
            self.open_add_project_flow(window, cx);
            return;
        }

        let gpui = cx
            .try_global::<Gpui>()
            .expect("Failed to obtain Gpui global");
        if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
            match event {
                ChatSidebarEvent::SessionSelected { session_id } => {
                    let _ = sender.try_send(BackendEvent::LoadSession {
                        session_id: session_id.clone(),
                    });
                }
                ChatSidebarEvent::SessionDeleteRequested { session_id } => {
                    let _ = sender.try_send(BackendEvent::DeleteSession {
                        session_id: session_id.clone(),
                    });
                }
                ChatSidebarEvent::NewSessionRequested {
                    name,
                    initial_project,
                } => {
                    let _ = sender.try_send(BackendEvent::CreateNewSession {
                        name: name.clone(),
                        initial_project: initial_project.clone(),
                    });
                }
                ChatSidebarEvent::AddProjectRequested => {
                    // Handled above
                }
            }
        } else {
            error!("Failed to lock backend event sender");
        }
    }

    fn send_message(
        &mut self,
        session_id: &str,
        content: String,
        attachments: Vec<crate::persistence::DraftAttachment>,
        branch_parent_id: Option<crate::persistence::NodeId>,
        cx: &mut Context<Self>,
    ) {
        if content.trim().is_empty() && attachments.is_empty() {
            return;
        }

        // Send user message event if we have an active session
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

            // Log branch editing info
            if branch_parent_id.is_some() {
                tracing::info!(
                    "RootView: Sending edited message (branch from {:?}) to session {}: {} (with {} attachments)",
                    branch_parent_id,
                    session_id,
                    content,
                    attachments.len()
                );
            }

            if agent_is_running {
                // Queue the message for the running agent
                tracing::info!(
                    "RootView: Queuing user message for running agent in session {}: {} (with {} attachments)",
                    session_id,
                    content,
                    attachments.len()
                );
                let _ = sender.0.try_send(UiEvent::QueueUserMessage {
                    message: content.clone(),
                    session_id: session_id.to_string(),
                    attachments: attachments.clone(),
                });
            } else {
                // Send message normally (agent is idle)
                tracing::info!(
                    "RootView: Sending user message to session {}: {} (with {} attachments, branch_parent: {:?})",
                    session_id,
                    content,
                    attachments.len(),
                    branch_parent_id
                );
                let _ = sender.0.try_send(UiEvent::SendUserMessage {
                    message: content.clone(),
                    session_id: session_id.to_string(),
                    attachments: attachments.clone(),
                    branch_parent_id,
                });
            }
        }
    }

    /// Handle message edit ready event - load content into input area
    pub fn handle_message_edit_ready(
        &mut self,
        content: String,
        attachments: Vec<crate::persistence::DraftAttachment>,
        branch_parent_id: Option<crate::persistence::NodeId>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.input_area.update(cx, |input_area, cx| {
            input_area.set_content_for_edit(content, attachments, branch_parent_id, window, cx);
        });
        cx.notify();
    }

    fn save_draft_for_session(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[crate::persistence::DraftAttachment],
        cx: &mut Context<Self>,
    ) {
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.save_draft_for_session(session_id, content, attachments);
        }
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

    /// Open the add-project flow: native folder picker, then name dialog.
    fn open_add_project_flow(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        debug!("Opening add-project folder picker");

        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select project folder".into()),
        });

        cx.spawn(async move |this, cx| {
            match receiver.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        debug!("Folder selected: {:?}", path);
                        // Store the path; the dialog will be created in the next render cycle
                        // (where we have window access).
                        let _ = this.update(cx, |this, cx| {
                            this.pending_project_path = Some(path);
                            cx.notify();
                        });
                    }
                }
                Ok(Ok(None)) => {
                    debug!("Folder picker cancelled");
                }
                Ok(Err(e)) => {
                    error!("Folder picker error: {}", e);
                }
                Err(e) => {
                    error!("Folder picker channel error: {}", e);
                }
            }
        })
        .detach();
    }

    /// Show the new project name dialog after a folder has been selected.
    /// Called from render() where we have window access.
    fn show_new_project_dialog(
        &mut self,
        path: std::path::PathBuf,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| NewProjectDialog::new(path, window, cx));
        let subscription = cx.subscribe_in(&dialog, window, Self::on_new_project_dialog_event);
        self.new_project_dialog = Some(dialog);
        self._new_project_dialog_subscription = Some(subscription);
    }

    /// Handle events from the NewProjectDialog.
    fn on_new_project_dialog_event(
        &mut self,
        _dialog: &Entity<NewProjectDialog>,
        event: &NewProjectDialogEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            NewProjectDialogEvent::Confirmed { name, path } => {
                debug!("New project confirmed: name='{}', path={:?}", name, path);
                // Send AddProject to backend
                let gpui = cx
                    .try_global::<Gpui>()
                    .expect("Failed to obtain Gpui global");
                if let Some(sender) = gpui.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::AddProject {
                        name: name.clone(),
                        path: path.clone(),
                    });
                }
                // Close dialog
                self.new_project_dialog = None;
                self._new_project_dialog_subscription = None;
                cx.notify();
            }
            NewProjectDialogEvent::Cancelled => {
                debug!("New project dialog cancelled");
                self.new_project_dialog = None;
                self._new_project_dialog_subscription = None;
                cx.notify();
            }
        }
    }

    /// Render the floating status popover if needed (currently: errors only).
    fn render_status_popover(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        // Get current error from global Gpui
        let current_error = if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.get_current_error()
        } else {
            None
        };

        // Check for error first (higher priority than activity states)
        if let Some(error_message) = current_error {
            let (bg_color, border_color, text_color) = if cx.theme().is_dark() {
                (
                    rgba(0x7F1D1D80), // Dark red background with transparency
                    rgba(0xEF4444FF), // Red border
                    rgba(0xFCA5A5FF), // Light red text
                )
            } else {
                (
                    rgba(0xFEF2F2FF), // Light red background
                    rgba(0xF87171FF), // Red border
                    rgba(0xDC2626FF), // Dark red text
                )
            };

            // Return the error popover positioned at bottom of scroll area
            return vec![div()
                .absolute()
                .bottom_2() // Small gap from the bottom of the scroll area
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
                        .overflow_hidden()
                        .flex()
                        .items_start() // Align items to top for multi-line text
                        .gap_2()
                        .max_w(px(600.)) // Limit width for long error messages
                        .min_w(px(200.)) // Ensure minimum width
                        .child(
                            div()
                                .flex_none()
                                .mt(px(1.)) // Slight top margin to align with first line of text
                                .child(
                                    svg()
                                        .size(px(14.))
                                        .path(SharedString::from("icons/circle_stop.svg"))
                                        .text_color(text_color),
                                ),
                        )
                        .child(
                            div()
                                .text_color(text_color)
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight(500.0))
                                .flex_grow()
                                .flex_shrink()
                                .min_w_0() // Allow shrinking below content size for text wrapping
                                .overflow_hidden() // Prevent text from overflowing
                                .whitespace_normal() // Enable text wrapping
                                .line_height(px(14.)) // Set line height for better readability
                                .child(error_message),
                        )
                        .child(
                            // Add a close button
                            div()
                                .id("error-close-btn")
                                .flex_none()
                                .size(px(20.))
                                .rounded_sm()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
                                .child(
                                    svg()
                                        .size(px(12.))
                                        .path(SharedString::from("icons/close.svg"))
                                        .text_color(text_color),
                                )
                                .on_click(|_, _, cx| {
                                    if let Some(sender) = cx.try_global::<UiEventSender>() {
                                        let _ = sender.0.try_send(UiEvent::ClearError);
                                    }
                                }),
                        ),
                )
                .into_any_element()];
        }

        // Transient status notification (lower priority than errors)
        let transient_status = if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.get_transient_status()
        } else {
            None
        };

        if let Some(status_message) = transient_status {
            let (bg_color, border_color, text_color, icon_color) = if cx.theme().is_dark() {
                (
                    rgba(0x78350F80), // Dark amber background with transparency
                    rgba(0xF59E0BFF), // Amber border
                    rgba(0xFDE68AFF), // Light amber text
                    rgba(0xFBBF24FF), // Amber icon
                )
            } else {
                (
                    rgba(0xFFFBEBFF), // Light amber background
                    rgba(0xF59E0BFF), // Amber border
                    rgba(0x92400EFF), // Dark amber text
                    rgba(0xD97706FF), // Amber icon
                )
            };

            return vec![div()
                .absolute()
                .bottom_2()
                .left(px(0.))
                .right(px(0.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .bg(bg_color)
                        .border_1()
                        .border_color(border_color)
                        .rounded_lg()
                        .shadow_lg()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .gap_2()
                        .max_w(px(600.))
                        .min_w(px(200.))
                        .child(
                            div().flex_none().child(
                                svg()
                                    .size(px(14.))
                                    .path(SharedString::from("icons/arrow_circle.svg"))
                                    .text_color(icon_color),
                            ),
                        )
                        .child(
                            div()
                                .text_color(text_color)
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight(500.0))
                                .flex_grow()
                                .flex_shrink()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_normal()
                                .line_height(px(14.))
                                .child(status_message),
                        ),
                )
                .into_any_element()];
        }

        // Activity states (WaitingForResponse, RateLimited) are now shown
        // inline at the bottom of the messages list — no floating popover.

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
        if let Some(session_id) = new_session_id.as_ref() {
            self.plan_collapsed = *self
                .plan_collapsed_sessions
                .get(session_id)
                .unwrap_or(&false);
        } else {
            self.plan_collapsed = false;
        }

        // Read everything we need from Gpui in a scoped borrow, then drop the ref
        let (input_value, attachments, backend_sender) = {
            let gpui = cx.try_global::<Gpui>();

            let (input_value, attachments) = if let (Some(new_id), Some(gpui)) =
                (new_session_id.as_ref(), &gpui)
            {
                if let Some((draft_text, draft_attachments)) = gpui.load_draft_for_session(new_id) {
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
                debug!("No new session, clearing text input and attachments");
                ("".to_string(), Vec::new())
            };

            // Extract the backend sender and clear worktree data while we hold the ref
            let backend_sender = if let Some(gpui) = &gpui {
                *gpui.current_worktree_data.lock().unwrap() = None;
                gpui.backend_event_sender.lock().unwrap().as_ref().cloned()
            } else {
                None
            };

            (input_value, attachments, backend_sender)
            // `gpui` borrow of `cx` dropped here
        };

        // Update the input area with the new content
        self.input_area.update(cx, |input_area, cx| {
            input_area.set_content(input_value, attachments, window, cx);
        });

        // Request fresh worktree listing for the new session
        if let (Some(session_id), Some(sender)) = (new_session_id.as_ref(), &backend_sender) {
            let _ = sender.try_send(BackendEvent::ListBranchesAndWorktrees {
                session_id: session_id.clone(),
            });
        }

        // Reset the worktree selector to "Local" while waiting for fresh data
        self.last_worktree_data = None;
        self.input_area.update(cx, |input_area, cx| {
            let selector = input_area.worktree_selector().clone();
            selector.update(cx, |sel, cx| {
                sel.set_local(window, cx);
            });
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
        // Get current chat state from global Gpui
        let (
            chat_sessions,
            current_session_id,
            current_activity_state,
            current_model,
            plan_state,
            current_sandbox_policy,
            current_worktree_data,
        ) = if let Some(gpui) = cx.try_global::<Gpui>() {
            (
                gpui.get_chat_sessions(),
                gpui.get_current_session_id(),
                gpui.current_session_activity_state.lock().unwrap().clone(),
                gpui.get_current_model(),
                gpui.get_plan_state(),
                gpui.get_current_sandbox_policy(),
                gpui.get_current_worktree_data(),
            )
        } else {
            (Vec::new(), None, None, None, None, None, None)
        };

        // Update chat sidebar if needed
        if self.chat_sessions != chat_sessions || self.current_session_id != current_session_id {
            let previous_session_id = self.current_session_id.clone();
            self.chat_sessions = chat_sessions.clone();
            self.current_session_id = current_session_id.clone();

            // Populate plan_collapsed_sessions from metadata (for sessions not yet toggled in this run)
            for meta in &chat_sessions {
                self.plan_collapsed_sessions
                    .entry(meta.id.clone())
                    .or_insert(meta.plan_collapsed);
            }

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

        // Check for pending edit (message editing for branching)
        if let Some(gpui) = cx.try_global::<Gpui>() {
            if let Some(pending_edit) = gpui.take_pending_edit() {
                self.handle_message_edit_ready(
                    pending_edit.content,
                    pending_edit.attachments,
                    pending_edit.branch_parent_id,
                    window,
                    cx,
                );
            }
        }

        // Ensure InputArea stays in sync with the current model
        let selected_model = self.input_area.read(cx).current_model();
        if selected_model != current_model {
            debug!(
                "Current model changed from {:?} to {:?}",
                selected_model, current_model
            );
            let model_to_set = current_model.clone();
            self.input_area.update(cx, |input_area, cx| {
                input_area.set_current_model(model_to_set, window, cx);
            });
        }

        if let Some(policy) = current_sandbox_policy {
            if self.input_area.read(cx).current_sandbox_policy() != policy {
                self.input_area.update(cx, |input_area, cx| {
                    input_area.set_current_sandbox_policy(policy.clone(), window, cx);
                });
            }
        }

        // Sync worktree data to the WorktreeSelector (only when changed)
        if current_worktree_data != self.last_worktree_data {
            self.last_worktree_data = current_worktree_data.clone();
            if let Some(wt_data) = current_worktree_data {
                self.input_area.update(cx, |input_area, cx| {
                    let selector = input_area.worktree_selector().clone();
                    selector.update(cx, |sel, cx| {
                        sel.set_worktrees(
                            &wt_data.worktrees,
                            wt_data.current_worktree_path.as_ref(),
                            wt_data.is_git_repo,
                            window,
                            cx,
                        );
                    });
                });
            }
        }

        // Update InputArea with current agent state
        let agent_is_running = if let Some(state) = &current_activity_state {
            !matches!(state, crate::session::instance::SessionActivityState::Idle)
        } else {
            false
        };

        let cancel_enabled = if agent_is_running {
            if let (Some(gpui), Some(session_id)) = (cx.try_global::<Gpui>(), &current_session_id) {
                !gpui
                    .session_stop_requests
                    .lock()
                    .unwrap()
                    .contains(session_id)
            } else {
                true
            }
        } else {
            false
        };

        self.input_area.update(cx, |input_area, _cx| {
            input_area.set_agent_state(agent_is_running, cancel_enabled);
        });

        let plan_for_banner = plan_state.clone().filter(|plan| !plan.entries.is_empty());
        let plan_visible = plan_for_banner.is_some();
        let plan_for_update = plan_for_banner.clone();
        self.plan_banner.update(cx, |banner, cx| {
            banner.set_plan(plan_for_update, self.plan_collapsed, cx);
        });

        // If a folder was selected from the picker, create the dialog now (we have window access)
        if let Some(path) = self.pending_project_path.take() {
            self.show_new_project_dialog(path, window, cx);
        }

        let new_project_dialog = self.new_project_dialog.clone();

        // Main container with titlebar and content
        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .on_action(cx.listener(Self::on_cancel_agent))
            .bg(cx.theme().background)
            .track_focus(&self.focus_handle(cx))
            .relative()
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
                    .justify_start()
                    // Left padding for macOS traffic lights (doubled for more space)
                    .pl(px(86.))
                    // Left side - controls
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            // Chat sidebar toggle button
                            .child(
                                div()
                                    .id("toggle-sidebar-btn")
                                    .size(px(28.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(
                                        Icon::default()
                                            .path(SharedString::from(if self.chat_collapsed {
                                                "icons/panel_left_open.svg"
                                            } else {
                                                "icons/panel_left_close.svg"
                                            }))
                                            .with_size(Size::Small)
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .on_click(cx.listener(Self::on_toggle_chat_sidebar)),
                            )
                            // Theme toggle button
                            .child(
                                div()
                                    .id("toggle-theme-btn")
                                    .size(px(28.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(
                                        Icon::default()
                                            .path(SharedString::from(if cx.theme().is_dark() {
                                                "icons/theme_light.svg"
                                            } else {
                                                "icons/theme_dark.svg"
                                            }))
                                            .with_size(Size::Small)
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .on_click(cx.listener(Self::on_toggle_theme)),
                            )
                            // Zoom out button
                            .child(
                                div()
                                    .id("zoom-out-btn")
                                    .size(px(28.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(
                                        Icon::default()
                                            .path(SharedString::from("icons/zoom_out.svg"))
                                            .with_size(Size::Small)
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .on_click(cx.listener(Self::on_zoom_out)),
                            )
                            // Zoom level indicator
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!(
                                        "{}%",
                                        (self.ui_scale * 100.0).round() as u32
                                    ))),
                            )
                            // Zoom in button
                            .child(
                                div()
                                    .id("zoom-in-btn")
                                    .size(px(28.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child(
                                        Icon::default()
                                            .path(SharedString::from("icons/zoom_in.svg"))
                                            .with_size(Size::Small)
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .on_click(cx.listener(Self::on_zoom_in)),
                            ),
                    ),
            )
            // Main content area with chat sidebar and messages+input (2-column layout)
            .child(
                div()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_row() // 2-column layout: chat | messages+input
                    // Left sidebar: Chat sessions
                    .child(self.chat_sidebar.clone())
                    .child(
                        // Messages and input (content area) with floating popover
                        div()
                            .bg(cx.theme().popover)
                            .flex()
                            .flex_col()
                            .flex_grow() // Grow to take available space
                            .flex_shrink() // Allow shrinking if needed
                            .overflow_hidden() // Prevent overflow
                            .child(
                                // Scroll area wrapper: relative container for popover overlay
                                div()
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h_0()
                                    .child(
                                        // Messages display area - virtualized list
                                        self.messages_view.clone(),
                                    )
                                    // Status popover - overlaid at bottom of scroll area
                                    .children(self.render_status_popover(cx)),
                            )
                            // Session plan banner (if available)
                            .when(plan_visible, |s| s.child(self.plan_banner.clone()))
                            // Input area sits at the bottom
                            .child(
                                div()
                                    .flex_none()
                                    .bg(cx.theme().background)
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .child(self.input_area.clone()),
                            ),
                    ),
            )
            // Modal dialog overlay for new project creation
            .when_some(new_project_dialog, |el, dialog| el.child(dialog))
    }
}
