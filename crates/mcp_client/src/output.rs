//! Tool output for MCP-backed tools.

use rmcp::model::CallToolResult;
use serde::{Deserialize, Serialize};
use tools_core::render::{ImageData, Render, ResourcesTracker};
use tools_core::result::ToolResult;

/// The result of one MCP tool call, reduced to what the agent loop needs:
/// text for the LLM, an error flag, and any images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolOutput {
    pub text: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<McpImage>,
}

/// Image content returned by an MCP tool (base64-encoded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpImage {
    pub media_type: String,
    pub base64_data: String,
}

impl McpToolOutput {
    /// Reduce an MCP `CallToolResult` to text + images. Text blocks are
    /// concatenated; resource/audio blocks are passed through as their raw
    /// JSON; `structured_content` is used when no content blocks are present.
    pub fn from_call_result(result: &CallToolResult) -> Self {
        use rmcp::model::ContentBlock;
        let mut text_parts = Vec::new();
        let mut images = Vec::new();
        for block in &result.content {
            match block {
                ContentBlock::Text(text) => text_parts.push(text.text.clone()),
                ContentBlock::Image(image) => images.push(McpImage {
                    media_type: image.mime_type.clone(),
                    base64_data: image.data.clone(),
                }),
                other => {
                    // Resource/audio/… content: pass through as raw JSON
                    // rather than dropping it silently.
                    if let Ok(json) = serde_json::to_string(other) {
                        text_parts.push(json);
                    }
                }
            }
        }
        if text_parts.is_empty() {
            if let Some(structured) = &result.structured_content {
                text_parts.push(
                    serde_json::to_string_pretty(structured).unwrap_or_else(|_| String::new()),
                );
            }
        }
        Self {
            text: text_parts.join("\n"),
            is_error: result.is_error.unwrap_or(false),
            images,
        }
    }

    /// An error output for a failed round-trip (server dead, timeout, …).
    pub fn transport_error(message: String) -> Self {
        Self {
            text: message,
            is_error: true,
            images: Vec::new(),
        }
    }
}

impl ToolResult for McpToolOutput {
    fn is_success(&self) -> bool {
        !self.is_error
    }
}

impl Render for McpToolOutput {
    fn status(&self) -> String {
        let first_line = self.text.lines().find(|line| !line.trim().is_empty());
        let mut status = match first_line {
            Some(line) if line.chars().count() > 80 => {
                format!("{}…", line.chars().take(80).collect::<String>())
            }
            Some(line) => line.to_string(),
            None if self.images.is_empty() => "(no output)".to_string(),
            None => format!("{} image(s)", self.images.len()),
        };
        if self.is_error {
            status = format!("Failed: {status}");
        }
        status
    }

    fn render(&self, _resources_tracker: &mut ResourcesTracker) -> String {
        if self.text.is_empty() && self.images.is_empty() {
            "(no output)".to_string()
        } else {
            self.text.clone()
        }
    }

    fn render_images(&self) -> Vec<ImageData> {
        self.images
            .iter()
            .map(|image| ImageData {
                media_type: image.media_type.clone(),
                base64_data: image.base64_data.clone(),
            })
            .collect()
    }
}
