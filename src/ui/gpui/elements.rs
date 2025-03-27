use crate::ui::gpui::file_icons;
use crate::ui::gpui::parameter_renderers::ParameterRendererRegistry;
use crate::ui::ToolStatus;
use gpui::{
    bounce, div, ease_in_out, hsla, percentage, px, rgba, svg, white, Animation, AnimationExt,
    IntoElement, SharedString, Styled, Transformation,
};
use gpui::{prelude::*, FontWeight};
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
    elements: Arc<Mutex<Vec<MessageElement>>>,
    role: MessageRole,
}

impl MessageContainer {
    pub fn with_role(role: MessageRole) -> Self {
        Self {
            elements: Arc::new(Mutex::new(Vec::new())),
            role,
        }
    }

    /// Get the role of this message container
    pub fn role(&self) -> MessageRole {
        self.role.clone()
    }

    /// Check if this is a user message
    pub fn is_user_message(&self) -> bool {
        self.role == MessageRole::User
    }

    pub fn elements(&self) -> Vec<MessageElement> {
        let elements = self.elements.lock().unwrap();
        elements.clone()
    }

    // Add a new text block
    pub fn add_text_block(&self, content: impl Into<String>) {
        self.finish_any_thinking_blocks();
        let mut elements = self.elements.lock().unwrap();
        elements.push(MessageElement::TextBlock(TextBlock {
            content: content.into(),
        }));
    }

    // Add a new thinking block
    #[allow(dead_code)]
    pub fn add_thinking_block(&self, content: impl Into<String>) {
        self.finish_any_thinking_blocks();
        let mut elements = self.elements.lock().unwrap();
        elements.push(MessageElement::ThinkingBlock(ThinkingBlock::new(
            content.into(),
        )));
    }

    // Add a new tool use block
    pub fn add_tool_use_block(&self, name: impl Into<String>, id: impl Into<String>) {
        self.finish_any_thinking_blocks();
        let mut elements = self.elements.lock().unwrap();
        elements.push(MessageElement::ToolUse(ToolUseBlock {
            name: name.into(),
            id: id.into(),
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
        }));
    }

    // Update the status of a tool block
    pub fn update_tool_status(
        &self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
    ) -> bool {
        let mut elements = self.elements.lock().unwrap();

        for element in elements.iter_mut() {
            if let MessageElement::ToolUse(tool) = element {
                if tool.id == tool_id {
                    tool.status = status;
                    tool.status_message = message;
                    return true;
                }
            }
        }

        false // No matching tool found
    }

    // Add or append to text block
    pub fn add_or_append_to_text_block(&self, content: impl Into<String>) {
        self.finish_any_thinking_blocks();

        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last_mut() {
            match last {
                MessageElement::TextBlock(block) => {
                    // Append to existing text block
                    block.content.push_str(&content);
                    return;
                }
                _ => {}
            }
        }

        // If we reach here, we need to add a new text block
        elements.push(MessageElement::TextBlock(TextBlock { content }));
    }

    // Add or append to thinking block
    pub fn add_or_append_to_thinking_block(&self, content: impl Into<String>) {
        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last_mut() {
            match last {
                MessageElement::ThinkingBlock(block) => {
                    // Append to existing thinking block
                    block.content.push_str(&content);
                    return;
                }
                _ => {}
            }
        }

        // If we reach here, we need to add a new thinking block
        elements.push(MessageElement::ThinkingBlock(ThinkingBlock::new(content)));
    }

    // Add or update tool parameter
    pub fn add_or_update_tool_parameter(
        &self,
        tool_id: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
    ) {
        let tool_id = tool_id.into();
        let name = name.into();
        let value = value.into();

        let mut elements = self.elements.lock().unwrap();

        // Find the tool block with matching ID
        for element in elements.iter_mut().rev() {
            if let MessageElement::ToolUse(tool) = element {
                if tool.id == tool_id {
                    // Check if parameter with this name already exists
                    for param in tool.parameters.iter_mut() {
                        if param.name == name {
                            // Update existing parameter
                            param.value.push_str(&value);
                            return;
                        }
                    }

                    // Add new parameter
                    tool.parameters.push(ParameterBlock { name, value });
                    return;
                }
            }
        }

        // If we didn't find a matching tool, create a new one with this parameter
        let mut tool = ToolUseBlock {
            name: "unknown".to_string(), // Default name since we only have ID
            id: tool_id,
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
        };

        tool.parameters.push(ParameterBlock { name, value });

        elements.push(MessageElement::ToolUse(tool));
    }

    // Mark a tool as ended (could add visual indicator)
    pub fn end_tool_use(&self, id: impl Into<String>) {
        // Currently no specific action needed, but could add visual indicator
        // that the tool execution is complete
        let _id = id.into();
    }

    fn finish_any_thinking_blocks(&self) {
        let mut elements = self.elements.lock().unwrap();
        // Mark any previous thinking blocks as completed
        for element in elements.iter_mut() {
            if let MessageElement::ThinkingBlock(thinking_block) = element {
                if !thinking_block.is_completed {
                    thinking_block.is_completed = true;
                    thinking_block.end_time = std::time::Instant::now();
                }
            }
        }
    }

    // Toggle a thinking block's collapsed state by its index
    pub fn toggle_thinking_collapsed(&self, index: usize) -> bool {
        let mut elements = self.elements.lock().unwrap();
        let mut changed = false;

        // Get a count of thinking blocks to find the correct one
        let mut thinking_index = 0;

        for element in elements.iter_mut() {
            if let MessageElement::ThinkingBlock(block) = element {
                if thinking_index == index {
                    block.is_collapsed = !block.is_collapsed;
                    changed = true;
                    break;
                }
                thinking_index += 1;
            }
        }

        changed
    }
}

/// Different types of elements that can appear in a message
#[derive(Debug, Clone)]
pub enum MessageElement {
    TextBlock(TextBlock),
    ThinkingBlock(ThinkingBlock),
    ToolUse(ToolUseBlock),
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
}

/// Parameter for a tool
#[derive(Debug, Clone)]
pub struct ParameterBlock {
    pub name: String,
    pub value: String,
}

// Renderer implementation for MessageElement
impl IntoElement for MessageElement {
    type Element = gpui::AnyElement;

    fn into_element(self) -> Self::Element {
        match self {
            MessageElement::TextBlock(block) => {
                div().text_color(white()).child(block.content).into_any()
            }
            MessageElement::ThinkingBlock(block) => {
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

                // Create the thinking block container
                div()
                    .rounded_md()
                    .p_2()
                    .mb_2()
                    .bg(hsla(280., 0.1, 0.2, 0.2)) // Very light purple background for thinking
                    .border_1()
                    .border_color(hsla(280., 0.3, 0.5, 0.3))
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
                                                &icon,
                                                18.0,
                                                hsla(280., 0.6, 0.6, 1.0), // Purple
                                                icon_text,
                                            )
                                            .into_any()
                                        } else {
                                            svg()
                                                .size(px(18.))
                                                .path(SharedString::from("icons/arrow_circle.svg"))
                                                .text_color(hsla(280., 0.6, 0.6, 1.0))
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
                                            .text_color(hsla(280., 0.5, 0.7, 1.0)) // Purple text
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
                                    .hover(|s| s.bg(hsla(280., 0.2, 0.3, 0.2)))
                                    .child(file_icons::render_icon(
                                        &chevron_icon,
                                        16.0,
                                        hsla(280., 0.5, 0.7, 1.0), // Purple
                                        chevron_text,
                                    ))
                                    // GPUI has limited custom attribute support
                                    // We'll identify it by position in the message instead
                                    .into_any(),
                            ])
                            .into_any(),
                        // Content (only shown when expanded)
                        if !block.is_collapsed {
                            div()
                                .pt_2()
                                .italic()
                                .text_size(px(16.))
                                .text_color(hsla(0., 0., 0.8, 0.9)) // Light gray color
                                // Can't easily set whitespace style, just use normal text
                                .border_t_1()
                                .border_color(hsla(280., 0.3, 0.5, 0.2))
                                .child(block.content.clone())
                                .into_any()
                        } else {
                            div().into_any() // Empty div when collapsed
                        },
                    ])
                    .into_any()
            }
            MessageElement::ToolUse(block) => {
                // Get the appropriate icon for this tool type
                let icon = file_icons::get().get_tool_icon(&block.name);

                // Get color based on status
                let icon_color = match block.status {
                    crate::ui::ToolStatus::Error => rgba(0xFD8E3FE0),
                    _ => rgba(0xFFFFFFAA),
                };

                // Border color based on status (more subtle indication)
                let border_color = match block.status {
                    crate::ui::ToolStatus::Pending => rgba(0x666666FF),
                    crate::ui::ToolStatus::Running => rgba(0x56BBF6FF),
                    crate::ui::ToolStatus::Success => rgba(0x47D136FF),
                    crate::ui::ToolStatus::Error => rgba(0xFD8E3FFF),
                };

                // Parameter rendering function that uses the global registry if available
                let render_parameter =
                    |param: &ParameterBlock| {
                        // Try to get the global registry
                        if let Some(registry) = ParameterRendererRegistry::global() {
                            // Use the registry to render the parameter
                            registry.render_parameter(&block.name, &param.name, &param.value)
                        } else {
                            // Fallback to default rendering if no registry is available
                            div()
                                .rounded_md()
                                .px_2()
                                .py_1()
                                .mr_1()
                                .mb_1() // Add margin to allow wrapping
                                .text_size(px(16.))
                                .bg(hsla(210., 0.1, 0.3, 0.3))
                                .child(div().flex().flex_row().items_center().gap_1().children(
                                    vec![
                                        div()
                                            .font_weight(FontWeight(500.0))
                                            .text_color(hsla(210., 0.5, 0.8, 1.0))
                                            .child(format!("{}:", param.name))
                                            .into_any(),
                                        div()
                                            .text_color(white())
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
                    .bg(rgba(0x161616FF))
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .children(vec![
                        div().w(px(3.)).h_full().bg(border_color).rounded_l(px(3.)),
                        div().flex_grow().h_full().child(
                            div().size_full().flex().flex_col().p_1().children({
                                let mut elements = Vec::new();

                                // First row: Tool header with icon, name, and regular parameters
                                elements.push(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_start() // Align to top if multiple parameters
                                        .children(vec![
                                            // Left side: Tool icon and name
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .flex_none()
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
                                                ])
                                                .into_any(),
                                            // Right side: Regular parameters
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .flex_grow() // Take remaining space
                                                .children(
                                                    regular_params
                                                        .iter()
                                                        .map(|param| render_parameter(param)),
                                                )
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

                                // Error message (only shown for error status)
                                if block.status == crate::ui::ToolStatus::Error {
                                    if let Some(msg) = &block.status_message {
                                        elements.push(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .p_2()
                                                .rounded_md()
                                                .bg(hsla(0., 0.15, 0.2, 0.2)) // Light red background for errors
                                                .border_l_2()
                                                .border_color(hsla(0., 0.5, 0.5, 0.5))
                                                .text_color(hsla(0., 0.3, 0.9, 1.0))
                                                .text_size(px(14.))
                                                .child(msg.clone())
                                                .into_any(),
                                        );
                                    }
                                }

                                elements
                            }),
                        ),
                    ])
                    .shadow_md()
                    .into_any()
            }
        }
    }
}
