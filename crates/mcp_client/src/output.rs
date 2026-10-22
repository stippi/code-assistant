//! Tool output for MCP-backed tools.

use base64::Engine;
use rmcp::model::{CallToolResult, Resource, ResourceContents};
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
    /// concatenated; embedded resources become their text or image, resource
    /// links a line naming the link; `structured_content` is used when no
    /// content blocks are present.
    pub fn from_call_result(result: &CallToolResult) -> Self {
        use rmcp::model::ContentBlock;
        let mut content = Content::default();
        for block in &result.content {
            match block {
                ContentBlock::Text(text) => content.text_parts.push(text.text.clone()),
                ContentBlock::Image(image) => content.images.push(McpImage {
                    media_type: image.mime_type.clone(),
                    base64_data: image.data.clone(),
                }),
                ContentBlock::Resource(embedded) => content.push_resource(&embedded.resource),
                ContentBlock::ResourceLink(link) => content
                    .text_parts
                    .push(format!("[resource link] {}", describe_resource(link))),
                ContentBlock::Audio(audio) => content
                    .text_parts
                    .push(format!("[audio ({}) not shown]", audio.mime_type)),
                other => {
                    // Content types newer than this client: pass through as
                    // raw JSON rather than dropping them silently.
                    if let Ok(json) = serde_json::to_string(other) {
                        content.text_parts.push(json);
                    }
                }
            }
        }
        if content.text_parts.is_empty()
            && let Some(structured) = &result.structured_content
        {
            content
                .text_parts
                .push(serde_json::to_string_pretty(structured).unwrap_or_else(|_| String::new()));
        }
        content.into_output(result.is_error.unwrap_or(false))
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

/// Text and images collected from MCP content blocks.
#[derive(Default)]
struct Content {
    text_parts: Vec<String>,
    images: Vec<McpImage>,
}

/// Image types the LLM providers accept as image input.
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// One resource content, reduced to what the model can use.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourcePart {
    /// Text, or binary data that decodes to UTF-8 (JSON, SVG, …).
    Text {
        uri: String,
        mime_type: Option<String>,
        text: String,
    },
    /// An image type the LLM providers accept.
    Image {
        uri: String,
        media_type: String,
        base64_data: String,
    },
    /// Other binary data, which the model cannot use.
    Binary {
        uri: String,
        mime_type: Option<String>,
        size: usize,
    },
    /// Invalid base64, or a content kind newer than this client.
    Unreadable { uri: String, reason: String },
}

impl ResourcePart {
    pub fn from_contents(contents: &ResourceContents) -> Self {
        match contents {
            ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                ..
            } => Self::Text {
                uri: uri.clone(),
                mime_type: mime_type.clone(),
                text: text.clone(),
            },
            ResourceContents::BlobResourceContents {
                uri,
                mime_type,
                blob,
                ..
            } => {
                if let Some(media_type) = mime_type
                    .as_deref()
                    .filter(|mime_type| IMAGE_TYPES.contains(mime_type))
                {
                    return Self::Image {
                        uri: uri.clone(),
                        media_type: media_type.to_string(),
                        base64_data: blob.clone(),
                    };
                }
                match base64::engine::general_purpose::STANDARD.decode(blob) {
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => Self::Text {
                            uri: uri.clone(),
                            mime_type: mime_type.clone(),
                            text,
                        },
                        Err(error) => Self::Binary {
                            uri: uri.clone(),
                            mime_type: mime_type.clone(),
                            size: error.into_bytes().len(),
                        },
                    },
                    Err(error) => Self::Unreadable {
                        uri: uri.clone(),
                        reason: format!("invalid base64 content: {error}"),
                    },
                }
            }
            other => Self::Unreadable {
                uri: String::new(),
                reason: format!(
                    "unsupported content: {}",
                    serde_json::to_string(other).unwrap_or_default()
                ),
            },
        }
    }

    pub fn uri(&self) -> &str {
        match self {
            Self::Text { uri, .. }
            | Self::Image { uri, .. }
            | Self::Binary { uri, .. }
            | Self::Unreadable { uri, .. } => uri,
        }
    }

    /// Why a [`Self::Binary`] or [`Self::Unreadable`] part cannot be shown.
    pub fn unusable_reason(&self) -> Option<String> {
        match self {
            Self::Binary {
                mime_type, size, ..
            } => Some(format!(
                "binary content ({}, {size} bytes) cannot be shown",
                mime_type.as_deref().unwrap_or("unknown type")
            )),
            Self::Unreadable { reason, .. } => Some(reason.clone()),
            Self::Text { .. } | Self::Image { .. } => None,
        }
    }
}

impl Content {
    /// Add an embedded resource: text as text under a header naming its
    /// URI, a supported image as an image, anything else as a placeholder.
    fn push_resource(&mut self, contents: &ResourceContents) {
        let part = ResourcePart::from_contents(contents);
        if let Some(reason) = part.unusable_reason() {
            self.text_parts
                .push(format!("[resource {}: {reason}]", part.uri()));
            return;
        }
        match part {
            ResourcePart::Text {
                uri,
                mime_type,
                text,
            } => {
                let mime_type = mime_type
                    .map(|mime_type| format!(" ({mime_type})"))
                    .unwrap_or_default();
                self.text_parts
                    .push(format!("--- resource {uri}{mime_type} ---\n{text}"));
            }
            ResourcePart::Image {
                uri,
                media_type,
                base64_data,
            } => {
                self.text_parts
                    .push(format!("[resource {uri}: image below]"));
                self.images.push(McpImage {
                    media_type,
                    base64_data,
                });
            }
            ResourcePart::Binary { .. } | ResourcePart::Unreadable { .. } => {}
        }
    }

    fn into_output(self, is_error: bool) -> McpToolOutput {
        McpToolOutput {
            text: self.text_parts.join("\n"),
            is_error,
            images: self.images,
        }
    }
}

/// One line describing a resource: URI, title or name, MIME type, size and
/// description.
pub(crate) fn describe_resource(resource: &Resource) -> String {
    let mut line = format!(
        "{} — {}",
        resource.uri,
        resource.title.as_deref().unwrap_or(&resource.name)
    );
    let details: Vec<String> = resource
        .mime_type
        .clone()
        .into_iter()
        .chain(resource.size.map(|size| format!("{size} bytes")))
        .collect();
    if !details.is_empty() {
        line.push_str(&format!(" ({})", details.join(", ")));
    }
    if let Some(description) = resource.description.as_deref().filter(|d| !d.is_empty()) {
        line.push_str(": ");
        line.push_str(description);
    }
    line
}

/// Deserialize a persisted MCP tool output without a live server or registry
/// entry. `McpToolOutput` is self-describing, so a recorded MCP execution
/// renders identically in any instance — including one that never connected
/// (or never trusted) the server that produced it.
/// [`McpTool::deserialize_output`] delegates here so there is one code path.
///
/// [`McpTool::deserialize_output`]: crate::tool::McpTool
pub fn deserialize_mcp_output(
    json: serde_json::Value,
) -> anyhow::Result<Box<dyn tools_core::dyn_tool::AnyOutput>> {
    let mut output: McpToolOutput = serde_json::from_value(json)?;
    // Re-check dimensions on load for sessions persisted before capping.
    output.cap_images(tools_core::MAX_IMAGE_EDGE);
    Ok(Box::new(output))
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

    fn cap_images(&mut self, max_edge: u32) {
        for image in &mut self.images {
            if let Some((media_type, base64_data)) =
                tools_core::cap_base64_image(&image.media_type, &image.base64_data, max_edge)
            {
                image.media_type = media_type;
                image.base64_data = base64_data;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_mcp_output_needs_no_live_tool() {
        let json = serde_json::json!({ "text": "42 issues found", "is_error": false });
        let output = deserialize_mcp_output(json).unwrap();
        assert!(output.is_success());
        assert_eq!(output.as_render().status(), "42 issues found");
    }

    #[test]
    fn deserialize_mcp_output_preserves_error_flag() {
        let json = serde_json::json!({ "text": "boom", "is_error": true });
        let output = deserialize_mcp_output(json).unwrap();
        assert!(!output.is_success());
        assert_eq!(output.as_render().status(), "Failed: boom");
    }
}
