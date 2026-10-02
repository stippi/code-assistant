//! Inline renderer for the `browser_*` tools.
//!
//! Browser work is many small calls (read, click, type, screenshot), so each
//! renders as one line — "Click ref_4", "Find \"Sign in\"" — like the other
//! inline tools. Expanding it shows the result: trees and logs in a monospace
//! font, cut to a readable length, and screenshots at a size where the page
//! can actually be read.

use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use code_assistant_core::tools::impls::browser::describe::{
    BROWSER_TOOL_NAMES, describe_call, has_structured_output, target_label,
};
use code_assistant_core::ui::ToolStatus;
use gpui_kit::{
    AnyElement, Context, Element, ImageSource, ObjectFit, ParentElement, Styled, StyledImage,
    Window, div, img, px, rems,
};

/// How many output lines an expanded block shows; the model saw all of them.
const MAX_OUTPUT_LINES: usize = 40;

/// Maximum height of a screenshot in an expanded block.
const SCREENSHOT_MAX_HEIGHT: f32 = 380.0;

pub struct BrowserToolRenderer;

impl ToolBlockRenderer for BrowserToolRenderer {
    fn supported_tools(&self) -> Vec<String> {
        BROWSER_TOOL_NAMES.iter().map(|s| s.to_string()).collect()
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        describe_call(&tool.name, &|name| param(tool, name))
    }

    fn header_tag(&self, tool: &ToolUseBlock) -> Option<String> {
        target_label(&|name| param(tool, name))
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_kit::component::theme::Theme,
        _card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        let output = tool.output.as_deref().unwrap_or("").trim();
        if output.is_empty() && tool.images.is_empty() {
            return None;
        }

        let mut container = div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .flex()
            .flex_col()
            .gap_2()
            .overflow_hidden();

        if !output.is_empty() {
            let color = if tool.status == ToolStatus::Error {
                theme.danger
            } else {
                theme.muted_foreground
            };
            let mut text = div()
                .text_size(rems(0.8125))
                .text_color(color)
                .overflow_hidden();
            if has_structured_output(&tool.name) {
                text = text.font_family("Menlo").line_height(rems(0.8125 * 1.4));
            }
            container = container.child(text.child(cap_lines(output, MAX_OUTPUT_LINES)));
        }

        for (media_type, base64_data) in &tool.images {
            if let Some(image) = crate::shared::image::parse_base64_image(media_type, base64_data) {
                container = container.child(
                    div()
                        .flex_none()
                        .border_1()
                        .border_color(theme.border)
                        .rounded_md()
                        .overflow_hidden()
                        .bg(theme.popover)
                        .shadow_sm()
                        .child(
                            img(ImageSource::Image(image))
                                .max_h(px(SCREENSHOT_MAX_HEIGHT))
                                .max_w_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                );
            }
        }

        Some(container.into_any())
    }
}

fn param(tool: &ToolUseBlock, name: &str) -> Option<String> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.clone())
        .filter(|v| !v.is_empty())
}

/// The first `max` lines of `text`, with a count of the rest.
fn cap_lines(text: &str, max: usize) -> String {
    let total = text.lines().count();
    if total <= max {
        return text.to_string();
    }
    let head: Vec<&str> = text.lines().take(max).collect();
    format!("{}\n… {} more lines", head.join("\n"), total - max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_cards::tests::make_tool;
    use std::sync::Arc;

    #[test]
    fn describes_calls_with_the_shared_wording() {
        let click = make_tool(
            "browser_computer",
            &[("action", "left_click"), ("ref", "ref_4")],
        );
        assert_eq!(BrowserToolRenderer.describe(&click), "Click ref_4");
        let nav = make_tool("browser_navigate", &[("url", "https://example.com")]);
        assert_eq!(
            BrowserToolRenderer.describe(&nav),
            "Navigate to https://example.com"
        );
    }

    #[test]
    fn tags_a_named_profile_and_tab() {
        let t = make_tool(
            "browser_navigate",
            &[("url", "https://elster.de"), ("profile", "elster")],
        );
        assert_eq!(
            BrowserToolRenderer.header_tag(&t).as_deref(),
            Some("elster")
        );
        let t = make_tool(
            "browser_navigate",
            &[("url", "https://x.com"), ("profile", "default")],
        );
        assert_eq!(BrowserToolRenderer.header_tag(&t), None);
    }

    #[test]
    fn long_output_is_capped_with_a_count() {
        let text = (1..=45)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let capped = cap_lines(&text, 40);
        assert!(capped.ends_with("line 40\n… 5 more lines"), "{capped}");
        assert_eq!(cap_lines("a\nb", 40), "a\nb");
    }

    #[test]
    fn registry_registers_all_browser_tools_inline() {
        let mut registry = crate::tool_cards::ToolBlockRendererRegistry::default();
        registry.register(Arc::new(BrowserToolRenderer));
        for name in BROWSER_TOOL_NAMES {
            let renderer = registry.get(name).expect(name);
            assert_eq!(renderer.style(), ToolBlockStyle::Inline);
        }
    }
}
