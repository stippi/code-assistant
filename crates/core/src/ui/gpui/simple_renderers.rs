use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, hsla, px, white, IntoElement, ParentElement, Styled};

/// Renderer for parameters that shouldn't show their parameter name
pub struct SimpleParameterRenderer {
    /// The list of tool+parameter combinations that should use this renderer
    supported_combinations: Vec<(String, String)>,
    /// Whether this parameter should be rendered with full width
    full_width: bool,
}

impl SimpleParameterRenderer {
    /// Create a new simple parameter renderer with specified combinations
    pub fn new(combinations: Vec<(String, String)>, full_width: bool) -> Self {
        Self {
            supported_combinations: combinations,
            full_width,
        }
    }
}

impl ParameterRenderer for SimpleParameterRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        self.supported_combinations.clone()
    }

    fn render(&self, _tool_name: &str, _param_name: &str, param_value: &str) -> gpui::AnyElement {
        div()
            .rounded_md()
            .px_2()
            .py_1()
            .mr_1()
            .mb_1() // Add margin to allow wrapping
            .text_size(px(15.))
            .bg(hsla(210., 0.1, 0.3, 0.3))
            .text_color(white())
            .child(param_value.to_string())
            .into_any_element()
    }

    fn is_full_width(&self, _tool_name: &str, _param_name: &str) -> bool {
        self.full_width
    }
}
