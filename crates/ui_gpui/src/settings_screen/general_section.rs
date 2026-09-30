//! General settings section — theme, scale, and other global preferences.

use code_assistant_core::session::idle_handoff::HandoffConfig;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, div, prelude::*, px,
};
use tracing::warn;

pub struct GeneralSection {
    focus_handle: FocusHandle,
    /// Input tokens of a session's last request from which an idle session
    /// gets a prepared hand-off; stored in `hand-off.json`.
    handoff_threshold_input: Entity<InputState>,
    _handoff_threshold_subscription: Subscription,
}

impl GeneralSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let threshold = HandoffConfig::load().idle_threshold_tokens;
        let handoff_threshold_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("150000")
                .default_value(threshold.to_string())
        });
        let subscription = cx.subscribe_in(
            &handoff_threshold_input,
            window,
            Self::on_handoff_threshold_input,
        );
        Self {
            focus_handle: cx.focus_handle(),
            handoff_threshold_input,
            _handoff_threshold_subscription: subscription,
        }
    }

    /// Save the threshold whenever the input holds a valid number.
    fn on_handoff_threshold_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Ok(idle_threshold_tokens) = input.read(cx).value().trim().parse::<u64>() else {
            return;
        };
        let config = HandoffConfig {
            idle_threshold_tokens,
        };
        if let Err(e) = config.save() {
            warn!("Failed to save the hand-off settings: {e:#}");
        }
    }
}

impl Focusable for GeneralSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GeneralSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(700.))
            // Header
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("GENERAL"),
            )
            // Info
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("Theme and zoom controls are available in the titlebar."),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Configuration files are stored in {}",
                                code_assistant_core::config_dir::config_dir().display()
                            )),
                    ),
            )
            // Config paths
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Configuration Files"),
                    )
                    .child(Self::render_config_path(
                        "Providers",
                        &llm::provider_config::ConfigurationSystem::providers_config_path()
                            .display()
                            .to_string(),
                        cx,
                    ))
                    .child(Self::render_config_path(
                        "Models",
                        &llm::provider_config::ConfigurationSystem::models_config_path()
                            .display()
                            .to_string(),
                        cx,
                    )),
            )
            // Prepared hand-off
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Hand-off when idle"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(160.))
                                    .child(Input::new(&self.handoff_threshold_input)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child("input tokens"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "When a session whose last request had at least this many \
                                 input tokens stays idle for two minutes, the agent prepares \
                                 a /new prompt for a follow-up session while the prompt cache \
                                 is still warm. 0 turns this off.",
                            ),
                    ),
            )
    }
}

impl GeneralSection {
    fn render_config_path(label: &str, path: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(80.))
                    .child(SharedString::from(format!("{}:", label))),
            )
            .child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(cx.theme().foreground)
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(cx.theme().muted)
                    .child(SharedString::from(path.to_string())),
            )
    }
}
