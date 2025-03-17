use crate::ui::gpui::file_icons;
use crate::ui::ToolStatus;
use gpui::{div, hsla, px, rgba, white, IntoElement};
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
        elements.push(MessageElement::ThinkingBlock(ThinkingBlock {
            content: content.into(),
        }));
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
                _ => {}
            }
        }

        // If we reach here, we need to add a new thinking block
        elements.push(MessageElement::ThinkingBlock(ThinkingBlock { content }));
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

/// Thinking text block (displayed in italic/different color)
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub content: String,
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
                div()
                    .italic()
                    .text_color(hsla(0., 0., 0.7, 0.8)) // Light gray color
                    .child(block.content)
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
