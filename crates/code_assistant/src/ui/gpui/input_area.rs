use super::attachment::{AttachmentEvent, AttachmentView};
use super::file_icons;
use super::model_selector::{ModelSelector, ModelSelectorEvent};
use super::sandbox_selector::{SandboxSelector, SandboxSelectorEvent};
use super::worktree_selector::{WorktreeSelector, WorktreeSelectorEvent};
use crate::persistence::{DraftAttachment, NodeId};
use base64::Engine;
use gpui::{
    div, prelude::*, px, ClickEvent, ClipboardEntry, Context, CursorStyle, Entity, EventEmitter,
    FocusHandle, Focusable, Render, SharedString, Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState, Paste};
use gpui_component::{ActiveTheme, Icon};
use sandbox::SandboxPolicy;

/// Events emitted by the InputArea component
#[derive(Clone, Debug)]
pub enum InputAreaEvent {
    /// Message submitted with content and attachments
    MessageSubmitted {
        content: String,
        attachments: Vec<DraftAttachment>,
        /// If set, this message creates a new branch from this parent node
        branch_parent_id: Option<NodeId>,
    },
    /// Content changed (for draft saving)
    ContentChanged {
        content: String,
        attachments: Vec<DraftAttachment>,
    },
    /// Focus requested on the input
    FocusRequested,
    /// Cancel/stop requested (for agent cancellation)
    CancelRequested,
    /// Cancel edit mode requested (restore original messages)
    CancelEditRequested,
    /// Clear draft requested (before clearing input)
    ClearDraftRequested,

    /// Model selection changed
    ModelChanged { model_name: String },
    /// Sandbox mode changed
    SandboxChanged { policy: SandboxPolicy },
    /// User wants to switch to local (no worktree)
    WorktreeSwitchedToLocal,
    /// User selected an existing worktree
    WorktreeSwitched {
        worktree_path: std::path::PathBuf,
        branch: String,
    },
    /// User wants to create a new worktree
    WorktreeCreateRequested,
    /// Worktree selector opened — request fresh data from backend
    WorktreeRefreshRequested,
}

/// Self-contained input area component that handles text input and attachments
pub struct InputArea {
    text_input: Entity<InputState>,
    model_selector: Entity<ModelSelector>,
    sandbox_selector: Entity<SandboxSelector>,
    worktree_selector: Entity<WorktreeSelector>,
    current_model: Option<String>,
    current_sandbox_policy: SandboxPolicy,
    attachments: Vec<DraftAttachment>,
    attachment_views: Vec<Entity<AttachmentView>>,
    focus_handle: FocusHandle,

    // Agent state for button rendering
    agent_is_running: bool,
    cancel_enabled: bool,
    externally_locked: bool,

    // Context usage ratio (0.0–1.0)
    context_usage_ratio: Option<f32>,

    // Branch editing state
    /// When editing a message, this is the parent node ID where the new branch will be created
    branch_parent_id: Option<NodeId>,

    // Subscriptions
    _input_subscription: Subscription,
    _model_selector_subscription: Subscription,
    _sandbox_selector_subscription: Subscription,
    _worktree_selector_subscription: Subscription,
}

impl InputArea {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Create the text input
        let text_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 8)
                .placeholder("Type your message...")
        });

        // Subscribe to text input events
        let input_subscription = cx.subscribe_in(&text_input, window, Self::on_input_event);

        // Create the model selector
        let model_selector = cx.new(|cx| ModelSelector::new(window, cx));
        let sandbox_selector = cx.new(|cx| SandboxSelector::new(window, cx));
        let worktree_selector = cx.new(|cx| WorktreeSelector::new(window, cx));

        // Subscribe to model selector events
        let model_selector_subscription =
            cx.subscribe_in(&model_selector, window, Self::on_model_selector_event);
        let sandbox_selector_subscription =
            cx.subscribe_in(&sandbox_selector, window, Self::on_sandbox_selector_event);
        let worktree_selector_subscription =
            cx.subscribe_in(&worktree_selector, window, Self::on_worktree_selector_event);

        Self {
            text_input,
            model_selector,
            sandbox_selector,
            worktree_selector,
            current_model: None,
            current_sandbox_policy: SandboxPolicy::DangerFullAccess,
            attachments: Vec::new(),
            attachment_views: Vec::new(),
            focus_handle: cx.focus_handle(),

            agent_is_running: false,
            cancel_enabled: false,
            externally_locked: false,
            context_usage_ratio: None,

            branch_parent_id: None,

            _input_subscription: input_subscription,
            _model_selector_subscription: model_selector_subscription,
            _sandbox_selector_subscription: sandbox_selector_subscription,
            _worktree_selector_subscription: worktree_selector_subscription,
        }
    }

    /// Set the input value and attachments (for loading drafts)
    pub fn set_content(
        &mut self,
        text: String,
        attachments: Vec<DraftAttachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Update text input
        self.text_input.update(cx, |text_input, cx| {
            text_input.set_value(text, window, cx);
        });

        // Update attachments
        self.attachments = attachments;
        self.rebuild_attachment_views(cx);
    }

    /// Set content for editing a message (creates a branch)
    pub fn set_content_for_edit(
        &mut self,
        text: String,
        attachments: Vec<DraftAttachment>,
        branch_parent_id: Option<NodeId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_content(text, attachments, window, cx);
        self.branch_parent_id = branch_parent_id;
        cx.notify();
    }

    /// Clear the edit mode state
    fn clear_edit_mode(&mut self) {
        self.branch_parent_id = None;
    }

    /// Check if we're currently in edit mode (editing an existing message)
    pub fn is_editing(&self) -> bool {
        self.branch_parent_id.is_some()
    }

    /// Cancel edit mode - clears input and emits event to restore original messages
    pub fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_parent_id.is_some() {
            self.branch_parent_id = None;
            self.clear(window, cx);
            cx.emit(InputAreaEvent::CancelEditRequested);
        }
    }

    /// Sync the dropdown with the current model selection
    pub fn set_current_model(
        &mut self,
        model_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.current_model = model_name.clone();
        self.model_selector.update(cx, |selector, cx| {
            selector.set_current_model(model_name, window, cx)
        });
    }

    /// Read the currently selected model name
    pub fn current_model(&self) -> Option<String> {
        self.current_model.clone()
    }

    pub fn set_current_sandbox_policy(
        &mut self,
        policy: SandboxPolicy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.current_sandbox_policy = policy.clone();
        self.sandbox_selector.update(cx, |selector, cx| {
            selector.set_policy(policy, window, cx);
        });
    }

    pub fn current_sandbox_policy(&self) -> SandboxPolicy {
        self.current_sandbox_policy.clone()
    }

    /// Ensure the model list stays up to date
    #[allow(dead_code)]
    pub fn refresh_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_selector
            .update(cx, |selector, cx| selector.refresh_models(window, cx));
    }

    /// Clear the input content
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Clear text input
        self.text_input.update(cx, |text_input, cx| {
            text_input.set_value("", window, cx);
        });

        // Clear attachments
        self.attachments.clear();
        self.attachment_views.clear();

        // Clear edit mode
        self.clear_edit_mode();
    }

    /// Get current content (text and attachments)
    #[allow(dead_code)]
    pub fn get_content(&self, cx: &Context<Self>) -> (String, Vec<DraftAttachment>) {
        let text = self.text_input.read(cx).value().to_string();
        (text, self.attachments.clone())
    }

    /// Update agent state for button rendering
    pub fn set_agent_state(
        &mut self,
        agent_is_running: bool,
        cancel_enabled: bool,
        externally_locked: bool,
    ) {
        self.agent_is_running = agent_is_running;
        self.cancel_enabled = cancel_enabled;
        self.externally_locked = externally_locked;
    }

    /// Update the context usage ratio (0.0–1.0)
    pub fn set_context_usage_ratio(&mut self, ratio: Option<f32>) {
        self.context_usage_ratio = ratio;
    }
    /// Handle paste events (for images).
    ///
    /// Registered via `capture_action` so it fires during the capture phase
    /// (top-down), *before* the inner `Input` component sees the `Paste`
    /// action.  GPUI stops action propagation by default during the bubble
    /// phase, so a regular `on_action` would never reach us — the `Input`
    /// handles it first and the action stops there.  Using capture avoids
    /// this problem and the event still propagates to the `Input` for normal
    /// text pasting.
    fn on_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(clipboard_item) = cx.read_from_clipboard() {
            let prev_count = self.attachments.len();

            for entry in clipboard_item.into_entries() {
                if let ClipboardEntry::Image(image) = entry {
                    let attachment = DraftAttachment::Image {
                        content: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                        mime_type: image.format.mime_type().to_string(),
                        width: None,
                        height: None,
                    };

                    self.attachments.push(attachment);
                }
            }

            if self.attachments.len() > prev_count {
                self.rebuild_attachment_views(cx);
                self.emit_content_changed(cx);
                cx.notify();
            }
        }
    }

    /// Remove an attachment by index
    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.attachments.len() {
            self.attachments.remove(index);

            // Rebuild attachment views with updated indices
            self.rebuild_attachment_views(cx);

            // Emit content changed event
            self.emit_content_changed(cx);

            cx.notify();
        }
    }

    /// Rebuild attachment views when attachments change
    fn rebuild_attachment_views(&mut self, cx: &mut Context<Self>) {
        self.attachment_views.clear();

        for (index, attachment) in self.attachments.iter().enumerate() {
            let attachment_view = cx.new(|cx| AttachmentView::new(attachment.clone(), index, cx));

            // Subscribe to attachment events
            cx.subscribe(
                &attachment_view,
                |view, _attachment_view, event: &AttachmentEvent, cx| match event {
                    AttachmentEvent::Remove(index) => {
                        view.remove_attachment(*index, cx);
                    }
                },
            )
            .detach();

            self.attachment_views.push(attachment_view);
        }
    }

    /// Emit content changed event
    fn emit_content_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.text_input.read(cx).value().to_string();
        cx.emit(InputAreaEvent::ContentChanged {
            content: text,
            attachments: self.attachments.clone(),
        });
    }

    /// Handle text input events
    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                // Emit content changed event for draft saving
                self.emit_content_changed(cx);
            }
            InputEvent::Focus => {
                cx.emit(InputAreaEvent::FocusRequested);
            }
            InputEvent::Blur => {}

            InputEvent::PressEnter { secondary } => {
                // Only send message on plain ENTER (not with modifiers)
                if !secondary {
                    // Get current text (this might include the newline that was just added)
                    let current_text = self.text_input.read(cx).value().to_string();
                    // Remove trailing newline if present (from ENTER key press)
                    let cleaned_text = current_text.trim_end_matches('\n').to_string();

                    // Capture branch_parent_id before clearing
                    let branch_parent_id = self.branch_parent_id;

                    // FIRST: Clear draft before doing anything else
                    cx.emit(InputAreaEvent::ClearDraftRequested);

                    // Emit event for RootView to handle
                    cx.emit(InputAreaEvent::MessageSubmitted {
                        content: cleaned_text,
                        attachments: self.attachments.clone(),
                        branch_parent_id,
                    });

                    // Clear the input and attachments
                    self.clear(window, cx);
                }
                // If secondary is true, do nothing - modifiers will be handled by InsertLineBreak action
            }
        }
    }

    /// Handle model selector events
    fn on_model_selector_event(
        &mut self,
        _model_selector: &Entity<ModelSelector>,
        event: &ModelSelectorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ModelSelectorEvent::ModelChanged { model_name } => {
                self.current_model = Some(model_name.clone());
                cx.emit(InputAreaEvent::ModelChanged {
                    model_name: model_name.clone(),
                });
            }
        }
    }

    fn on_sandbox_selector_event(
        &mut self,
        _selector: &Entity<SandboxSelector>,
        event: &SandboxSelectorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SandboxSelectorEvent::PolicyChanged { policy } => {
                self.current_sandbox_policy = policy.clone();
                cx.emit(InputAreaEvent::SandboxChanged {
                    policy: policy.clone(),
                });
            }
        }
    }

    fn on_worktree_selector_event(
        &mut self,
        _selector: &Entity<WorktreeSelector>,
        event: &WorktreeSelectorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorktreeSelectorEvent::SwitchedToLocal => {
                cx.emit(InputAreaEvent::WorktreeSwitchedToLocal);
            }
            WorktreeSelectorEvent::SwitchedToWorktree {
                worktree_path,
                branch,
            } => {
                cx.emit(InputAreaEvent::WorktreeSwitched {
                    worktree_path: worktree_path.clone(),
                    branch: branch.clone(),
                });
            }
            WorktreeSelectorEvent::CreateNewWorktreeRequested => {
                cx.emit(InputAreaEvent::WorktreeCreateRequested);
            }
            WorktreeSelectorEvent::RefreshRequested => {
                cx.emit(InputAreaEvent::WorktreeRefreshRequested);
            }
        }
    }

    /// Get the worktree selector entity for external updates.
    pub fn worktree_selector(&self) -> &Entity<WorktreeSelector> {
        &self.worktree_selector
    }

    /// Handle submit button click
    fn on_submit_click(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.text_input.read(cx).value().to_string();

        if !content.trim().is_empty() || !self.attachments.is_empty() {
            // Capture branch_parent_id before clearing
            let branch_parent_id = self.branch_parent_id;

            // FIRST: Clear draft before doing anything else
            cx.emit(InputAreaEvent::ClearDraftRequested);

            // Emit event for RootView to handle
            cx.emit(InputAreaEvent::MessageSubmitted {
                content: content.clone(),
                attachments: self.attachments.clone(),
                branch_parent_id,
            });

            // Clear the input and attachments
            self.clear(window, cx);
        }
    }

    /// Handle cancel button click
    fn on_cancel_click(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputAreaEvent::CancelRequested);
    }

    /// Render the input area
    fn render_input_area(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_input_handle = self.text_input.read(cx).focus_handle(cx);
        let is_focused = text_input_handle.is_focused(window);
        let has_input_content = !self.text_input.read(cx).value().trim().is_empty();

        div()
            .id("input-area")
            .flex_none() // Important: don't grow or shrink
            .flex()
            .flex_col() // Column to accommodate attachments area
            .gap_0()
            // Edit mode banner - shows when editing an existing message
            .when(self.is_editing(), |parent| {
                parent.child(
                    div()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().warning.opacity(0.1))
                        .border_b_1()
                        .border_color(cx.theme().warning.opacity(0.3))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/pencil.svg"))
                                        .text_color(cx.theme().warning)
                                        .size(px(14.)),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().warning)
                                        .child("Editing message — this will create a new branch"),
                                ),
                        )
                        .child(
                            div()
                                .id("cancel-edit-btn")
                                .p_1()
                                .rounded_sm()
                                .cursor(CursorStyle::PointingHand)
                                .hover(|s| s.bg(cx.theme().warning.opacity(0.15)))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.cancel_edit(window, cx);
                                }))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/close.svg"))
                                        .text_color(cx.theme().warning)
                                        .size(px(14.)),
                                ),
                        ),
                )
            })
            // Externally locked banner
            .when(self.externally_locked, |parent| {
                parent.child(
                    div()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().warning.opacity(0.1))
                        .border_b_1()
                        .border_color(cx.theme().warning.opacity(0.3))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::default()
                                .path(SharedString::from("icons/lock.svg"))
                                .text_color(cx.theme().warning)
                                .size(px(14.)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().warning)
                                .child("Session is active in another instance"),
                        ),
                )
            })
            // Attachments area - show image previews when available
            .when(!self.attachments.is_empty(), |parent| {
                parent.child(
                    div()
                        .p_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .flex()
                        .flex_row()
                        .gap_2()
                        .flex_wrap()
                        .children(self.attachment_views.iter().cloned()),
                )
            })
            // Main input row: [text field + selectors] [buttons]
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .p_2()
                    .gap_2()
                    .when(self.externally_locked, |el| el.opacity(0.45))
                    // Left column: text field + selector row (share same width)
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child({
                                div()
                                    .bg(cx.theme().popover)
                                    .border(if is_focused { px(2.) } else { px(1.) })
                                    .p(if is_focused { px(0.) } else { px(1.) })
                                    .border_color(if is_focused {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().sidebar_border
                                    })
                                    .rounded_md()
                                    .track_focus(&text_input_handle)
                                    .child(Input::new(&self.text_input).appearance(false))
                            })
                            // Selector row: model | worktree | sandbox | context ring
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().flex().child(self.model_selector.clone()))
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .child(self.worktree_selector.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .child(self.sandbox_selector.clone()),
                                    )
                                    .child({
                                        let ratio = self.context_usage_ratio.unwrap_or(0.0);
                                        let progress_color = if ratio >= 0.8 {
                                            cx.theme().warning
                                        } else {
                                            cx.theme().muted_foreground
                                        };
                                        div()
                                            .id("context-indicator")
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .ml_1()
                                            .tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(format!(
                                                    "Context: {:.0}%",
                                                    ratio * 100.0
                                                ))
                                                .build(window, cx)
                                            })
                                            .child({
                                                let scale = cx.theme().font_size / px(16.0);
                                                super::context_indicator::ContextIndicator::new(
                                                    ratio,
                                                )
                                                .size(px(16.0 * scale))
                                                .stroke_width(px(2.5 * scale))
                                                .bg_color(cx.theme().muted_foreground.opacity(0.25))
                                                .progress_color(progress_color)
                                            })
                                    }),
                            ),
                    )
                    // Right column: buttons (vertically centered next to text field)
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .children({
                                let mut buttons = Vec::new();

                                // Send button - enabled when input has content and not externally locked
                                let send_enabled = has_input_content && !self.externally_locked;

                                let mut send_button = div()
                                    .id("send-btn")
                                    .size(px(40.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor(if send_enabled {
                                        CursorStyle::PointingHand
                                    } else {
                                        CursorStyle::OperationNotAllowed
                                    })
                                    .child(file_icons::render_icon(
                                        &file_icons::get().get_type_icon(file_icons::SEND),
                                        22.0,
                                        if send_enabled {
                                            cx.theme().primary
                                        } else {
                                            cx.theme().muted_foreground
                                        },
                                        ">",
                                    ));

                                if send_enabled {
                                    send_button = send_button
                                        .hover(|s| s.bg(cx.theme().muted))
                                        .on_click(cx.listener(Self::on_submit_click));
                                }
                                buttons.push(send_button);

                                // Cancel button
                                let mut cancel_button = div()
                                    .id("cancel-btn")
                                    .size(px(40.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor(if self.cancel_enabled {
                                        CursorStyle::PointingHand
                                    } else {
                                        CursorStyle::OperationNotAllowed
                                    })
                                    .child(file_icons::render_icon(
                                        &file_icons::get().get_type_icon(file_icons::STOP),
                                        22.0,
                                        if self.cancel_enabled {
                                            cx.theme().danger
                                        } else {
                                            cx.theme().muted_foreground
                                        },
                                        "⬜",
                                    ));

                                if self.cancel_enabled {
                                    cancel_button = cancel_button
                                        .hover(|s| s.bg(cx.theme().muted))
                                        .on_click(cx.listener(Self::on_cancel_click));
                                }

                                buttons.push(cancel_button);

                                buttons
                            }),
                    ),
            )
    }
}

impl Focusable for InputArea {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<InputAreaEvent> for InputArea {}
impl Render for InputArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .capture_action(cx.listener(Self::on_paste))
            .on_action({
                let text_input_handle = self.text_input.clone();
                move |_: &crate::ui::gpui::InsertLineBreak, window, cx| {
                    // Insert a line break at the current cursor position
                    text_input_handle.update(cx, |input_state, cx| {
                        input_state.insert("\n", window, cx);
                    });
                }
            })
            .track_focus(&self.focus_handle(cx))
            .child(self.render_input_area(window, cx))
    }
}
