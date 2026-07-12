//! Browser card renderer for the `browser_*` tool blocks.
//!
//! Each browser action (navigate / read / act / close / login) is its own tool
//! call, so it already gets its own block. This renders that block as a
//! collapsible card: a header with the action and status, and — on expand — the
//! page screenshot captured at that step, plus a short URL/title caption.

use super::{animated_card_body, CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use crate::shared::file_icons;
use code_assistant_core::ui::ToolStatus;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, img, percentage, px, rems, Animation, AnimationExt, AnyElement, ClickEvent, Context,
    ImageSource, InteractiveElement, IntoElement, ObjectFit, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Transformation, Window,
};
use std::time::Duration;

const BROWSER_TOOLS: [&str; 5] = [
    "browser_navigate",
    "browser_read",
    "browser_act",
    "browser_close",
    "browser_login",
];

/// Maximum height of a screenshot inside a card body.
const SCREENSHOT_MAX_HEIGHT: f32 = 380.0;

pub struct BrowserCardRenderer;

impl ToolBlockRenderer for BrowserCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        BROWSER_TOOLS.iter().map(|s| s.to_string()).collect()
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Card
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        describe(tool)
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        let card_ctx = card_ctx?;
        let scale = card_ctx.animation_scale;
        let is_collapsed = card_ctx.is_collapsed;

        let is_dark = theme.background.l < 0.5;
        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };
        let header_text_color = theme.muted_foreground;

        let mut card = div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden();

        // ---- Header ----
        let icon = file_icons::get().get_tool_icon(&tool.name);
        let header_left = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .flex_grow()
            .child(file_icons::render_icon_container(
                &icon,
                13.0,
                header_text_color,
                "🌐",
            ))
            .child(
                div()
                    .text_size(rems(0.75))
                    .text_color(header_text_color)
                    .overflow_hidden()
                    .child(describe(tool)),
            );

        let mut header_right = div().flex().flex_row().items_center().gap_2();
        match tool.status {
            ToolStatus::Running | ToolStatus::Pending => {
                header_right = header_right.child(
                    gpui::svg()
                        .size(px(12.))
                        .path(SharedString::from("icons/arrow_circle.svg"))
                        .text_color(header_text_color)
                        .with_animation(
                            SharedString::from(format!("browser-spin-{}", tool.id)),
                            Animation::new(Duration::from_secs(2)).repeat(),
                            |svg, delta| {
                                svg.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ),
                );
            }
            ToolStatus::Error => {
                header_right = header_right.child(
                    gpui::svg()
                        .size(px(13.0))
                        .path(SharedString::from("icons/close.svg"))
                        .text_color(theme.danger),
                );
            }
            ToolStatus::Success => {}
        }

        let chevron_icon = if is_collapsed {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        };
        header_right = header_right.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(6.))
                .group_hover("browser-header", |s| s.bg(header_text_color.opacity(0.1)))
                .child(file_icons::render_icon(
                    &chevron_icon,
                    14.0,
                    header_text_color.opacity(0.4),
                    "▾",
                )),
        );

        card = card.child(
            div()
                .id(SharedString::from(format!("browser-header-{}", tool.id)))
                .group("browser-header")
                .px_3()
                .py_1p5()
                .bg(header_bg)
                .cursor_pointer()
                .flex()
                .flex_row()
                .justify_between()
                .items_center()
                .map(|d| {
                    if scale <= 0.0 {
                        d.rounded(px(4.))
                    } else {
                        d.rounded_t(px(4.))
                    }
                })
                .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                    view.toggle_tool_collapsed(cx);
                }))
                .child(header_left)
                .child(header_right),
        );

        // ---- Body (animated) ----
        if scale > 0.0 {
            let body_inner = self.render_body(tool, theme);
            card = card.child(animated_card_body(
                body_inner,
                scale,
                card_ctx.content_height.clone(),
            ));
        }

        Some(card.into_any_element())
    }
}

impl BrowserCardRenderer {
    fn render_body(&self, tool: &ToolUseBlock, theme: &gpui_component::theme::Theme) -> gpui::Div {
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(theme.background)
            .rounded_b(px(4.))
            .overflow_hidden();

        // Screenshot(s) captured at this step.
        for (media_type, base64_data) in &tool.images {
            if let Some(image) = crate::shared::image::parse_base64_image(media_type, base64_data) {
                body = body.child(
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

        // Caption: the URL/title lines for success, or the error text.
        if let Some(output) = tool.output.as_deref() {
            let is_error = tool.status == ToolStatus::Error;
            let caption = caption_from_output(output, is_error);
            if !caption.is_empty() {
                let color = if is_error {
                    theme.danger
                } else {
                    theme.muted_foreground
                };
                body = body.child(div().text_size(rems(0.75)).text_color(color).child(caption));
            }
        } else if matches!(tool.status, ToolStatus::Running | ToolStatus::Pending) {
            body = body.child(
                div()
                    .text_size(rems(0.75))
                    .text_color(theme.muted_foreground.opacity(0.7))
                    .child("Working…"),
            );
        }

        body
    }
}

/// A one-line header description per browser tool, from its parameters.
fn describe(tool: &ToolUseBlock) -> String {
    let param = |name: &str| {
        tool.parameters
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.clone())
    };
    let profile_suffix = match param("profile") {
        Some(p) if !p.is_empty() && p != "default" => format!("  ·  {p}"),
        _ => String::new(),
    };

    let base = match tool.name.as_str() {
        "browser_navigate" => match param("url") {
            Some(url) => format!("Navigate to {}", truncate(&url, 70)),
            None => "Navigate".to_string(),
        },
        "browser_login" => match param("url") {
            Some(url) => format!("Log in at {}", truncate(&url, 60)),
            None => "Log in".to_string(),
        },
        "browser_read" => "Read page".to_string(),
        "browser_close" => "Close browser".to_string(),
        "browser_act" => describe_act(param("actions").as_deref()),
        other => other.to_string(),
    };
    format!("{base}{profile_suffix}")
}

/// For `browser_act`, summarize the action count when the JSON parses.
fn describe_act(actions_json: Option<&str>) -> String {
    if let Some(json) = actions_json {
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json)
        {
            let n = items.len();
            return format!("Interact ({n} step{})", if n == 1 { "" } else { "s" });
        }
    }
    "Interact with page".to_string()
}

/// Show the first few informative lines of the tool output as a caption. For
/// success that is the `Profile/URL/Title` header the tool emits; for errors it
/// is the error message.
fn caption_from_output(output: &str, is_error: bool) -> String {
    if is_error {
        return output.trim().lines().take(3).collect::<Vec<_>>().join("\n");
    }
    output
        .lines()
        .filter(|l| l.starts_with("URL:") || l.starts_with("Title:"))
        .take(2)
        .collect::<Vec<_>>()
        .join("  ·  ")
}

fn truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count > max_chars {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_cards::tests::make_tool;
    use std::sync::Arc;

    #[test]
    fn describe_navigate_shows_url() {
        let t = make_tool("browser_navigate", &[("url", "https://example.com")]);
        assert_eq!(describe(&t), "Navigate to https://example.com");
    }

    #[test]
    fn describe_appends_non_default_profile() {
        let t = make_tool(
            "browser_navigate",
            &[("url", "https://elster.de"), ("profile", "elster")],
        );
        assert_eq!(describe(&t), "Navigate to https://elster.de  ·  elster");
    }

    #[test]
    fn describe_default_profile_has_no_suffix() {
        let t = make_tool("browser_read", &[("profile", "default")]);
        assert_eq!(describe(&t), "Read page");
    }

    #[test]
    fn describe_act_counts_steps() {
        let two = make_tool(
            "browser_act",
            &[(
                "actions",
                r##"[{"click":{"selector":"#a"}},{"type":{"selector":"#b","text":"x"}}]"##,
            )],
        );
        assert_eq!(describe(&two), "Interact (2 steps)");
        let one = make_tool(
            "browser_act",
            &[("actions", r##"[{"click":{"selector":"#a"}}]"##)],
        );
        assert_eq!(describe(&one), "Interact (1 step)");
    }

    #[test]
    fn caption_extracts_url_and_title_on_success() {
        let out = "Profile: default\nURL: https://x.com\nTitle: Hi\n\nbody text";
        assert_eq!(
            caption_from_output(out, false),
            "URL: https://x.com  ·  Title: Hi"
        );
    }

    #[test]
    fn caption_shows_error_text() {
        assert_eq!(
            caption_from_output("Browser error: boom", true),
            "Browser error: boom"
        );
    }

    #[test]
    fn registry_registers_all_browser_tools() {
        let mut registry = crate::tool_cards::ToolBlockRendererRegistry::default();
        registry.register(Arc::new(BrowserCardRenderer));
        for name in BROWSER_TOOLS {
            assert!(registry.get(name).is_some(), "missing renderer for {name}");
        }
    }
}
