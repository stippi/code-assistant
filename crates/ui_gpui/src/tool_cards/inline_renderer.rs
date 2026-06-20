//! Inline renderer for exploration / read-only tools.
//!
//! Handles tools like `list_files`, `glob_files`, `web_search`, etc.
//! Renders as a minimal single-line description with expandable output.

use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use code_assistant_core::ui::ToolStatus;
use gpui::{
    div, img, px, rems, AnyElement, Context, Element, ImageSource, ObjectFit, ParentElement,
    Styled, StyledImage, Window,
};

/// A description template entry.
struct DescribeTemplate {
    tool_name: &'static str,
    /// Format string with `{param}` placeholders.  The renderer substitutes
    /// the first matching parameter value found in the tool block.
    template: &'static str,
    /// Fallback text shown before parameters have been resolved.
    fallback: &'static str,
}

/// Inline renderer for exploration / read-only tools.
pub struct InlineToolRenderer {
    tools: Vec<String>,
    templates: Vec<DescribeTemplate>,
}

impl InlineToolRenderer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let templates = vec![
            DescribeTemplate {
                tool_name: "list_files",
                template: "List {paths}",
                fallback: "List",
            },
            DescribeTemplate {
                tool_name: "glob_files",
                template: "Glob {pattern}",
                fallback: "Glob",
            },
            DescribeTemplate {
                tool_name: "web_search",
                template: "Search web for \"{query}\"",
                fallback: "Search web",
            },
            DescribeTemplate {
                tool_name: "web_fetch",
                template: "Fetch {url}",
                fallback: "Fetch",
            },
            DescribeTemplate {
                tool_name: "perplexity_ask",
                template: "Ask Perplexity",
                fallback: "Ask Perplexity",
            },
            DescribeTemplate {
                tool_name: "view_images",
                template: "View {paths}",
                fallback: "View",
            },
            DescribeTemplate {
                tool_name: "view_documents",
                template: "View {paths}",
                fallback: "View",
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
            // streamed), show the stable fallback text.
            if desc.contains('{') {
                tmpl.fallback.to_string()
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
        let has_images = !tool.images.is_empty();

        if output.is_empty() && !has_images {
            return None;
        }

        let output_color = if tool.status == ToolStatus::Error {
            theme.danger
        } else {
            theme.muted_foreground
        };

        let mut container = div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .text_size(rems(0.8125))
            .text_color(output_color)
            .overflow_hidden();

        // Text output
        if !output.is_empty() {
            container = container.child(output.to_string());
        }

        // Render images when expanded (for view_images tool)
        if has_images {
            let mut gallery = div().flex().flex_wrap().gap_2().mt_2();

            for (media_type, base64_data) in &tool.images {
                if let Some(image) =
                    crate::shared::image::parse_base64_image(media_type, base64_data)
                {
                    gallery = gallery.child(
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
                                    .max_h(px(200.))
                                    .max_w(px(400.))
                                    .object_fit(ObjectFit::Contain),
                            ),
                    );
                } else {
                    gallery = gallery.child(
                        div()
                            .flex_none()
                            .p_2()
                            .bg(theme.warning.opacity(0.1))
                            .border_1()
                            .border_color(theme.warning.opacity(0.3))
                            .rounded_md()
                            .text_color(theme.warning_foreground.opacity(0.8))
                            .text_xs()
                            .child(format!("Failed to decode: {}", media_type)),
                    );
                }
            }

            container = container.child(gallery);
        }

        Some(container.into_any())
    }
}
