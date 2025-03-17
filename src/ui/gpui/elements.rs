use crate::ui::gpui::file_icons;
use crate::ui::ToolStatus;
use gpui::{div, hsla, px, white, IntoElement, SharedString};
use gpui::{prelude::*, FontWeight};
use std::sync::{Arc, Mutex};

/// Container for all elements within a message
#[derive(Clone)]
pub struct MessageContainer {
    elements: Arc<Mutex<Vec<MessageElement>>>,
}

impl MessageContainer {
    pub fn new() -> Self {
        Self {
            elements: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn elements(&self) -> Vec<MessageElement> {
        let elements = self.elements.lock().unwrap();
        elements.clone()
    }

    // Add a new text block
    pub fn add_text_block(&self, content: impl Into<String>) {
        let mut elements = self.elements.lock().unwrap();
        elements.push(MessageElement::TextBlock(TextBlock {
            content: content.into(),
        }));
    }

    // Add a new thinking block
    #[allow(dead_code)]
    pub fn add_thinking_block(&self, content: impl Into<String>) {
        let mut elements = self.elements.lock().unwrap();
        elements.push(MessageElement::ThinkingBlock(ThinkingBlock::new(
            content.into(),
        )));
    }

    // Add a new tool use block
    pub fn add_tool_use_block(&self, name: impl Into<String>, id: impl Into<String>) {
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
                _ => {
                    // If the last element is not a thinking block, mark any previous
                    // thinking blocks as completed
                    for element in elements.iter_mut() {
                        if let MessageElement::ThinkingBlock(thinking_block) = element {
                            if !thinking_block.is_completed {
                                thinking_block.is_completed = true;
                                thinking_block.end_time = std::time::Instant::now();
                            }
                        }
                    }
                }
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
                                            // Use the custom rotating animation component
                                            crate::ui::gpui::animations::RotatingArrow::new(
                                                18.0,
                                                hsla(280., 0.6, 0.6, 1.0), // Purple
                                            )
                                            .into_element()
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
                    ToolStatus::Pending => hsla(0., 0., 0.5, 1.0), // Gray
                    ToolStatus::Running => hsla(210., 0.7, 0.7, 1.0), // Blue
                    ToolStatus::Success => hsla(120., 0.7, 0.5, 1.0), // Green
                    ToolStatus::Error => hsla(0., 0.7, 0.5, 1.0),  // Red
                };

                // Border color based on status (more subtle indication)
                let border_color = match block.status {
                    ToolStatus::Success => hsla(120., 0.3, 0.5, 0.4), // Success: light green
                    ToolStatus::Error => hsla(0., 0.3, 0.5, 0.4),     // Error: light red
                    _ => hsla(210., 0.5, 0.5, 0.3), // Others: light blue (default)
                };

                // Render parameter badges for a more compact display
                let render_parameter_badge = |param: &ParameterBlock| {
                    div()
                        .rounded_md()
                        .px_2()
                        .py_1()
                        .mr_1()
                        .mb_1() // Add margin to allow wrapping
                        .text_sm()
                        .bg(hsla(210., 0.1, 0.3, 0.3))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .children(vec![
                                    div()
                                        .font_weight(FontWeight(500.0))
                                        .text_color(hsla(210., 0.5, 0.8, 1.0))
                                        .child(format!("{}:", param.name))
                                        .into_any(),
                                    div()
                                        .text_color(white())
                                        .child(param.value.clone())
                                        .into_any(),
                                ]),
                        )
                        .into_any_element()
                };

                div()
                    .border_1()
                    .border_color(border_color)
                    .rounded_md()
                    .p_2()
                    .mb_2()
                    .bg(hsla(210., 0.1, 0.2, 0.2)) // Very light blue background
                    .flex()
                    .flex_col()
                    .children(vec![
                        // Tool header: icon and name in a row
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .mb_2() // Margin below header
                            .children(vec![
                                // Tool icon
                                file_icons::render_icon_container(&icon, 18.0, icon_color, "🔧")
                                    .mr_2()
                                    .into_any(),
                                // Tool name
                                div()
                                    .font_weight(FontWeight(700.0))
                                    .text_color(hsla(210., 0.7, 0.7, 1.0))
                                    .mr_2()
                                    .flex_none() // Prevent shrinking
                                    .child(block.name)
                                    .into_any(),
                                // Parameters in a flex wrap container
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .flex_grow() // Take remaining space
                                    .children(block.parameters.iter().map(render_parameter_badge))
                                    .into_any(),
                            ])
                            .into_any(),
                        // Error message (only shown for error status)
                        if block.status == ToolStatus::Error {
                            if let Some(msg) = &block.status_message {
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
                                    .text_sm()
                                    .child(msg.clone())
                                    .into_any()
                            } else {
                                div().into_any() // Empty element if no message
                            }
                        } else {
                            div().into_any() // Empty element for non-error status
                        },
                    ])
                    .into_any()
            }
        }
    }
}
