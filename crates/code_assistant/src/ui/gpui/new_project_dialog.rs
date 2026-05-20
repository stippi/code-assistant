use gpui::{
    div, prelude::*, px, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, StyledExt};
use std::collections::HashMap;
use std::path::PathBuf;

/// Events emitted by the NewProjectDialog
#[derive(Clone, Debug)]
pub enum NewProjectDialogEvent {
    /// User confirmed project creation
    Confirmed { name: String, path: PathBuf },
    /// User cancelled the dialog
    Cancelled,
}

/// Validation state for the project name
#[derive(Clone, Debug, PartialEq)]
enum NameValidation {
    /// Name is valid and can be used
    Valid,
    /// Name is empty
    Empty,
    /// Exact duplicate: same name and same path — adding would be a no-op
    AlreadyExists,
    /// Name clash: same name but different path — must choose a different name
    NameClash { existing_path: String },
}

/// A modal dialog that asks for a project name after a folder has been selected.
pub struct NewProjectDialog {
    /// The selected folder path
    path: PathBuf,
    /// Input state for the project name field
    name_input: Entity<InputState>,
    /// Existing projects (name → path) for validation
    existing_projects: HashMap<String, PathBuf>,
    /// Current validation state
    validation: NameValidation,
    focus_handle: FocusHandle,
    _input_subscription: Subscription,
}

impl NewProjectDialog {
    pub fn new(
        path: PathBuf,
        existing_projects: HashMap<String, PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Derive default name from folder name
        let default_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string();

        let name_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Project name");
            state.set_value(SharedString::from(default_name.clone()), window, cx);
            state
        });

        let input_subscription = cx.subscribe_in(&name_input, window, Self::on_input_event);

        let validation = Self::validate_name(&default_name, &path, &existing_projects);

        Self {
            path,
            name_input,
            existing_projects,
            validation,
            focus_handle: cx.focus_handle(),
            _input_subscription: input_subscription,
        }
    }

    fn validate_name(
        name: &str,
        new_path: &PathBuf,
        existing_projects: &HashMap<String, PathBuf>,
    ) -> NameValidation {
        let name = name.trim();
        if name.is_empty() {
            return NameValidation::Empty;
        }

        if let Some(existing_path) = existing_projects.get(name) {
            // Compare paths (try canonical, fall back to direct comparison)
            let existing_canonical = existing_path.canonicalize().ok();
            let new_canonical = new_path.canonicalize().ok();
            let paths_match = match (&existing_canonical, &new_canonical) {
                (Some(a), Some(b)) => a == b,
                _ => existing_path == new_path,
            };

            if paths_match {
                NameValidation::AlreadyExists
            } else {
                NameValidation::NameClash {
                    existing_path: existing_path.display().to_string(),
                }
            }
        } else {
            NameValidation::Valid
        }
    }

    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { .. } => {
                self.confirm(cx);
            }
            InputEvent::Change => {
                let value = self.name_input.read(cx).value().to_string();
                self.validation = Self::validate_name(&value, &self.path, &self.existing_projects);
                cx.notify();
            }
            _ => {}
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.validation != NameValidation::Valid {
            return;
        }
        let name = self.name_input.read(cx).value().to_string();
        let name = name.trim().to_string();
        if !name.is_empty() {
            cx.emit(NewProjectDialogEvent::Confirmed {
                name,
                path: self.path.clone(),
            });
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(NewProjectDialogEvent::Cancelled);
    }

    fn can_submit(&self) -> bool {
        self.validation == NameValidation::Valid
    }
}

impl EventEmitter<NewProjectDialogEvent> for NewProjectDialog {}

impl Focusable for NewProjectDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NewProjectDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path_display = self.path.display().to_string();
        let can_submit = self.can_submit();

        // Build the hint text and color based on validation state
        let hint_element = match &self.validation {
            NameValidation::Valid | NameValidation::Empty => None,
            NameValidation::AlreadyExists => Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("This project is already configured."),
            ),
            NameValidation::NameClash { existing_path } => Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child("A different project with this name already exists:")
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger.opacity(0.8))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(existing_path.clone())),
                    ),
            ),
        };

        // Full-screen overlay with a backdrop
        div()
            .id("new-project-dialog-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background.opacity(0.6))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel(cx)),
            )
            .child(
                // Dialog card
                div()
                    .id("new-project-dialog")
                    .w(px(400.))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .shadow_lg()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    // Prevent backdrop click from closing when clicking inside dialog
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Title
                    .child(
                        div()
                            .text_base()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child("New Project"),
                    )
                    // Path display
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(path_display)),
                    )
                    // Name input
                    .child(div().child(Input::new(&self.name_input)))
                    // Validation hint (shown below input when relevant)
                    .when_some(hint_element, |this, hint| this.child(hint))
                    // Buttons row
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            // Cancel button
                            .child(
                                div()
                                    .id("dialog-cancel-btn")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Cancel"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                            )
                            // Create button (disabled when validation fails)
                            .child(
                                div()
                                    .id("dialog-create-btn")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .when(can_submit, |this| {
                                        this.cursor_pointer()
                                            .bg(cx.theme().primary)
                                            .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.confirm(cx)),
                                            )
                                    })
                                    .when(!can_submit, |this| {
                                        this.cursor_default().bg(cx.theme().muted).opacity(0.5)
                                    })
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(if can_submit {
                                                cx.theme().primary_foreground
                                            } else {
                                                cx.theme().muted_foreground
                                            })
                                            .child("Create"),
                                    ),
                            ),
                    ),
            )
    }
}
