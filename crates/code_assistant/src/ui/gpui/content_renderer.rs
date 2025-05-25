use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, px, rgba, Element, FontWeight, ParentElement, Styled};

/// Renderer for the "content" parameter of the "write_file" tool
pub struct ContentRenderer;

impl ParameterRenderer for ContentRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        vec![("write_file".to_string(), "content".to_string())]
    }

    fn render(
        &self,
        _tool_name: &str,
        _param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        // Container for the content - no parameter name shown
        div()
            .rounded_md()
            .bg(if theme.is_dark() {
                rgba(0x0A0A0AFF) // Darker background in Dark Mode
            } else {
                rgba(0xEAEAEAFF) // Lighter background in Light Mode
            })
            .p_2()
            .text_size(px(15.))
            .font_weight(FontWeight(500.0))
            .child(param_value.to_string())
            .into_any()
    }

    fn is_full_width(&self, _tool_name: &str, _param_name: &str) -> bool {
        true // Content parameter is always full-width
    }
}
