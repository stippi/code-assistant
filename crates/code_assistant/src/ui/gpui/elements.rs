use crate::ui::gpui::file_icons;
use crate::ui::gpui::parameter_renderers::ParameterRendererRegistry;
use crate::ui::ToolStatus;
use gpui::{
    bounce, div, ease_in_out, percentage, px, svg, Animation, AnimationExt, Context, Entity,
    IntoElement, MouseButton, SharedString, Styled, Transformation,
};
use gpui::{prelude::*, FontWeight};
use gpui_component::ActiveTheme;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Role of a message in the conversation
#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// Container for all elements within a message
#[derive(Clone)]
pub struct MessageContainer {
    elements: Arc<Mutex<Vec<Entity<BlockView>>>>,
    role: MessageRole,
}

impl MessageContainer {
    pub fn with_role(role: MessageRole, _cx: &mut Context<Self>) -> Self {
        Self {
            elements: Arc::new(Mutex::new(Vec::new())),
            role,
        }
    }

    /// Check if this is a user message
    pub fn is_user_message(&self) -> bool {
        self.role == MessageRole::User
    }

    pub fn elements(&self) -> Vec<Entity<BlockView>> {
        let elements = self.elements.lock().unwrap();
        elements.clone()
    }

    // Add a new text block
    pub fn add_text_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::TextBlock(TextBlock {
            content: content.into(),
        });
        let view = cx.new(|cx| BlockView::new(block, cx));
        elements.push(view);
    }

    // Add a new thinking block
    #[allow(dead_code)]
    pub fn add_thinking_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(content.into()));
        let view = cx.new(|cx| BlockView::new(block, cx));
        elements.push(view);
    }

    // Add a new tool use block
    pub fn add_tool_use_block(
        &self,
        name: impl Into<String>,
        id: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.finish_any_thinking_blocks(cx);
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ToolUse(ToolUseBlock {
            name: name.into(),
            id: id.into(),
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
            is_collapsed: true, // Default to collapsed
        });
        let view = cx.new(|cx| BlockView::new(block, cx));
        elements.push(view);
    }

    // Update the status of a tool block
    pub fn update_tool_status(
        &self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let elements = self.elements.lock().unwrap();
        let mut updated = false;

        for element in elements.iter() {
            element.update(cx, |view, cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        tool.status = status;
                        tool.status_message = message.clone();

                        // Auto-expand failed tool calls
                        if status == ToolStatus::Error {
                            tool.is_collapsed = false;
                        }

                        updated = true;
                        cx.notify();
                    }
                }
            });
        }

        updated
    }

    // Add or append to text block
    pub fn add_or_append_to_text_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_appended = false;

            last.update(cx, |view, cx| {
                if let Some(text_block) = view.block.as_text_mut() {
                    text_block.content.push_str(&content);
                    was_appended = true;
                    cx.notify();
                }
            });

            if was_appended {
                return;
            }
        }

        // If we reach here, we need to add a new text block
        let block = BlockData::TextBlock(TextBlock {
            content: content.to_string(),
        });
        let view = cx.new(|cx| BlockView::new(block, cx));
        elements.push(view);
    }

    // Add or append to thinking block
    pub fn add_or_append_to_thinking_block(
        &self,
        content: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_appended = false;

            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    thinking_block.content.push_str(&content);
                    was_appended = true;
                    cx.notify();
                }
            });

            if was_appended {
                return;
            }
        }

        // If we reach here, we need to add a new thinking block
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(content.to_string()));
        let view = cx.new(|cx| BlockView::new(block, cx));
        elements.push(view);
    }

    // Add or update tool parameter
    pub fn add_or_update_tool_parameter(
        &self,
        tool_id: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let tool_id = tool_id.into();
        let name = name.into();
        let value = value.into();
        let mut elements = self.elements.lock().unwrap();
        let mut tool_found = false;

        // Find the tool block with matching ID
        for element in elements.iter().rev() {
            let mut param_added = false;

            element.update(cx, |view, cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        tool_found = true;

                        // Check if parameter with this name already exists
                        for param in tool.parameters.iter_mut() {
                            if param.name == name {
                                // Update existing parameter
                                param.value.push_str(&value);
                                param_added = true;
                                break;
                            }
                        }

                        // Add new parameter if not found
                        if !param_added {
                            tool.parameters.push(ParameterBlock {
                                name: name.clone(),
                                value: value.clone(),
                            });
                            param_added = true;
                        }

                        cx.notify();
                    }
                }
            });

            if param_added {
                return;
            }
        }

        // If we didn't find a matching tool, create a new one with this parameter
        if !tool_found {
            let mut tool = ToolUseBlock {
                name: "unknown".to_string(), // Default name since we only have ID
                id: tool_id.clone(),
                parameters: Vec::new(),
                status: ToolStatus::Pending,
                status_message: None,
                is_collapsed: true, // Default to collapsed
            };

            tool.parameters.push(ParameterBlock {
                name: name.clone(),
                value: value.clone(),
            });

            let block = BlockData::ToolUse(tool);
            let view = cx.new(|cx| BlockView::new(block, cx));
            elements.push(view);
        }
    }

    // Mark a tool as ended (could add visual indicator)
    pub fn end_tool_use(&self, id: impl Into<String>, _cx: &mut Context<Self>) {
        // Currently no specific action needed, but could add visual indicator
        // that the tool execution is complete
        let _id = id.into();
    }

    fn finish_any_thinking_blocks(&self, cx: &mut Context<Self>) {
        let elements = self.elements.lock().unwrap();

        // Mark any previous thinking blocks as completed
        for element in elements.iter() {
            element.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    if !thinking_block.is_completed {
                        thinking_block.is_completed = true;
                        thinking_block.end_time = std::time::Instant::now();
                        cx.notify();
                    }
                }
            });
        }
    }

    // Toggle a thinking block's collapsed state by its index
    #[allow(dead_code)]
    pub fn toggle_thinking_collapsed(&self, cx: &mut Context<Self>, index: usize) -> bool {
        let elements = self.elements.lock().unwrap();
        let mut thinking_index = 0;
        let mut changed = false;

        for element in elements.iter() {
            element.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    if thinking_index == index {
                        thinking_block.is_collapsed = !thinking_block.is_collapsed;
                        changed = true;
                        cx.notify();
                    }
                    thinking_index += 1;
                }
            });

            if changed {
                break;
            }
        }

        changed
    }
}

/// Different types of blocks that can appear in a message
#[derive(Debug, Clone)]
pub enum BlockData {
    TextBlock(TextBlock),
    ThinkingBlock(ThinkingBlock),
    ToolUse(ToolUseBlock),
}

impl BlockData {
    fn as_text_mut(&mut self) -> Option<&mut TextBlock> {
        match self {
            BlockData::TextBlock(b) => Some(b),
            _ => None,
        }
    }

    fn as_thinking_mut(&mut self) -> Option<&mut ThinkingBlock> {
        match self {
            BlockData::ThinkingBlock(b) => Some(b),
            _ => None,
        }
    }

    fn as_tool_mut(&mut self) -> Option<&mut ToolUseBlock> {
        match self {
            BlockData::ToolUse(b) => Some(b),
            _ => None,
        }
    }
}

/// Entity view for a block
pub struct BlockView {
    block: BlockData,
}

impl BlockView {
    pub fn new(block: BlockData, _cx: &mut Context<Self>) -> Self {
        Self { block }
    }

    fn toggle_thinking_collapsed(&mut self, cx: &mut Context<Self>) {
        if let Some(thinking) = self.block.as_thinking_mut() {
            thinking.is_collapsed = !thinking.is_collapsed;
            cx.notify();
        }
    }

    fn toggle_tool_collapsed(&mut self, cx: &mut Context<Self>) {
        if let Some(tool) = self.block.as_tool_mut() {
            tool.is_collapsed = !tool.is_collapsed;
            cx.notify();
        }
    }
}

impl Render for BlockView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.block {
            BlockData::TextBlock(block) => {
                // Use TextView with Markdown for rendering text
                div()
                    .text_color(cx.theme().foreground)
                    .child(gpui_component::text::TextView::markdown(
                        "md-block",
                        block.content.clone(),
                    ))
                    .into_any_element()
            }
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

                // Define header text based on state
                let header_text = if block.is_completed {
                    format!("Thought for {}", block.formatted_duration())
                } else {
                    "Thinking...".to_string()
                };

                // Use theme utilities for colors
                let blue_base = cx.theme().info; // Theme color for thinking block
                let thinking_bg = crate::ui::gpui::theme::colors::thinking_block_bg(&cx.theme());
                let chevron_color =
                    crate::ui::gpui::theme::colors::thinking_block_chevron(&cx.theme());
                let text_color = cx.theme().info_foreground;

                div()
                    .rounded_md()
                    .p_2()
                    .mb_2()
                    .bg(thinking_bg)
                    .flex()
                    .flex_col()
                    .children(vec![
                        // Header row with icon and text
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between() // Spread items
                            .w_full()
                            .mb_1()
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
                                            // Just render the brain icon normally
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
                                                    Animation::new(Duration::from_secs(2))
                                                        .repeat()
                                                        .with_easing(bounce(ease_in_out)),
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
                                // Right side with the expand/collapse button
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .size(px(24.))
                                    .rounded_full()
                                    .hover(|s| s.bg(blue_base.opacity(0.1)))
                                    .child(file_icons::render_icon(
                                        &chevron_icon,
                                        16.0,
                                        chevron_color,
                                        chevron_text,
                                    ))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |view, _event, _window, cx| {
                                            view.toggle_thinking_collapsed(cx);
                                        }),
                                    )
                                    .into_any(),
                            ])
                            .into_any(),
                        // Content (only shown when expanded)
                        if !block.is_collapsed {
                            div()
                                .pt_1()
                                .text_size(px(14.))
                                .italic()
                                .text_color(text_color)
                                .child(gpui_component::text::TextView::markdown(
                                    "thinking-content",
                                    block.content.clone(),
                                ))
                                .into_any()
                        } else {
                            // If collapsed, show a preview of the first line using Markdown
                            let first_line = block.content.lines().next().unwrap_or("").to_string();
                            div()
                                .pt_1()
                                .text_size(px(14.))
                                .italic()
                                .text_color(text_color)
                                .opacity(0.7)
                                .text_ellipsis()
                                .child(gpui_component::text::TextView::markdown(
                                    "thinking-preview",
                                    first_line + "...",
                                ))
                                .into_any()
                        },
                    ])
                    .into_any_element()
            }
            BlockData::ToolUse(block) => {
                // Get the appropriate icon for this tool type
                let icon = file_icons::get().get_tool_icon(&block.name);

                // Get the chevron icon based on collapsed state
                let (chevron_icon, chevron_text) = if block.is_collapsed {
                    (
                        file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                        "▼",
                    )
                } else {
                    (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
                };

                // Use theme utilities for colors
                let icon_color =
                    crate::ui::gpui::theme::colors::tool_block_icon(&cx.theme(), &block.status);
                let border_color = crate::ui::gpui::theme::colors::tool_border_by_status(
                    &cx.theme(),
                    &block.status,
                );
                let tool_bg = crate::ui::gpui::theme::colors::tool_block_bg(&cx.theme());
                let chevron_color = crate::ui::gpui::theme::colors::thinking_block_chevron(&cx.theme());

                // Parameter rendering function that uses the global registry if available
                let render_parameter =
                    |param: &ParameterBlock| {
                        // Try to get the global registry
                        if let Some(registry) = ParameterRendererRegistry::global() {
                            // Use the registry to render the parameter with theme
                            registry.render_parameter(
                                &block.name,
                                &param.name,
                                &param.value,
                                &cx.theme(),
                            )
                        } else {
                            // Fallback to default rendering if no registry is available
                            div()
                                .rounded_md()
                                .px_2()
                                .py_1()
                                .mr_1()
                                .mb_1() // Add margin to allow wrapping
                                .text_size(px(16.))
                                .bg(crate::ui::gpui::theme::colors::tool_parameter_bg(
                                    &cx.theme(),
                                ))
                                .child(div().flex().flex_row().items_center().gap_1().children(
                                    vec![
                                    div()
                                        .font_weight(FontWeight(500.0))
                                        .text_color(
                                            crate::ui::gpui::theme::colors::tool_parameter_label(
                                                &cx.theme(),
                                            ),
                                        )
                                        .child(format!("{}:", param.name))
                                        .into_any(),
                                    div()
                                        .text_color(
                                            crate::ui::gpui::theme::colors::tool_parameter_value(
                                                &cx.theme(),
                                            ),
                                        )
                                        .child(param.value.clone())
                                        .into_any(),
                                ],
                                ))
                                .into_any_element()
                        }
                    };

                // Separate parameters into regular and full-width
                let registry = ParameterRendererRegistry::global();

                let (regular_params, fullwidth_params): (
                    Vec<&ParameterBlock>,
                    Vec<&ParameterBlock>,
                ) = block.parameters.iter().partition(|param| {
                    !registry.as_ref().map_or(false, |reg| {
                        reg.get_renderer(&block.name, &param.name)
                            .is_full_width(&block.name, &param.name)
                    })
                });

                div()
                    .rounded(px(3.))
                    .mb_2()
                    .bg(tool_bg)
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .children(vec![
                        div()
                            .w(px(3.))
                            .flex_none()
                            .h_full()
                            .bg(border_color)
                            .rounded_l(px(3.)),
                        div().flex_grow().h_full().child(
                            div().size_full().flex().flex_col().p_1().children({
                                let mut elements = Vec::new();

                                // First row: Tool header with icon, name, and regular parameters
                                elements.push(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center() // Align all items center
                                        .justify_between() // Space between header and chevron
                                        .cursor_pointer() // Make entire header clickable
                                        .hover(|s| s.bg(border_color.opacity(0.1))) // Hover effect
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |view, _event, _window, cx| {
                                                view.toggle_tool_collapsed(cx);
                                            }),
                                        )
                                        .children(vec![
                                            // Left side: Tool icon, name and regular parameters
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .flex_grow()
                                                .children(vec![
                                                    // Tool icon
                                                    file_icons::render_icon_container(
                                                        &icon, 16.0, icon_color, "🔧",
                                                    )
                                                    .mx_2()
                                                    .into_any(),
                                                    // Tool name
                                                    div()
                                                        .font_weight(FontWeight(700.0))
                                                        .text_color(icon_color)
                                                        .mr_2()
                                                        .flex_none() // Prevent shrinking
                                                        .child(block.name.clone())
                                                        .into_any(),
                                                    // Regular parameters
                                                    div()
                                                        .flex()
                                                        .flex_wrap()
                                                        .gap_1()
                                                        .flex_grow() // Take remaining space
                                                        .children(
                                                            regular_params
                                                                .iter()
                                                                .map(|param| render_parameter(param)),
                                                        )
                                                        .into_any(),
                                                ])
                                                .into_any(),
                                            // Right side: Chevron icon
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .flex_none()
                                                .cursor_pointer()
                                                .size(px(24.))
                                                .rounded_full()
                                                .hover(|s| s.bg(border_color.opacity(0.2)))
                                                .child(file_icons::render_icon(
                                                    &chevron_icon,
                                                    16.0,
                                                    chevron_color,
                                                    chevron_text,
                                                ))
                                                .into_any(),
                                        ])
                                        .into_any(),
                                );

                                // Second row: Full-width parameters (if any)
                                if !fullwidth_params.is_empty() {
                                    elements.push(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .w_full()
                                            .mt_1() // Add margin between rows
                                            .children(
                                                fullwidth_params
                                                    .iter()
                                                    .map(|param| render_parameter(param)),
                                            )
                                            .into_any(),
                                    );
                                }

                                // Tool output content (only shown when expanded or on error)
                                // Error message (always shown for error status)
                                if block.status == crate::ui::ToolStatus::Error && block.status_message.is_some() {
                                    elements.push(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .p_2()
                                            .mt_1()
                                            .rounded_md()
                                            .max_h(px(300.)) // Max height
                                            .overflow_y_auto() // Add vertical scrolling
                                            .bg(cx.theme().danger.opacity(0.2))
                                            .border_l_2()
                                            .border_color(cx.theme().danger.opacity(0.5))
                                            .text_color(cx.theme().danger.opacity(0.9))
                                            .text_size(px(14.))
                                            .child(block.status_message.clone().unwrap_or_default())
                                            .into_any(),
                                    );
                                }
                                // Success message (only when expanded)
                                else if !block.is_collapsed && block.status_message.is_some() {
                                    elements.push(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .p_2()
                                            .mt_1()
                                            .rounded_md()
                                            .max_h(px(300.)) // Max height
                                            .overflow_y_auto() // Add vertical scrolling
                                            .bg(cx.theme().success.opacity(0.1))
                                            .border_l_2()
                                            .border_color(cx.theme().success.opacity(0.3))
                                            .text_color(cx.theme().foreground)
                                            .text_size(px(14.))
                                            .child(block.status_message.clone().unwrap_or_default())
                                            .into_any(),
                                    );
                                }

                                elements
                            }),
                        ),
                    ])
                    .shadow_sm()
                    .into_any_element()
            }
        }
    }
}

/// Regular text block
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub content: String,
}

/// Thinking text block with collapsible content
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub content: String,
    pub is_collapsed: bool,
    pub is_completed: bool,
    pub start_time: std::time::Instant,
    pub end_time: std::time::Instant,
}

impl ThinkingBlock {
    pub fn new(content: String) -> Self {
        let now = std::time::Instant::now();
        Self {
            content,
            is_collapsed: true,  // Default is collapsed
            is_completed: false, // Default is not completed
            start_time: now,
            end_time: now, // Initially same as start_time
        }
    }

    pub fn formatted_duration(&self) -> String {
        // Calculate duration based on status
        let duration = if self.is_completed {
            // For completed blocks, use the stored end_time
            self.end_time.duration_since(self.start_time)
        } else {
            // For ongoing blocks, show elapsed time
            self.start_time.elapsed()
        };

        if duration.as_secs() < 60 {
            format!("{}s", duration.as_secs())
        } else {
            let minutes = duration.as_secs() / 60;
            let seconds = duration.as_secs() % 60;
            format!("{}m{}s", minutes, seconds)
        }
    }
}

/// Tool use block with name and parameters
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    pub name: String,
    pub id: String,
    pub parameters: Vec<ParameterBlock>,
    pub status: ToolStatus,
    pub status_message: Option<String>,
    pub is_collapsed: bool,
}

/// Parameter for a tool
#[derive(Debug, Clone)]
pub struct ParameterBlock {
    pub name: String,
    pub value: String,
}
