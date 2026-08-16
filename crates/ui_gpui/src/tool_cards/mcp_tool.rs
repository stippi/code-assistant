//! Inline renderer for MCP server tools.
//!
//! MCP tools are registered at runtime with dynamic names of the form
//! `mcp__<server>__<tool>` (see `mcp_client::naming`). They can't be
//! registered in the [`ToolBlockRendererRegistry`](super::ToolBlockRendererRegistry)
//! by a fixed name, so a single instance of this renderer is installed as the
//! registry's MCP fallback and handles every `mcp__…` block.
//!
//! Visually they follow the lightweight/explore-tool pattern: a generic MCP
//! icon, the bare tool name (`get_me`), and the server name (`github_tools_sap`)
//! as a pill on the right of the header — with the usual chevron to expand the
//! tool output.

use super::inline_renderer::{render_inline_output, render_inline_output_text};
use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use gpui::{AnyElement, Context, Window};

/// Split a registry tool name `mcp__<server>__<tool>` into `(server, tool)`.
///
/// The server segment is everything between the `mcp__` prefix and the next
/// `__`; the tool is the remainder (kept verbatim, so tool names containing
/// underscores survive). Returns `None` for names that don't match the shape.
pub(crate) fn parse_mcp_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.split_once("__")
}

/// Pretty-print `raw` if the whole (trimmed) string is a JSON object or array.
/// Most MCP tools return their result as a single compact JSON string, which
/// is far more readable formatted. Returns `None` for non-JSON output (plain
/// text, error messages), which is then shown verbatim.
fn pretty_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

/// Renders any `mcp__…` tool block inline.
#[derive(Default)]
pub struct McpToolRenderer;

impl McpToolRenderer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self
    }
}

impl ToolBlockRenderer for McpToolRenderer {
    fn supported_tools(&self) -> Vec<String> {
        // Registered as the registry's MCP fallback, not by fixed name.
        Vec::new()
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        match parse_mcp_name(&tool.name) {
            Some((_, tool_name)) => tool_name.to_string(),
            None => tool.name.clone(),
        }
    }

    fn header_tag(&self, tool: &ToolUseBlock) -> Option<String> {
        parse_mcp_name(&tool.name).map(|(server, _)| server.to_string())
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
        // Show formatted JSON in monospace when the output is JSON; otherwise
        // fall back to the plain inline rendering.
        match tool.output.as_deref().and_then(pretty_json) {
            Some(pretty) => render_inline_output_text(&pretty, true, tool, theme),
            None => render_inline_output(tool, theme),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_and_tool() {
        assert_eq!(
            parse_mcp_name("mcp__github_tools_sap__get_me"),
            Some(("github_tools_sap", "get_me"))
        );
    }

    #[test]
    fn tool_name_keeps_its_underscores() {
        assert_eq!(
            parse_mcp_name("mcp__jira__search_issues"),
            Some(("jira", "search_issues"))
        );
    }

    #[test]
    fn server_name_may_contain_hyphens() {
        assert_eq!(
            parse_mcp_name("mcp__http-test__echo"),
            Some(("http-test", "echo"))
        );
    }

    #[test]
    fn non_mcp_name_is_rejected() {
        assert_eq!(parse_mcp_name("read_files"), None);
        assert_eq!(parse_mcp_name("mcp__no_tool_separator"), None);
    }

    #[test]
    fn pretty_json_formats_objects_and_arrays() {
        let pretty = pretty_json(r#"{"a":1,"b":[2,3]}"#).expect("valid json");
        assert!(pretty.contains('\n'), "should be multi-line: {pretty}");
        assert!(pretty.contains("\"a\": 1"));
        assert!(pretty_json("  [1, 2, 3]  ").is_some());
    }

    #[test]
    fn pretty_json_leaves_non_json_alone() {
        assert_eq!(pretty_json("plain text output"), None);
        assert_eq!(pretty_json(""), None);
        // A bare number/string is not pretty-printed (nothing to gain).
        assert_eq!(pretty_json("42"), None);
    }
}
