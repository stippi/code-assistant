//! General settings section — theme, scale, and other global preferences.

use gpui::{div, prelude::*, px, App, Context, FocusHandle, Focusable, SharedString};
use gpui_component::ActiveTheme;

pub struct GeneralSection {
    focus_handle: FocusHandle,
}

impl GeneralSection {
    pub fn new(_window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for GeneralSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GeneralSection {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .font_weight(gpui::FontWeight::SEMIBOLD)
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
                            .child("Configuration files are stored in ~/.config/code-assistant/"),
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
                            .font_weight(gpui::FontWeight::MEDIUM)
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
