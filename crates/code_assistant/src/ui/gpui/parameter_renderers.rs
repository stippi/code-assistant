use gpui::{px, Element, IntoElement, ParentElement, SharedString, Styled};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::warn;

/// A unique key for tool+parameter combinations
pub type ParameterKey = String;

/// Helper function to create a unique key for a tool-parameter combination
pub fn create_parameter_key(tool_name: &str, param_name: &str) -> ParameterKey {
    format!("{tool_name}:{param_name}")
}

/// Specification for a virtual parameter that combines multiple actual parameters
#[derive(Clone, Debug)]
pub struct VirtualParameterSpec {
    pub virtual_name: String,
    pub source_params: Vec<String>,
    pub completion_strategy: VirtualParameterCompletionStrategy,
}

/// Strategy for when to show virtual parameters vs individual source parameters
#[derive(Clone, Debug)]
pub enum VirtualParameterCompletionStrategy {
    /// Show individual parameters during streaming, switch to virtual when all sources present
    StreamIndividualThenCombine,
    /// Wait for EndTool event before showing virtual parameter
    WaitForToolCompletion,
    /// Show virtual parameter as soon as any source parameter arrives
    ShowImmediately,
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

    /// Define virtual parameters that combine multiple actual parameters
    /// Returns: Vec<VirtualParameterSpec> defining how to combine parameters
    fn virtual_parameters(&self) -> Vec<VirtualParameterSpec> {
        Vec::new()
    }

    /// Render a virtual parameter from collected source parameters
    /// Returns Some(element) if this renderer can handle the virtual parameter, None otherwise
    fn render_virtual_parameter(
        &self,
        _tool_name: &str,
        _virtual_param_name: &str,
        _source_params: &HashMap<String, String>,
        _theme: &gpui_component::theme::Theme,
    ) -> Option<gpui::AnyElement> {
        None
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

    /// Get virtual parameters that should be rendered for a tool
    pub fn get_virtual_parameters_for_tool(&self, tool_name: &str) -> Vec<VirtualParameterSpec> {
        use std::collections::HashSet;

        let mut seen_virtual_params = HashSet::new();
        let specs: Vec<VirtualParameterSpec> = self.renderers
            .values()
            .flat_map(|renderer| renderer.virtual_parameters())
            .filter(|spec| {
                // Only include if this renderer supports the tool for any of the source parameters
                spec.source_params.iter().any(|param| {
                    let key = create_parameter_key(tool_name, param);
                    self.renderers.contains_key(&key)
                })
            })
            .filter(|spec| {
                // Deduplicate by virtual parameter name to avoid multiple instances
                let virtual_key = format!("{}:{}", tool_name, spec.virtual_name);
                if seen_virtual_params.contains(&virtual_key) {
                    false
                } else {
                    seen_virtual_params.insert(virtual_key);
                    true
                }
            })
            .collect();

        specs
    }

    /// Check if parameters should be hidden due to virtual parameter handling
    pub fn should_hide_parameter(
        &self,
        _tool_name: &str,
        param_name: &str,
        all_params: &HashMap<String, String>,
        tool_completed: bool,
    ) -> bool {
        for renderer in self.renderers.values() {
            for virtual_spec in renderer.virtual_parameters() {
                if virtual_spec.source_params.contains(&param_name.to_string()) {
                    return match virtual_spec.completion_strategy {
                        VirtualParameterCompletionStrategy::StreamIndividualThenCombine => {
                            // Hide if all source params present OR tool completed
                            virtual_spec
                                .source_params
                                .iter()
                                .all(|p| all_params.contains_key(p))
                                || tool_completed
                        }
                        VirtualParameterCompletionStrategy::WaitForToolCompletion => tool_completed,
                        VirtualParameterCompletionStrategy::ShowImmediately => {
                            true // Always hide source params
                        }
                    };
                }
            }
        }
        false
    }

    /// Render virtual parameters for a tool given the current parameter state
    pub fn render_virtual_parameters(
        &self,
        tool_name: &str,
        all_params: &HashMap<String, String>,
        tool_completed: bool,
        theme: &gpui_component::theme::Theme,
    ) -> Vec<gpui::AnyElement> {
        let mut virtual_elements = Vec::new();

        for virtual_spec in self.get_virtual_parameters_for_tool(tool_name) {
            // Check completion strategy to see if we should render this virtual parameter
            let should_render = match virtual_spec.completion_strategy {
                VirtualParameterCompletionStrategy::StreamIndividualThenCombine => {
                    virtual_spec
                        .source_params
                        .iter()
                        .all(|p| all_params.contains_key(p))
                        || tool_completed
                }
                VirtualParameterCompletionStrategy::WaitForToolCompletion => tool_completed,
                VirtualParameterCompletionStrategy::ShowImmediately => virtual_spec
                    .source_params
                    .iter()
                    .any(|p| all_params.contains_key(p)),
            };

            if should_render {
                // Find renderer that can handle this virtual parameter
                for renderer_arc in self.renderers.values() {
                    if let Some(element) = renderer_arc.render_virtual_parameter(
                        tool_name,
                        &virtual_spec.virtual_name,
                        all_params,
                        theme,
                    ) {
                        virtual_elements.push(element);
                        break; // Only use the first renderer that can handle it
                    }
                }
            }
        }

        virtual_elements
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
            .max_w_full()
            .rounded_md()
            .px_2()
            .py_1()
            .text_size(px(13.))
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
                            .child(format!("{param_name}:"))
                            .into_any(),
                        div()
                            .text_color(crate::ui::gpui::theme::colors::tool_parameter_value(theme))
                            .text_overflow(gpui::TextOverflow::Truncate(SharedString::from("...")))
                            .flex_shrink() // Allow this div to shrink below its content size
                            .min_w_0() // Allow shrinking below content width
                            .child(param_value.to_string())
                            .into_any(),
                    ]),
            )
            .into_any_element()
    }
}
