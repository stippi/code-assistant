//! Unified tool block rendering system.
//!
//! Replaces the two-level plugin system (`ParameterRendererRegistry` +
//! `ToolOutputRendererRegistry`) with a single `ToolBlockRenderer` trait that
//! controls the **entire** rendering of a tool block.
//!
//! ## Two rendering modes
//!
//! * **Inline** — minimal single-line rendering for exploration/read-only tools
//!   (e.g. `read_files`, `search_files`).  Always starts collapsed; chevron
//!   appears on hover; on expand the output is shown below with a subtle left
//!   border.
//!
//! * **Card** — bordered card with header, body, and optional footer for tools
//!   with meaningful visual output (e.g. `execute_command`, `edit`).

use crate::ui::gpui::elements::{BlockView, ToolUseBlock};
use crate::ui::ToolStatus;
use gpui::{px, AnyElement, Context, Element, Pixels, Window};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// CardRenderContext — passed to Card-style renderers
// ---------------------------------------------------------------------------

/// Animation and layout state passed from `BlockView` to card renderers.
///
/// Card renderers use this to:
/// - read the current collapse/expand animation progress (`animation_scale`)
/// - share a persistent height measurement cell with the layout engine
///   (`content_height`) so the animated wrapper can constrain the body height
///   across frames
pub struct CardRenderContext {
    /// Current animation scale: 0.0 = fully collapsed, 1.0 = fully expanded.
    /// Intermediate values occur during the ease-out animation.
    pub animation_scale: f32,
    /// Whether the tool block is logically collapsed (target state).
    pub is_collapsed: bool,
    /// Persistent height cell shared with `BlockView`.  The card renderer
    /// should use this in its animated body wrapper (via
    /// `on_children_prepainted`) so the measured height survives across frames.
    pub content_height: Rc<Cell<Pixels>>,
    /// The session's current/default project name.  Card renderers can compare
    /// this against the tool's `project` parameter to decide whether to show it.
    pub current_project: String,
}

// ---------------------------------------------------------------------------
// ToolBlockStyle
// ---------------------------------------------------------------------------

/// How a tool block should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolBlockStyle {
    /// Minimal inline rendering — icon + description text.
    Inline,
    /// Full card with border, header, body.
    Card,
}

// ---------------------------------------------------------------------------
// ToolBlockRenderer trait
// ---------------------------------------------------------------------------

/// Controls the complete rendering of a tool block.
pub trait ToolBlockRenderer: Send + Sync {
    /// Which tools this renderer handles.
    fn supported_tools(&self) -> Vec<String>;

    /// Whether this tool renders as inline or card.
    fn style(&self) -> ToolBlockStyle;

    /// Generate a one-line description from parameters (for inline tools).
    fn describe(&self, tool: &ToolUseBlock) -> String {
        tool.name.clone()
    }

    /// Render the tool block content.
    ///
    /// For **Inline** renderers this returns the expanded output area
    /// (the single-line description + collapse chrome is handled by the
    /// caller in `elements.rs`).  `card_ctx` is `None` for inline tools.
    ///
    /// For **Card** renderers this returns the complete card element.
    /// `card_ctx` carries the current animation scale and a persistent
    /// height cell for smooth collapse/expand transitions.
    fn render(
        &self,
        tool: &ToolUseBlock,
        is_generating: bool,
        theme: &gpui_component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<AnyElement>;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Global registry mapping tool names → renderers.
pub struct ToolBlockRendererRegistry {
    renderers: HashMap<String, Arc<dyn ToolBlockRenderer>>,
}

static GLOBAL_REGISTRY: OnceLock<Mutex<Option<Arc<ToolBlockRendererRegistry>>>> = OnceLock::new();

impl ToolBlockRendererRegistry {
    pub fn new() -> Self {
        Self {
            renderers: HashMap::new(),
        }
    }

    /// Register a renderer for all tools it declares.
    pub fn register(&mut self, renderer: Arc<dyn ToolBlockRenderer>) {
        for tool_name in renderer.supported_tools() {
            self.renderers.insert(tool_name, renderer.clone());
        }
    }

    /// Look up the renderer for a tool.  Returns `None` if no renderer is
    /// registered (fall back to existing rendering).
    pub fn get(&self, tool_name: &str) -> Option<&Arc<dyn ToolBlockRenderer>> {
        self.renderers.get(tool_name)
    }

    // -- global singleton --

    pub fn set_global(registry: Arc<ToolBlockRendererRegistry>) {
        let m = GLOBAL_REGISTRY.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = m.lock() {
            *guard = Some(registry);
        }
    }

    pub fn global() -> Option<Arc<ToolBlockRendererRegistry>> {
        GLOBAL_REGISTRY
            .get()
            .and_then(|m| m.lock().ok())
            .and_then(|guard| guard.clone())
    }
}

// ---------------------------------------------------------------------------
// Animated card body helper
// ---------------------------------------------------------------------------

/// Wrap the card body in an animated height container.
///
/// Uses the persistent `content_height` from `CardRenderContext` so the
/// measured natural height survives across frames.  The wrapper constrains
/// the visible height to `natural_height × animation_scale`.
pub fn animated_card_body(
    body_content: impl gpui::IntoElement,
    animation_scale: f32,
    content_height: Rc<Cell<Pixels>>,
) -> gpui::Div {
    use gpui::prelude::FluentBuilder;
    use gpui::{div, Bounds, ParentElement, Styled};

    let height_for_render = content_height.clone();

    div()
        .overflow_hidden()
        .when(animation_scale < 1.0, move |d| {
            let h = height_for_render.get();
            if h > px(0.0) {
                d.h(h * animation_scale)
            } else {
                // Height not yet measured — constrain to zero so content
                // stays hidden until on_children_prepainted provides the
                // real value on the next frame.  Without this the content
                // flashes at full height for one frame.
                d.h(px(0.0))
            }
        })
        .on_children_prepainted({
            move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                if let Some(first) = bounds_vec.first() {
                    let new_h = first.size.height;
                    if content_height.get() != new_h {
                        content_height.set(new_h);
                    }
                }
            }
        })
        .child(body_content)
}

// ---------------------------------------------------------------------------
// InlineToolRenderer
// ---------------------------------------------------------------------------

/// A description template entry.
struct DescribeTemplate {
    tool_name: &'static str,
    /// Format string with `{param}` placeholders.  The renderer substitutes
    /// the first matching parameter value found in the tool block.
    template: &'static str,
}

/// Inline renderer for exploration / read-only tools.
pub struct InlineToolRenderer {
    tools: Vec<String>,
    templates: Vec<DescribeTemplate>,
}

impl InlineToolRenderer {
    pub fn new() -> Self {
        let templates = vec![
            DescribeTemplate {
                tool_name: "read_files",
                template: "Read {paths}",
            },
            DescribeTemplate {
                tool_name: "list_files",
                template: "List {paths}",
            },
            DescribeTemplate {
                tool_name: "search_files",
                template: "Search for \"{regex}\"",
            },
            DescribeTemplate {
                tool_name: "glob_files",
                template: "Glob {pattern}",
            },
            DescribeTemplate {
                tool_name: "web_search",
                template: "Search web for \"{query}\"",
            },
            DescribeTemplate {
                tool_name: "web_fetch",
                template: "Fetch {url}",
            },
            DescribeTemplate {
                tool_name: "perplexity_ask",
                template: "Ask Perplexity",
            },
        ];

        let tools: Vec<String> = templates.iter().map(|t| t.tool_name.to_string()).collect();

        Self { tools, templates }
    }

    /// Resolve `{param}` placeholders in a template against the tool's
    /// parameters.
    fn resolve_template(template: &str, tool: &ToolUseBlock) -> String {
        let mut result = template.to_string();
        for param in &tool.parameters {
            let placeholder = format!("{{{}}}", param.name);
            if result.contains(&placeholder) {
                // Truncate long values for the description line
                let display_value = if param.value.len() > 80 {
                    format!("{}…", &param.value[..77])
                } else {
                    param.value.clone()
                };
                result = result.replace(&placeholder, &display_value);
            }
        }
        result
    }
}

impl ToolBlockRenderer for InlineToolRenderer {
    fn supported_tools(&self) -> Vec<String> {
        self.tools.clone()
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        // Find the matching template
        if let Some(tmpl) = self.templates.iter().find(|t| t.tool_name == tool.name) {
            let desc = Self::resolve_template(tmpl.template, tool);
            // If the template still has unresolved placeholders (params not yet
            // streamed), show a friendlier fallback.
            if desc.contains('{') {
                tool.name.replace('_', " ")
            } else {
                desc
            }
        } else {
            tool.name.replace('_', " ")
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_component::theme::Theme,
        _card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        // Inline tools: render the output text with a left-border style when
        // expanded.  If there's no output yet, return None.
        let output = tool.output.as_deref().unwrap_or("");
        if output.is_empty() {
            return None;
        }

        let output_color = if tool.status == ToolStatus::Error {
            theme.danger
        } else {
            theme.muted_foreground
        };

        use gpui::{div, px, rems, ParentElement, Styled};
        Some(
            div()
                .pl(px(8.))
                .ml(px(8.))
                .border_l_2()
                .border_color(theme.border)
                .py(px(4.))
                .text_size(rems(0.8125))
                .text_color(output_color)
                .overflow_hidden()
                .child(output.to_string())
                .into_any(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::gpui::elements::{ParameterBlock, ToolUseBlock};

    fn make_tool(name: &str, params: &[(&str, &str)]) -> ToolUseBlock {
        ToolUseBlock {
            name: name.to_string(),
            id: "test-id".to_string(),
            parameters: params
                .iter()
                .map(|(n, v)| ParameterBlock {
                    name: n.to_string(),
                    value: v.to_string(),
                })
                .collect(),
            status: ToolStatus::Success,
            status_message: None,
            output: None,
            styled_output: None,
            state: crate::ui::gpui::elements::ToolBlockState::Collapsed,
            duration_seconds: None,
        }
    }

    #[test]
    fn test_describe_read_files() {
        let renderer = InlineToolRenderer::new();
        let tool = make_tool("read_files", &[("paths", "src/main.rs")]);
        assert_eq!(renderer.describe(&tool), "Read src/main.rs");
    }

    #[test]
    fn test_describe_search_files() {
        let renderer = InlineToolRenderer::new();
        let tool = make_tool("search_files", &[("regex", "fn main")]);
        assert_eq!(renderer.describe(&tool), "Search for \"fn main\"");
    }

    #[test]
    fn test_describe_missing_params_fallback() {
        let renderer = InlineToolRenderer::new();
        let tool = make_tool("read_files", &[]);
        // No params yet → friendly fallback
        assert_eq!(renderer.describe(&tool), "read files");
    }

    #[test]
    fn test_describe_long_value_truncated() {
        let renderer = InlineToolRenderer::new();
        let long_path = "a".repeat(100);
        let tool = make_tool("read_files", &[("paths", &long_path)]);
        let desc = renderer.describe(&tool);
        assert!(desc.len() < 100);
        assert!(desc.ends_with('…'));
    }

    #[test]
    fn test_registry_lookup() {
        let mut registry = ToolBlockRendererRegistry::new();
        registry.register(Arc::new(InlineToolRenderer::new()));
        assert!(registry.get("read_files").is_some());
        assert!(registry.get("search_files").is_some());
        assert!(registry.get("execute_command").is_none());
    }
}
