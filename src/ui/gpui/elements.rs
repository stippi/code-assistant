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
                // Get status info
                let (status_color, status_icon) = match block.status {
                    ToolStatus::Pending => (rgba(0x404040), "⋯"), // Gray, ellipsis
                    ToolStatus::Running => (rgba(0x0000ff), "⚙"), // Blue, gear
                    ToolStatus::Success => (rgba(0x00ff00), "✓"), // Green, checkmark
                    ToolStatus::Error => (rgba(0xff0000), "✗"),   // Red, x
                };

                // Border color based on status (more subtle indication)
                let border_color = match block.status {
                    ToolStatus::Success => hsla(120., 0.3, 0.5, 0.4), // Success: light green
                    ToolStatus::Error => hsla(0., 0.3, 0.5, 0.4),     // Error: light red
                    _ => hsla(210., 0.5, 0.5, 0.3), // Others: light blue (default)
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
                        // Tool name header
                        div()
                            .font_weight(FontWeight(700.0))
                            .text_color(hsla(210., 0.7, 0.7, 1.0)) // Blue text
                            .mb_1()
                            .child(format!("🔧 {}", block.name))
                            .into_any(),
                        // Parameters
                        div()
                            .flex()
                            .flex_col()
                            .pl_2()
                            .mb_2() // Add margin to separate parameters from status
                            .children(block.parameters.iter().map(|param| {
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_start()
                                    .mb_1()
                                    .children(vec![
                                        div()
                                            .font_weight(FontWeight(500.0))
                                            .text_color(hsla(210., 0.5, 0.8, 1.0)) // Light blue text
                                            .min_w(px(100.))
                                            .mr_2()
                                            .child(format!("{}:", param.name.clone()))
                                            .into_any(),
                                        div()
                                            .text_color(white())
                                            .flex_1()
                                            .child(param.value.clone())
                                            .into_any(),
                                    ])
                                    .into_any()
                            }))
                            .into_any(),
                        // Status indicator below parameters
                        if block.status != ToolStatus::Pending {
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .pl_2()
                                .gap_2()
                                .children(vec![
                                    // Status circle with icon
                                    div()
                                        .w(px(16.0))
                                        .h(px(16.0))
                                        .rounded_full()
                                        .bg(status_color)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(white())
                                        .text_xs()
                                        .font_weight(FontWeight(700.0))
                                        .child(status_icon)
                                        .into_any(),
                                    // Status message (if any)
                                    if let Some(msg) = &block.status_message {
                                        div().flex_1().text_sm().child(msg.clone()).into_any()
                                    } else {
                                        div().into_any() // Empty element
                                    },
                                ])
                                .into_any()
                        } else {
                            div().h(px(0.0)).into_any() // Empty element for pending status
                        },
                    ])
                    .into_any()
            }
        }
    }
}
