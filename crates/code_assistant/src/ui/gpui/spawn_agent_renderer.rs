//! Custom parameter renderer for spawn_agent tool instructions

use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, px, FontWeight, IntoElement, ParentElement, Styled};

/// Renderer for spawn_agent instructions parameter
/// Renders as full-width with a modest heading
pub struct SpawnAgentInstructionsRenderer;

impl ParameterRenderer for SpawnAgentInstructionsRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        vec![("spawn_agent".to_string(), "instructions".to_string())]
    }

    fn render(
        &self,
        _tool_name: &str,
        _param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        div()
            .w_full()
            .rounded_md()
            .px_2()
            .py_1()
            .text_size(px(13.))
            .bg(crate::ui::gpui::theme::colors::tool_parameter_bg(theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight(500.0))
                            .text_size(px(12.))
                            .text_color(crate::ui::gpui::theme::colors::tool_parameter_label(theme))
                            .child("Instructions:"),
                    )
                    .child(
                        div()
                            .text_color(crate::ui::gpui::theme::colors::tool_parameter_value(theme))
                            .child(param_value.to_string()),
                    ),
            )
            .into_any_element()
    }

    fn is_full_width(&self, _tool_name: &str, _param_name: &str) -> bool {
        true
    }
}
