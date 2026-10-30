//! A modal "About" dialog showing goreleaser-style build/version information
//! (version, git commit + tree state, build timestamp, Rust toolchain and
//! target triple). Modeled on [`super::project_dialog::NewProjectDialog`].

use crate::Gpui;
use code_assistant_core::updates::{UpdateState, UpdateStatus, UpdateStep};
use code_assistant_core::version;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, StyledExt};
use gpui_kit::{
    ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, SharedString, Window, div,
    prelude::*, px,
};

/// Events emitted by the [`AboutDialog`].
#[derive(Clone, Debug)]
pub enum AboutDialogEvent {
    /// User closed the dialog.
    Closed,
}

pub struct AboutDialog {
    focus_handle: FocusHandle,
}

impl AboutDialog {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(AboutDialogEvent::Closed);
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(version::long_with_name()));
    }

    /// A single label/value info row.
    fn info_row(label: &str, value: String, cx: &Context<Self>) -> gpui_kit::AnyElement {
        div()
            .flex()
            .flex_row()
            .gap_2()
            .items_baseline()
            .child(
                div()
                    .flex_none()
                    .w(px(64.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(value)),
            )
            .into_any_element()
    }
}

/// One line on where the self-update stands.
fn update_summary(state: &UpdateState) -> String {
    let status = match &state.status {
        UpdateStatus::Disabled { reason } => return reason.clone(),
        UpdateStatus::Idle => "not checked yet".to_string(),
        UpdateStatus::Checking => "checking…".to_string(),
        UpdateStatus::UpToDate => "up to date".to_string(),
        UpdateStatus::Available { version } => format!("{version} available"),
        UpdateStatus::Downloading { version, .. } => format!(
            "downloading {version} ({}%)",
            state.download_percent().unwrap_or(0)
        ),
        UpdateStatus::Downloaded { version } => {
            format!("{version} downloaded, restart to update")
        }
        UpdateStatus::Installing { version } => format!("installing {version}…"),
    };
    let checked = state
        .checked_at
        .map(|at| format!(", checked {}", at.format("%H:%M")))
        .unwrap_or_default();
    match &state.error {
        Some(error) if error.step == UpdateStep::Check => {
            format!(
                "{status}{checked}; the last check failed: {}",
                error.message
            )
        }
        _ => format!("{status}{checked}"),
    }
}

impl EventEmitter<AboutDialogEvent> for AboutDialog {}

impl Focusable for AboutDialog {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AboutDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let update_state = cx.try_global::<Gpui>().and_then(Gpui::update_state);
        let can_check = update_state.as_ref().is_some_and(|state| {
            !matches!(
                state.status,
                UpdateStatus::Disabled { .. }
                    | UpdateStatus::Checking
                    | UpdateStatus::Downloading { .. }
                    | UpdateStatus::Installing { .. }
            )
        });
        // Full-screen overlay with a backdrop
        div()
            .id("about-dialog-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background.opacity(0.6))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close(cx)),
            )
            .child(
                // Dialog card
                div()
                    .id("about-dialog")
                    .w(px(460.))
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
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Header: icon + app name + version summary
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/info.svg"))
                                    .with_size(Size::Medium)
                                    .text_color(cx.theme().primary),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_medium()
                                            .text_color(cx.theme().foreground)
                                            .child("Code Assistant"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from(format!(
                                                "v{} ({})",
                                                version::VERSION,
                                                version::build_profile()
                                            ))),
                                    ),
                            ),
                    )
                    // Info rows
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(Self::info_row(
                                "commit",
                                format!("{} ({})", version::GIT_COMMIT, version::git_tree_label()),
                                cx,
                            ))
                            .child(Self::info_row(
                                "built",
                                version::BUILD_TIMESTAMP.to_string(),
                                cx,
                            ))
                            .child(Self::info_row(
                                "rust",
                                format!("{} {}", version::RUSTC_VERSION, version::TARGET),
                                cx,
                            ))
                            .when_some(update_state.as_ref(), |el, state| {
                                el.child(Self::info_row("updates", update_summary(state), cx))
                            }),
                    )
                    // Buttons row
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .when(can_check, |el| {
                                el.child(
                                    div()
                                        .id("about-check-updates-btn")
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
                                                .child("Check for updates"),
                                        )
                                        .on_click(|_, _, cx| {
                                            if let Some(gpui) = cx.try_global::<Gpui>() {
                                                gpui.cmd_check_for_update();
                                            }
                                        }),
                                )
                            })
                            // Copy button
                            .child(
                                div()
                                    .id("about-copy-btn")
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
                                            .child("Copy"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                            )
                            // Close button
                            .child(
                                div()
                                    .id("about-close-btn")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(cx.theme().primary)
                                    .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().primary_foreground)
                                            .child("Close"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::updates::{UpdateError, Version};

    #[test]
    fn summarizes_the_update_state() {
        let mut state = UpdateState::new(Version::new(0, 2, 18), UpdateStatus::UpToDate);
        assert_eq!(update_summary(&state), "up to date");

        state.status = UpdateStatus::Available {
            version: Version::new(0, 2, 19),
        };
        state.error = Some(UpdateError {
            step: UpdateStep::Check,
            message: "offline".into(),
        });
        assert_eq!(
            update_summary(&state),
            "0.2.19 available; the last check failed: offline"
        );

        state.status = UpdateStatus::Disabled {
            reason: "Not a release build.".into(),
        };
        assert_eq!(update_summary(&state), "Not a release build.");
    }
}
