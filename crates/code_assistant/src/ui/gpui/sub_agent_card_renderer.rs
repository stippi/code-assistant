//! Sub-agent card renderer for `spawn_agent` tool blocks.
//!
//! Renders the spawn_agent tool as a bordered card with:
//! - Header: icon + "Sub-agent", cancel button while running, red ✕ on error, chevron
//! - Body: instructions (muted, collapsible), tool call history, activity spinner,
//!   status line, and markdown response
//!
//! Replaces the old `SpawnAgentOutputRenderer` (ToolOutputRenderer) +
//! `SpawnAgentInstructionsRenderer` (ParameterRenderer) with a unified
//! `ToolBlockRenderer`.

use crate::agent::sub_agent::{SubAgentActivity, SubAgentOutput, SubAgentToolStatus};
use crate::ui::gpui::elements::{BlockView, ToolUseBlock};
use crate::ui::gpui::file_icons;
use crate::ui::gpui::tool_block_renderers::{
    animated_card_body, CardRenderContext, ToolBlockRenderer, ToolBlockStyle,
};
use crate::ui::ToolStatus;
use gpui::prelude::FluentBuilder;
use gpui::{
    bounce, div, ease_in_out, percentage, px, svg, Animation, AnimationExt, ClickEvent, Context,
    Element, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Transformation, Window,
};
use gpui_component::text::TextView;
use std::time::Duration;

// ---------------------------------------------------------------------------
// SubAgentCardRenderer
// ---------------------------------------------------------------------------

pub struct SubAgentCardRenderer;

impl ToolBlockRenderer for SubAgentCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["spawn_agent".to_string()]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Card
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        if let Some(instructions) = get_param(tool, "instructions") {
            let truncated = truncate_str(instructions, 60);
            format!("Sub-agent: {truncated}")
        } else {
            "Sub-agent".to_string()
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        let card_ctx = card_ctx?;

        // Need at least instructions or output to show anything.
        let instructions = get_param(tool, "instructions");
        let output_str = tool.output.as_deref().unwrap_or("");

        if instructions.is_none() && output_str.is_empty() {
            return None;
        }

        // Parse output JSON (if available).
        let parsed = if !output_str.is_empty() {
            SubAgentOutput::from_json(output_str)
        } else {
            None
        };

        let has_error =
            tool.status == ToolStatus::Error || parsed.as_ref().is_some_and(|p| p.error.is_some());

        let is_running = matches!(tool.status, ToolStatus::Pending | ToolStatus::Running);

        let scale = card_ctx.animation_scale;
        let is_collapsed = card_ctx.is_collapsed;

        let is_dark = theme.background.l < 0.5;

        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };

        // --- Card container ---

        let mut card = div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden();

        // --- Header ---
        let header_text_color = theme.muted_foreground;

        let icon = file_icons::get().get_tool_icon("spawn_agent");

        let chevron_icon = if is_collapsed {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        };

        // Left: icon + label
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
                "⚙",
            ))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(header_text_color)
                    .child("Sub-agent"),
            );

        // Right: [status/spinner] [cancel] [✕] [chevron]
        let mut header_right = div().flex().flex_row().items_center().gap_2();

        // Activity spinner in header (while running)
        if is_running {
            if let Some(ref parsed) = parsed {
                if let Some(ref activity) = parsed.activity {
                    let (text, color) = activity_label(activity, theme);
                    if !text.is_empty() {
                        header_right = header_right.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .child(
                                    svg()
                                        .size(px(12.))
                                        .path(SharedString::from("icons/arrow_circle.svg"))
                                        .text_color(color)
                                        .with_animation(
                                            "sub_agent_header_spinner",
                                            Animation::new(Duration::from_secs(2))
                                                .repeat()
                                                .with_easing(bounce(ease_in_out)),
                                            |svg, delta| {
                                                svg.with_transformation(Transformation::rotate(
                                                    percentage(delta),
                                                ))
                                            },
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(color)
                                        .child(text.to_string()),
                                ),
                        );
                    }
                }
            } else {
                // No parsed output yet — show generic waiting
                header_right = header_right.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.muted_foreground)
                        .child("Starting…"),
                );
            }
        }

        // Cancel button (while running)
        if is_running {
            let tool_id_cancel = tool.id.clone();
            header_right = header_right.child(
                div()
                    .id(SharedString::from(format!("cancel-sa-{}", tool.id)))
                    .px_2()
                    .py(px(1.))
                    .rounded(px(4.))
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.danger.opacity(0.15)).text_color(theme.danger))
                    .on_click(cx.listener(move |_view, _event: &ClickEvent, _window, cx| {
                        if let Some(sender) = cx.try_global::<crate::ui::gpui::UiEventSender>() {
                            let _ = sender.0.try_send(crate::ui::UiEvent::CancelSubAgent {
                                tool_id: tool_id_cancel.clone(),
                            });
                        }
                    }))
                    .child("Cancel"),
            );
        }

        // Red ✕ on error
        if has_error {
            header_right = header_right.child(
                gpui::svg()
                    .size(px(13.0))
                    .path(SharedString::from("icons/close.svg"))
                    .text_color(theme.danger),
            );
        }

        // Chevron — highlights on header hover via group
        header_right = header_right.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(6.))
                .group_hover("sa-header", |s| s.bg(header_text_color.opacity(0.1)))
                .child(file_icons::render_icon(
                    &chevron_icon,
                    14.0,
                    header_text_color.opacity(0.4),
                    "▾",
                )),
        );

        // Header with conditional rounding
        card = card.child(
            div()
                .id(SharedString::from(format!("sa-header-{}", tool.id)))
                .group("sa-header")
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
                        d.rounded_md()
                    } else {
                        d.rounded_t_md()
                    }
                })
                .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                    view.toggle_tool_collapsed(cx);
                }))
                .child(header_left)
                .child(header_right),
        );

        // --- Body (animated) ---
        if scale > 0.0 {
            let body_bg = if is_dark {
                gpui::hsla(0.0, 0.0, 0.08, 1.0)
            } else {
                gpui::hsla(0.0, 0.0, 0.97, 1.0)
            };

            let mut body = div()
                .w_full()
                .px_3()
                .py_1p5()
                .bg(body_bg)
                .rounded_b_md()
                .flex()
                .flex_col()
                .gap_0p5()
                .text_size(px(13.));

            // Instructions (compact, muted)
            if let Some(instructions) = instructions {
                if !instructions.is_empty() {
                    body = body.child(
                        div()
                            .pb_1()
                            .mb_1()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.muted_foreground.opacity(0.7))
                                    .overflow_hidden()
                                    .child(truncate_str(instructions, 200)),
                            ),
                    );
                }
            }

            // Tool call history + activity + status + response (from parsed output)
            if let Some(ref parsed) = parsed {
                // Tool lines
                for tool_call in &parsed.tools {
                    body = body.child(render_tool_line(tool_call, theme));
                }

                // Activity line with spinner (inside body, only when not running —
                // running state already shown in header)
                if !is_running {
                    if let Some(ref activity) = parsed.activity {
                        if let Some(el) = render_activity_line(activity, theme) {
                            body = body.child(el);
                        }
                    }
                }

                // Cancelled / error status
                if let Some(el) = render_status_line(parsed, theme) {
                    body = body.child(el);
                }

                // Final response as markdown
                if let Some(ref response) = parsed.response {
                    if !response.is_empty() {
                        body = body.child(render_response(response, theme, window, cx));
                    }
                }
            } else if output_str.is_empty() && is_running {
                // No output yet — show waiting indicator in body
                body = body.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child("Waiting for sub-agent…"),
                );
            }

            card = card.child(animated_card_body(
                body,
                scale,
                card_ctx.content_height.clone(),
            ));
        }

        Some(card.into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Body rendering helpers (extracted from SpawnAgentOutputRenderer)
// ---------------------------------------------------------------------------

/// Render a single compact tool line.
fn render_tool_line(
    tool: &crate::agent::sub_agent::SubAgentToolCall,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    let icon = file_icons::get().get_tool_icon(&tool.name);

    let (icon_color, text_color) = match tool.status {
        SubAgentToolStatus::Running => (theme.info, theme.muted_foreground),
        SubAgentToolStatus::Success => (theme.success, theme.muted_foreground),
        SubAgentToolStatus::Error => (theme.danger, theme.danger),
    };

    let display_text = tool
        .title
        .as_ref()
        .filter(|t| !t.is_empty())
        .cloned()
        .or_else(|| tool.message.as_ref().filter(|m| !m.is_empty()).cloned())
        .unwrap_or_else(|| tool.name.replace('_', " "));

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .py(px(2.))
        .child(file_icons::render_icon_container(
            &icon, 14.0, icon_color, "🔧",
        ))
        .child(
            div()
                .text_size(px(13.))
                .text_color(text_color)
                .child(display_text),
        )
        .into_any()
}

/// Get label and color for an activity state.
fn activity_label(
    activity: &SubAgentActivity,
    theme: &gpui_component::theme::Theme,
) -> (&'static str, gpui::Hsla) {
    match activity {
        SubAgentActivity::WaitingForLlm => ("Waiting…", theme.muted_foreground),
        SubAgentActivity::Streaming => ("Responding…", theme.info),
        SubAgentActivity::ExecutingTools => ("Executing tools…", theme.info),
        SubAgentActivity::Completed => ("", theme.muted_foreground),
        SubAgentActivity::Cancelled => ("Cancelled", theme.warning),
        SubAgentActivity::Failed => ("", theme.danger),
    }
}

/// Render activity line for non-running states (cancelled, etc.).
fn render_activity_line(
    activity: &SubAgentActivity,
    theme: &gpui_component::theme::Theme,
) -> Option<gpui::AnyElement> {
    match activity {
        SubAgentActivity::Cancelled => Some(
            div()
                .py(px(2.))
                .text_size(px(13.))
                .text_color(theme.warning)
                .child("Cancelled")
                .into_any(),
        ),
        _ => None,
    }
}

/// Render error/cancelled status line.
fn render_status_line(
    output: &SubAgentOutput,
    theme: &gpui_component::theme::Theme,
) -> Option<gpui::AnyElement> {
    if output.cancelled == Some(true) {
        return Some(
            div()
                .py(px(2.))
                .text_size(px(13.))
                .text_color(theme.warning)
                .child("Sub-agent cancelled")
                .into_any(),
        );
    }

    if let Some(ref error) = output.error {
        return Some(
            div()
                .py(px(2.))
                .text_size(px(13.))
                .text_color(theme.danger)
                .child(format!("Error: {error}"))
                .into_any(),
        );
    }

    None
}

/// Render the final response as markdown.
fn render_response(
    response: &str,
    theme: &gpui_component::theme::Theme,
    window: &mut Window,
    cx: &mut Context<BlockView>,
) -> gpui::AnyElement {
    div()
        .mt_1()
        .pt_1()
        .border_t_1()
        .border_color(theme.border)
        .text_color(theme.foreground)
        .child(TextView::markdown(
            "sub-agent-response",
            response.to_string(),
            window,
            cx,
        ))
        .into_any()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count > max_chars {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    } else {
        s.to_string()
    }
}
