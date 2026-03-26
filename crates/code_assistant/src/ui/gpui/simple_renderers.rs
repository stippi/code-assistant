use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, px, IntoElement, ParentElement, SharedString, Styled, TextOverflow};

/// Renderer for parameters that shouldn't show their parameter name
pub struct SimpleParameterRenderer {
    /// The list of tool+parameter combinations that should use this renderer
    supported_combinations: Vec<(String, String)>,
    /// Whether this parameter should be rendered with full width
    full_width: bool,
    /// Optional: if set, the parameter is full-width when value length exceeds this threshold
    full_width_threshold: Option<usize>,
}

impl SimpleParameterRenderer {
    /// Create a new simple parameter renderer with specified combinations
    pub fn new(combinations: Vec<(String, String)>, full_width: bool) -> Self {
        Self {
            supported_combinations: combinations,
            full_width,
            full_width_threshold: None,
        }
    }

    /// Create a renderer where full-width is determined dynamically by value length.
    /// Values shorter than `threshold` characters render inline; longer ones full-width.
    pub fn with_dynamic_width(combinations: Vec<(String, String)>, threshold: usize) -> Self {
        Self {
            supported_combinations: combinations,
            full_width: false,
            full_width_threshold: Some(threshold),
        }
    }
}

impl ParameterRenderer for SimpleParameterRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        self.supported_combinations.clone()
    }

    fn render(
        &self,
        _tool_name: &str,
        _param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        let is_fw = self.is_full_width(_tool_name, _param_name, param_value);

        let mut el = div()
            .rounded_md()
            .px_2()
            .py_1()
            .text_size(px(13.))
            .bg(crate::ui::gpui::theme::colors::tool_parameter_bg(theme))
            .text_color(crate::ui::gpui::theme::colors::tool_parameter_value(theme));

        if !is_fw {
            // Inline: truncate with ellipsis
            el = el.text_overflow(TextOverflow::Truncate(SharedString::from("...")));
        }

        el.child(param_value.to_string()).into_any_element()
    }

    fn is_full_width(&self, _tool_name: &str, _param_name: &str, param_value: &str) -> bool {
        if let Some(threshold) = self.full_width_threshold {
            return param_value.len() > threshold;
        }
        self.full_width
    }
}
