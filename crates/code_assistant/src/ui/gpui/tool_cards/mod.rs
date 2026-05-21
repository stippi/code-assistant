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

pub mod animated_card;
pub mod code_card;
pub mod diff_card;
pub mod inline_renderer;
pub mod sub_agent_card;
pub mod terminal_card;

use crate::ui::gpui::blocks::{BlockView, ToolUseBlock};
use gpui::{AnyElement, Context, Entity, Pixels, Window};
use gpui_component::text::TextViewState;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

// Re-exports for backward compatibility
pub use animated_card::animated_card_body;
pub use inline_renderer::InlineToolRenderer;

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
    /// For write_file tool blocks: whether to show diff view (true) or plain
    /// new-file view (false). Only relevant when original_content is available.
    pub write_file_diff_mode: bool,
    /// Optional markdown state owned by the surrounding `BlockView`. Card
    /// renderers that show markdown should use this instead of `TextView::markdown`
    /// so virtualized rows do not recreate parsed markdown state on remount.
    pub markdown_state: Option<Entity<TextViewState>>,
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::gpui::blocks::{ParameterBlock, ToolUseBlock};
    use crate::ui::ToolStatus;

    pub(crate) fn make_tool(name: &str, params: &[(&str, &str)]) -> ToolUseBlock {
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
            state: crate::ui::gpui::blocks::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn test_registry_lookup() {
        let mut registry = ToolBlockRendererRegistry::new();
        registry.register(Arc::new(InlineToolRenderer::new()));
        registry.register(Arc::new(code_card::CodeCardRenderer));
        assert!(registry.get("read_files").is_some());
        assert!(registry.get("search_files").is_some());
        assert!(registry.get("list_files").is_some());
        assert!(registry.get("execute_command").is_none());
    }

    #[test]
    fn test_describe_read_files() {
        let renderer = code_card::CodeCardRenderer;
        let tool = make_tool("read_files", &[("paths", "src/main.rs")]);
        assert_eq!(renderer.describe(&tool), "Read src/main.rs");
    }

    #[test]
    fn test_describe_search_files() {
        let renderer = code_card::CodeCardRenderer;
        let tool = make_tool("search_files", &[("regex", "fn main")]);
        assert_eq!(renderer.describe(&tool), "Search for \"fn main\"");
    }

    #[test]
    fn test_describe_search_files_trailing_quote() {
        let renderer = code_card::CodeCardRenderer;
        let tool = make_tool("search_files", &[("regex", "cursor_not_allowed\"")]);
        assert_eq!(
            renderer.describe(&tool),
            "Search for \"cursor_not_allowed\""
        );
    }

    #[test]
    fn test_describe_search_files_surrounding_quotes() {
        let renderer = code_card::CodeCardRenderer;
        let tool = make_tool("search_files", &[("regex", "\"cursor_\"")]);
        assert_eq!(renderer.describe(&tool), "Search for \"cursor_\"");
    }

    #[test]
    fn test_describe_missing_params_fallback() {
        let renderer = code_card::CodeCardRenderer;
        let tool = make_tool("read_files", &[]);
        assert_eq!(renderer.describe(&tool), "Read");
    }

    #[test]
    fn test_describe_long_value_truncated() {
        let renderer = code_card::CodeCardRenderer;
        let long_path = "a".repeat(100);
        let tool = make_tool("read_files", &[("paths", &long_path)]);
        let desc = renderer.describe(&tool);
        assert!(desc.len() < 100);
        assert!(desc.ends_with('…'));
    }
}
