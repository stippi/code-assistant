//! Self-update in the title bar: a chip while a newer release is available,
//! downloading or downloaded; a click downloads it or restarts into it
//! (asking first while sessions are working).

use super::MainScreen;
use crate::Gpui;
use code_assistant_core::updates::{UpdateState, UpdateStatus, UpdateStep};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{AnyElement, Context, Hsla, SharedString, div, prelude::*, px};
use tracing::warn;

/// What the chip offers.
enum Action {
    Download,
    Restart,
}

struct Chip {
    label: String,
    tooltip: String,
    icon: &'static str,
    failed: bool,
    action: Option<Action>,
}

/// The chip for `state`, or `None` when there is nothing to offer. Failed
/// checks show nothing here: offline is not worth a mark in the title bar.
fn chip(state: &UpdateState) -> Option<Chip> {
    let error = |step| {
        state
            .error
            .as_ref()
            .filter(|error| error.step == step)
            .map(|error| error.message.clone())
    };
    Some(match &state.status {
        UpdateStatus::Available { version } => {
            match error(UpdateStep::Download).or_else(|| error(UpdateStep::Install)) {
                Some(message) => Chip {
                    label: "Update failed".into(),
                    tooltip: format!("{message}\n\nClick to download {version} again."),
                    icon: "icons/circle_alert.svg",
                    failed: true,
                    action: Some(Action::Download),
                },
                None => Chip {
                    label: format!("Update {version}"),
                    tooltip: format!(
                        "Version {version} is available (this is {}). Click to download it.",
                        state.current_version
                    ),
                    icon: "icons/arrow_down.svg",
                    failed: false,
                    action: Some(Action::Download),
                },
            }
        }
        UpdateStatus::Downloading { version, .. } => Chip {
            label: format!("Downloading {}%", state.download_percent().unwrap_or(0)),
            tooltip: format!("Downloading version {version}"),
            icon: "icons/arrow_down.svg",
            failed: false,
            action: None,
        },
        UpdateStatus::Downloaded { version } => {
            let tooltip = match (error(UpdateStep::Install), &state.not_applied) {
                (Some(message), _) => format!("Installing {version} failed: {message}"),
                (None, Some(not_applied)) if not_applied == version => {
                    format!("The update to {version} did not apply. Click to try again.")
                }
                _ => format!("Version {version} is downloaded. Click to restart into it."),
            };
            Chip {
                label: "Restart to update".into(),
                tooltip,
                icon: "icons/refresh_cw.svg",
                failed: error(UpdateStep::Install).is_some(),
                action: Some(Action::Restart),
            }
        }
        UpdateStatus::Installing { version } => Chip {
            label: "Installing…".into(),
            tooltip: format!("Installing version {version}"),
            icon: "icons/refresh_cw.svg",
            failed: false,
            action: None,
        },
        _ => return None,
    })
}

impl MainScreen {
    /// The title-bar chip, or `None` when no update is in sight.
    pub(super) fn render_update_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chip = chip(&cx.try_global::<Gpui>()?.update_state()?)?;
        let color: Hsla = if chip.failed {
            cx.theme().danger
        } else {
            cx.theme().primary
        };
        let tooltip = SharedString::from(chip.tooltip);
        let clickable = chip.action.is_some();
        Some(
            div()
                .id("update-chip")
                .mr_1()
                .h(px(24.))
                .px_2()
                .rounded_md()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(color)
                .border_1()
                .border_color(color.opacity(0.4))
                .when(clickable, |el| {
                    el.cursor_pointer().hover(|s| s.bg(cx.theme().muted))
                })
                .when(self.update_confirm_open, |el| el.bg(cx.theme().muted))
                .child(
                    Icon::default()
                        .path(SharedString::from(chip.icon))
                        .with_size(Size::XSmall)
                        .text_color(color),
                )
                .child(SharedString::from(chip.label))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .when_some(chip.action, |el, action| {
                    el.on_click(cx.listener(move |this, _, _, cx| match action {
                        Action::Download => {
                            if let Some(gpui) = cx.try_global::<Gpui>() {
                                gpui.cmd_download_update();
                            }
                        }
                        Action::Restart => {
                            if this.project_sidebar.read(cx).working_session_count() > 0 {
                                this.update_confirm_open = !this.update_confirm_open;
                                cx.notify();
                            } else {
                                this.restart_to_update(cx);
                            }
                        }
                    }))
                })
                .into_any_element(),
        )
    }

    /// Below the chip: restarting stops the working sessions — go ahead?
    pub(super) fn render_update_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.update_confirm_open {
            return None;
        }
        let working = self.project_sidebar.read(cx).working_session_count();
        let message = match working {
            0 => "No session is working anymore.".to_string(),
            1 => "A session is working. Restarting stops it; its conversation is kept.".to_string(),
            n => format!(
                "{n} sessions are working. Restarting stops them; their conversations are kept."
            ),
        };
        Some(
            div()
                .absolute()
                .top(px(52.))
                .right(px(16.))
                .w(px(320.))
                .p_3()
                .flex()
                .flex_col()
                .gap_3()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .shadow_lg()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(SharedString::from(message)),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("update-confirm-cancel")
                                .label("Cancel")
                                .small()
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_confirm_open = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("update-confirm-restart")
                                .label("Restart now")
                                .small()
                                .primary()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.restart_to_update(cx);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Swaps the downloaded release in and restarts into it. A failure
    /// shows on the chip (the state carries it).
    fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        self.update_confirm_open = false;
        cx.notify();
        let Some(service) = cx.try_global::<Gpui>().and_then(Gpui::update_service) else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let installed = cx
                .background_spawn(async move { service.install().await })
                .await;
            match installed {
                Ok(launch) => cx.update(|cx| {
                    cx.set_restart_path(launch);
                    cx.restart();
                }),
                Err(e) => warn!("Update not installed: {e:#}"),
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::updates::{UpdateError, Version};

    fn state(status: UpdateStatus) -> UpdateState {
        UpdateState::new(Version::new(0, 2, 18), status)
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn shows_nothing_without_a_newer_release() {
        for status in [
            UpdateStatus::Idle,
            UpdateStatus::Checking,
            UpdateStatus::UpToDate,
            UpdateStatus::Disabled {
                reason: "dev build".into(),
            },
        ] {
            assert!(chip(&state(status)).is_none());
        }
    }

    #[test]
    fn a_failed_check_leaves_the_offer_alone() {
        let mut available = state(UpdateStatus::Available {
            version: v("0.2.19"),
        });
        available.error = Some(UpdateError {
            step: UpdateStep::Check,
            message: "offline".into(),
        });
        let chip = chip(&available).unwrap();
        assert_eq!(chip.label, "Update 0.2.19");
        assert!(!chip.failed);
    }

    #[test]
    fn a_failed_download_offers_a_retry() {
        let mut available = state(UpdateStatus::Available {
            version: v("0.2.19"),
        });
        available.error = Some(UpdateError {
            step: UpdateStep::Download,
            message: "connection reset".into(),
        });
        let chip = chip(&available).unwrap();
        assert!(chip.failed);
        assert!(matches!(chip.action, Some(Action::Download)));
        assert!(chip.tooltip.contains("connection reset"));
    }

    #[test]
    fn downloading_shows_progress_and_takes_no_click() {
        let chip = chip(&state(UpdateStatus::Downloading {
            version: v("0.2.19"),
            received: 42,
            total: 100,
        }))
        .unwrap();
        assert_eq!(chip.label, "Downloading 42%");
        assert!(chip.action.is_none());
    }

    #[test]
    fn a_download_that_did_not_apply_says_so() {
        let mut downloaded = state(UpdateStatus::Downloaded {
            version: v("0.2.19"),
        });
        downloaded.not_applied = Some(v("0.2.19"));
        let chip = chip(&downloaded).unwrap();
        assert!(matches!(chip.action, Some(Action::Restart)));
        assert!(chip.tooltip.contains("did not apply"), "{}", chip.tooltip);
    }
}
