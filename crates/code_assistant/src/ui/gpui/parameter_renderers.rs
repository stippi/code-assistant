use gpui::{px, Element, IntoElement, ParentElement, Styled};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::warn;

/// A unique key for tool+parameter combinations
pub type ParameterKey = String;

/// Helper function to create a unique key for a tool-parameter combination
pub fn create_parameter_key(tool_name: &str, param_name: &str) -> ParameterKey {
    format!("{}:{}", tool_name, param_name)
}

/// Trait for parameter renderers that can provide custom rendering for tool parameters
pub trait ParameterRenderer: Send + Sync {
    /// List of supported tool+parameter combinations
    fn supported_parameters(&self) -> Vec<(String, String)>;

    /// Render the parameter as a UI element
    fn render(
        &self,
        tool_name: &str,
        param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement;

    /// Indicates if this parameter should be rendered with full width
    /// Default is false (normal inline parameter)
    fn is_full_width(&self, _tool_name: &str, _param_name: &str) -> bool {
        false
    }
}

/// Registry for parameter renderers
pub struct ParameterRendererRegistry {
    // Map from tool+parameter key to renderer
    renderers: HashMap<ParameterKey, Arc<Box<dyn ParameterRenderer>>>,
    // Default renderer for parameters with no specific renderer
    default_renderer: Arc<Box<dyn ParameterRenderer>>,
}

// Global registry singleton using OnceLock (thread-safe)
static GLOBAL_REGISTRY: OnceLock<Mutex<Option<Arc<ParameterRendererRegistry>>>> = OnceLock::new();

impl ParameterRendererRegistry {
    // Set the global registry
    pub fn set_global(registry: Arc<ParameterRendererRegistry>) {
        // Initialize the global mutex if not already initialized
        let global_mutex = GLOBAL_REGISTRY.get_or_init(|| Mutex::new(None));

        // Set the registry instance
        if let Ok(mut guard) = global_mutex.lock() {
            *guard = Some(registry);
        } else {
            warn!("Failed to acquire lock for setting global registry");
        }
    }

    // Get a reference to the global registry
    pub fn global() -> Option<Arc<ParameterRendererRegistry>> {
        if let Some(global_mutex) = GLOBAL_REGISTRY.get() {
            if let Ok(guard) = global_mutex.lock() {
                return guard.clone();
            }
        }
        None
    }

    /// Create a new registry with the given default renderer
    pub fn new(default_renderer: Box<dyn ParameterRenderer>) -> Self {
        Self {
            renderers: HashMap::new(),
            default_renderer: Arc::new(default_renderer),
        }
    }

    /// Register a new renderer for its supported parameters
    pub fn register_renderer(&mut self, renderer: Box<dyn ParameterRenderer>) {
        let renderer_arc = Arc::new(renderer);

        for (tool_name, param_name) in renderer_arc.supported_parameters() {
            let key = create_parameter_key(&tool_name, &param_name);
            if self.renderers.contains_key(&key) {
                warn!("Overriding existing renderer for {}", key);
            }
            self.renderers.insert(key, renderer_arc.clone());
        }
    }

    /// Get the appropriate renderer for a tool+parameter combination
    pub fn get_renderer(
        &self,
        tool_name: &str,
        param_name: &str,
    ) -> Arc<Box<dyn ParameterRenderer>> {
        let key = create_parameter_key(tool_name, param_name);

        self.renderers
            .get(&key)
            .unwrap_or(&self.default_renderer)
            .clone()
    }

    /// Render a parameter using the appropriate renderer
    pub fn render_parameter(
        &self,
        tool_name: &str,
        param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        let renderer = self.get_renderer(tool_name, param_name);
        renderer.render(tool_name, param_name, param_value, theme)
    }
}

/// Default parameter renderer that displays parameters in a simple badge format
pub struct DefaultParameterRenderer;

impl ParameterRenderer for DefaultParameterRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        // Default renderer supports no specific parameters
        Vec::new()
    }

    fn render(
        &self,
        _tool_name: &str,
        param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        use gpui::{div, FontWeight};

        div()
            .rounded_md()
            .px_2()
            .py_1()
            .mr_1()
            .mb_1() // Add margin to allow wrapping
            .text_size(px(15.))
            .bg(crate::ui::gpui::theme::colors::tool_parameter_bg(theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .children(vec![
                        div()
                            .font_weight(FontWeight(500.0))
                            .text_color(crate::ui::gpui::theme::colors::tool_parameter_label(theme))
                            .child(format!("{}:", param_name))
                            .into_any(),
                        div()
                            .text_color(crate::ui::gpui::theme::colors::tool_parameter_value(theme))
                            .child(param_value.to_string())
                            .into_any(),
                    ]),
            )
            .into_any_element()
    }
}
