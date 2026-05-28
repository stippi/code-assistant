//! Rendering logic for [`BlockView`].
//!
//! This module implements the [`Render`] trait for `BlockView` and contains the
//! helper methods that produce GPUI elements for each block variant (text,
//! thinking, tool-use, compaction, image).

use super::{AnimationState, BlockData, BlockView, ToolBlockState, ToolUseBlock};
use crate::ui::gpui::shared::file_icons;
use crate::ui::ToolStatus;

/// Maximum height for rendered images in pixels
const MAX_IMAGE_HEIGHT: f32 = 80.0;

use gpui::{
    div, img, percentage, px, rems, svg, Animation, AnimationExt, ClickEvent, Context, ImageSource,
    IntoElement, ObjectFit, SharedString, Styled, Transformation,
};
use gpui::{prelude::*, FontWeight};
use gpui_component::ActiveTheme;
use std::time::Duration;

impl BlockView {
    // ------------------------------------------------------------------
    // Card skeleton (shown while parameters are still streaming)
    // ------------------------------------------------------------------

    /// Render a minimal card header for a tool whose renderer returned `None`
    /// (typically because parameters haven't arrived yet). This prevents the
    /// ugly `[edit]` / `[spawn_agent]` text flash.
    pub(super) fn render_card_skeleton(
        &self,
        block: &ToolUseBlock,
        renderer: &dyn crate::ui::gpui::tool_cards::ToolBlockRenderer,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        let is_dark = theme.background.l < 0.5;
        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };
        let header_text_color = theme.muted_foreground;
        let icon = file_icons::get().get_tool_icon(&block.name);
        let label = renderer.describe(block);

        div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .bg(header_bg)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .child(file_icons::render_icon_container(
                        &icon,
                        13.0,
                        header_text_color,
                        "⚙",
                    ))
                    .child(
                        div()
                            .text_size(rems(0.75))
                            .text_color(header_text_color)
                            .child(label),
                    ),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------------
    // Inline tool rendering
    // ------------------------------------------------------------------

    /// Render a tool block in the compact inline style.
    ///
    /// Layout:
    /// ```text
    /// [icon]  Description text                          [▾]   (chevron on hover)
    /// │  output content when expanded …
    /// ```
    pub(super) fn render_inline_tool(
        &mut self,
        block: &ToolUseBlock,
        renderer: &dyn crate::ui::gpui::tool_cards::ToolBlockRenderer,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = cx.theme().clone();

        // Icon
        let icon = file_icons::get().get_tool_icon(&block.name);
        let (icon_color, desc_color) = match block.status {
            ToolStatus::Error => (theme.danger, theme.danger),
            ToolStatus::Running | ToolStatus::Pending | ToolStatus::Success => {
                (theme.muted_foreground, theme.muted_foreground)
            }
        };

        // Description text
        let description = if block.status == ToolStatus::Error {
            if let Some(ref msg) = block.status_message {
                format!("{} — {}", renderer.describe(block), msg)
            } else {
                renderer.describe(block)
            }
        } else {
            renderer.describe(block)
        };

        // Determine expansion state — purely based on ToolBlockState, no is_generating override
        let is_expanded = block.state == ToolBlockState::Expanded;
        let has_output =
            block.output.as_ref().is_some_and(|o| !o.is_empty()) || !block.images.is_empty();
        let can_expand = has_output;

        // Animation scale for smooth expand/collapse
        let animation_scale = match &self.animation_state {
            AnimationState::Animating { height_scale, .. } => *height_scale,
            AnimationState::Idle => {
                if is_expanded {
                    1.0
                } else {
                    0.0
                }
            }
        };

        // Chevron icon (only visible on hover, via group)
        let chevron_icon = if is_expanded {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        };
        let chevron_color = theme.muted_foreground;

        // Running spinner
        let show_spinner = self.is_generating
            && (block.status == ToolStatus::Pending || block.status == ToolStatus::Running);

        // --- Build the element ---
        let mut container = div().w_full().mt_0p5();

        // Header line: clickable area with icon + description + chevron-on-hover
        let header = div()
            .id("inline-tool-header")
            .group("inline-tool")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_1()
            .py_1p5()
            .px_3()
            .cursor_pointer()
            .when(!can_expand && !is_expanded, |d| d.cursor_default())
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.toggle_tool_collapsed(cx);
            }))
            .child(
                // Left side: icon + description
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .flex_grow()
                    .min_w_0()
                    // Icon (or spinner) — both wrapped in a 14×14 container
                    // to prevent layout shift when transitioning.
                    .when(show_spinner, |d| {
                        d.child(
                            div()
                                .w(px(14.))
                                .h(px(14.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    gpui::svg()
                                        .size(px(14.))
                                        .path(SharedString::from("icons/arrow_circle.svg"))
                                        .text_color(icon_color)
                                        .with_animation(
                                            "inline_spinner",
                                            Animation::new(Duration::from_secs(2)).repeat(),
                                            |svg, delta| {
                                                svg.with_transformation(Transformation::rotate(
                                                    percentage(delta),
                                                ))
                                            },
                                        ),
                                ),
                        )
                    })
                    .when(!show_spinner, |d| {
                        d.child(file_icons::render_icon_container(
                            &icon, 14.0, icon_color, "🔧",
                        ))
                    })
                    // Description text
                    .child(
                        div()
                            .text_size(rems(0.8125))
                            .text_color(desc_color)
                            .overflow_hidden()
                            .text_overflow(gpui::TextOverflow::Truncate(SharedString::from("…")))
                            .child(description),
                    ),
            )
            // Chevron area — always laid out to prevent height changes when
            // output becomes available. The icon itself is only visible when
            // expandable, with a highlight on hover.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(6.))
                    .when(can_expand, |d| {
                        d.group_hover("inline-tool", |s| s.bg(theme.muted_foreground.opacity(0.1)))
                            .child(file_icons::render_icon(
                                &chevron_icon,
                                14.0,
                                chevron_color.opacity(0.4),
                                "▾",
                            ))
                    }),
            );

        container = container.child(header);

        // Animated output area
        if (is_expanded || animation_scale > 0.0) && has_output {
            if let Some(output_el) =
                renderer.render(block, self.is_generating, &theme, None, window, cx)
            {
                container = container.child(crate::ui::gpui::tool_cards::animated_card_body(
                    output_el,
                    animation_scale,
                    self.content_height.clone(),
                ));
            }
        }

        container
    }

    // ------------------------------------------------------------------
    // Zigzag line helper (used by compaction summary)
    // ------------------------------------------------------------------

    /// Render a zigzag/wiggle line using a canvas element.
    /// The line fills the available width and is vertically centered.
    pub(super) fn render_zigzag_line(color: gpui::Hsla) -> impl IntoElement {
        use gpui::{canvas, point, PathBuilder};

        canvas(
            |_, _, _| {},
            move |bounds, _, window, _cx| {
                let width = bounds.size.width;
                let height = bounds.size.height;
                let y_center = bounds.origin.y + height / 2.0;
                let x_start = bounds.origin.x;

                // Zigzag parameters
                let segment_width_f = 6.0_f32;
                let amplitude = px(2.5);

                // Compute number of segments from the width (Pixels -> f32 via division trick)
                // width / px(1.0) isn't available, so we'll use a large fixed count
                // and clamp x positions to not exceed bounds.
                let approx_segments = 200_i32; // More than enough for any realistic width

                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(x_start, y_center));

                for i in 1..=approx_segments {
                    let x = x_start + px(segment_width_f * i as f32);
                    if x > x_start + width {
                        break;
                    }
                    let y = if i % 2 == 0 {
                        y_center - amplitude
                    } else {
                        y_center + amplitude
                    };
                    builder.line_to(point(x, y));
                }

                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size_full()
    }
}

// --------------------------------------------------------------------------
// Render trait implementation
// --------------------------------------------------------------------------

impl gpui::Render for BlockView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.block.clone() {
            BlockData::TextBlock(block) => div()
                .mt_3()
                .text_color(cx.theme().foreground)
                .child(self.markdown_view(&block.content, true, cx))
                .into_any_element(),
            BlockData::ThinkingBlock(block) => {
                // Get the appropriate icon based on completed state
                let (icon, icon_text) = if block.is_completed {
                    (
                        file_icons::get().get_type_icon(file_icons::WORKING_MEMORY),
                        "🧠",
                    )
                } else {
                    (Some(SharedString::from("icons/arrow_circle.svg")), "🔄")
                };

                // Get the chevron icon based on collapsed state
                let (chevron_icon, chevron_text) = if block.is_collapsed {
                    (
                        file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                        "▼",
                    )
                } else {
                    (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
                };

                // Define header text based on state using reasoning-aware method
                let header_text = block.get_display_title(self.is_generating);

                // Use theme utilities for colors
                let blue_base = cx.theme().info; // Theme color for thinking block

                let thinking_bg =
                    crate::ui::gpui::shared::theme::colors::thinking_block_bg(cx.theme());
                let chevron_color =
                    crate::ui::gpui::shared::theme::colors::thinking_block_chevron(cx.theme());
                let text_color = cx.theme().info_foreground;

                div()
                    .mt_2()
                    .rounded_md()
                    .bg(thinking_bg)
                    .flex()
                    .flex_col()
                    .children(vec![
                        // Header row — entire row is clickable
                        div()
                            .id("thinking-header")
                            .group("thinking-header")
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .px_3()
                            .py_1p5()
                            .cursor_pointer()
                            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                                view.toggle_thinking_collapsed(cx);
                            }))
                            .children(vec![
                                // Left side with icon and text
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .children(vec![
                                        // Rotating arrow or brain icon
                                        if block.is_completed {
                                            file_icons::render_icon_container(
                                                &icon, 18.0, blue_base, icon_text,
                                            )
                                            .into_any()
                                        } else {
                                            svg()
                                                .size(px(18.))
                                                .path(SharedString::from("icons/arrow_circle.svg"))
                                                .text_color(blue_base)
                                                .with_animation(
                                                    "image_circle",
                                                    Animation::new(Duration::from_secs(2)).repeat(),
                                                    |svg, delta| {
                                                        svg.with_transformation(
                                                            Transformation::rotate(percentage(
                                                                delta,
                                                            )),
                                                        )
                                                    },
                                                )
                                                .into_any()
                                        },
                                        // Header text
                                        div()
                                            .font_weight(FontWeight(500.0))
                                            .text_color(blue_base)
                                            .child(header_text)
                                            .into_any(),
                                    ])
                                    .into_any(),
                                // Chevron — highlights on header hover via group
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(24.))
                                    .rounded(px(6.))
                                    .group_hover("thinking-header", |s| {
                                        s.bg(blue_base.opacity(0.1))
                                    })
                                    .child(file_icons::render_icon(
                                        &chevron_icon,
                                        16.0,
                                        chevron_color,
                                        chevron_text,
                                    ))
                                    .into_any(),
                            ])
                            .into_any(),
                        // Animated content container (uses shared helper)
                        {
                            let scale = match &self.animation_state {
                                AnimationState::Animating { height_scale, .. } => *height_scale,
                                AnimationState::Idle => {
                                    if block.is_collapsed {
                                        0.0
                                    } else {
                                        1.0
                                    }
                                }
                            };

                            let body_content = if !block.is_collapsed || scale > 0.0 {
                                let content = block.get_expanded_content(self.is_generating);
                                div()
                                    .px_3()
                                    .pt_1()
                                    .pb_2()
                                    .text_size(rems(0.875))
                                    .italic()
                                    .text_color(text_color)
                                    .child(self.markdown_view(&content, false, cx))
                                    .into_any()
                            } else {
                                div().into_any()
                            };

                            crate::ui::gpui::tool_cards::animated_card_body(
                                body_content,
                                scale,
                                self.content_height.clone(),
                            )
                            .into_any()
                        },
                    ])
                    .into_any_element()
            }
            BlockData::ToolUse(block) => {
                // Unified tool block rendering via ToolBlockRendererRegistry
                if let Some(registry) =
                    crate::ui::gpui::tool_cards::ToolBlockRendererRegistry::global()
                {
                    if let Some(renderer) = registry.get(&block.name) {
                        match renderer.style() {
                            crate::ui::gpui::tool_cards::ToolBlockStyle::Inline => {
                                let block_clone = block.clone();
                                return self
                                    .render_inline_tool(&block_clone, renderer.as_ref(), window, cx)
                                    .into_any_element();
                            }

                            crate::ui::gpui::tool_cards::ToolBlockStyle::Card => {
                                let block_clone = block.clone();
                                let theme = cx.theme().clone();

                                // Build animation context from BlockView state
                                let scale = match &self.animation_state {
                                    AnimationState::Animating { height_scale, .. } => *height_scale,
                                    AnimationState::Idle => match block.state {
                                        ToolBlockState::Collapsed => 0.0,
                                        ToolBlockState::Expanded => 1.0,
                                    },
                                };

                                let current_project = self.current_project.lock().unwrap().clone();
                                let markdown_state = self.markdown_state("", cx);

                                let card_ctx = crate::ui::gpui::tool_cards::CardRenderContext {
                                    animation_scale: scale,
                                    is_collapsed: block.state == ToolBlockState::Collapsed,
                                    content_height: self.content_height.clone(),
                                    current_project,
                                    write_file_diff_mode: self.write_file_diff_mode,
                                    markdown_state: Some(markdown_state),
                                };

                                if let Some(element) = renderer.render(
                                    &block_clone,
                                    self.is_generating,
                                    &theme,
                                    Some(&card_ctx),
                                    window,
                                    cx,
                                ) {
                                    return div().mt_2().child(element).into_any_element();
                                }
                                // Renderer returned None (e.g. parameters still
                                // streaming) — show a skeleton card with just
                                // the header so we don't flash a raw "[name]"
                                // placeholder.
                                return div()
                                    .mt_2()
                                    .child(self.render_card_skeleton(
                                        &block,
                                        renderer.as_ref(),
                                        &theme,
                                    ))
                                    .into_any_element();
                            }
                        }
                    } else {
                        tracing::warn!("No ToolBlockRenderer registered for tool '{}'", block.name);
                    }
                }

                div()
                    .mt_0p5()
                    .px_2()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .text_size(rems(0.8125))
                    .child(format!("[{}]", block.name))
                    .into_any_element()
            }
            BlockData::CompactionSummary(block) => {
                let is_expanded = block.is_expanded;

                // Chevron icon
                let chevron_icon = if is_expanded {
                    file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
                } else {
                    file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
                };
                let zigzag_color = cx.theme().border;
                let label_color = cx.theme().muted_foreground;

                // Zigzag line element (canvas-drawn)
                let zigzag_left = Self::render_zigzag_line(zigzag_color);
                let zigzag_right = Self::render_zigzag_line(zigzag_color);

                let header = div()
                    .id("compaction-header")
                    .group("compaction")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_1p5()
                    .px_3()
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.toggle_compaction(cx);
                    }))
                    // Left zigzag line
                    .child(
                        div()
                            .flex_1()
                            .h(px(8.))
                            .overflow_hidden()
                            .child(zigzag_left),
                    )
                    // Center: icon + label
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .flex_none()
                            .child(
                                svg()
                                    .size(px(14.))
                                    .path(SharedString::from("icons/clear.svg"))
                                    .text_color(label_color),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.8125))
                                    .text_color(label_color)
                                    .child("Conversation compacted"),
                            ),
                    )
                    // Right zigzag line
                    .child(
                        div()
                            .flex_1()
                            .h(px(8.))
                            .overflow_hidden()
                            .child(zigzag_right),
                    )
                    // Chevron
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .rounded(px(6.))
                            .group_hover("compaction", |s| {
                                s.bg(cx.theme().muted_foreground.opacity(0.1))
                            })
                            .child(file_icons::render_icon(
                                &chevron_icon,
                                14.0,
                                label_color.opacity(0.4),
                                "▾",
                            )),
                    );

                let mut container = div().mt_2().w_full().flex().flex_col();
                container = container.child(header);

                // Animated expand/collapse for the summary content
                let animation_scale = match &self.animation_state {
                    AnimationState::Animating { height_scale, .. } => *height_scale,
                    AnimationState::Idle => {
                        if is_expanded {
                            1.0
                        } else {
                            0.0
                        }
                    }
                };

                if is_expanded || animation_scale > 0.0 {
                    let body = div()
                        .px_3()
                        .pb_2()
                        .text_color(cx.theme().foreground)
                        .child(self.markdown_view(&block.summary, true, cx));

                    container = container.child(crate::ui::gpui::tool_cards::animated_card_body(
                        body,
                        animation_scale,
                        self.content_height.clone(),
                    ));
                }

                container.into_any_element()
            }
            BlockData::ImageBlock(block) => {
                if let Some(image) = &block.image {
                    div()
                        .mt_2()
                        .flex_none() // Don't grow or shrink
                        .child(
                            div()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_md()
                                .overflow_hidden()
                                .bg(cx.theme().popover)
                                .shadow_sm()
                                .child(
                                    img(ImageSource::Image(image.clone()))
                                        .max_h(px(MAX_IMAGE_HEIGHT)) // Use constant for max height
                                        .object_fit(ObjectFit::Contain), // Maintain aspect ratio
                                ),
                        )
                        .into_any_element()
                } else {
                    // Fallback to placeholder if image parsing failed
                    div()
                        .mt_2()
                        .flex_none()
                        .p_2()
                        .bg(cx.theme().warning.opacity(0.1))
                        .border_1()
                        .border_color(cx.theme().warning.opacity(0.3))
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .max_w(px(200.0)) // Limit width of error message
                        .child(
                            div()
                                .text_color(cx.theme().warning_foreground)
                                .text_xs()
                                .child("⚠️"),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().warning_foreground.opacity(0.8))
                                .text_xs()
                                .child(format!("Failed: {}", block.media_type)),
                        )
                        .into_any_element()
                }
            }
        }
    }
}
