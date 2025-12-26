use crate::ui::streaming::DisplayFragment;
use agent_client_protocol as acp;

/// Convert a DisplayFragment to an ACP ContentBlock
pub fn fragment_to_content_block(fragment: &DisplayFragment) -> acp::ContentBlock {
    match fragment {
        DisplayFragment::PlainText(text) => {
            acp::ContentBlock::Text(acp::TextContent::new(text.clone()))
        }
        // Thinking text is just regular text in ACP (no special annotation)
        DisplayFragment::ThinkingText(text) => {
            acp::ContentBlock::Text(acp::TextContent::new(text.clone()))
        }
        DisplayFragment::CompactionDivider { summary } => acp::ContentBlock::Text(
            acp::TextContent::new(format!("Conversation compacted:\n{summary}")),
        ),
        DisplayFragment::Image { media_type, data } => {
            acp::ContentBlock::Image(acp::ImageContent::new(data.clone(), media_type.clone()))
        }

        // Tool-related fragments are not converted to content blocks
        // They are handled separately as ToolCall updates
        DisplayFragment::ToolName { .. }
        | DisplayFragment::ToolParameter { .. }
        | DisplayFragment::ToolEnd { .. }
        | DisplayFragment::ToolOutput { .. }
        | DisplayFragment::ToolTerminal { .. }
        | DisplayFragment::ReasoningSummaryStart
        | DisplayFragment::ReasoningSummaryDelta(_)
        | DisplayFragment::ReasoningComplete
        | DisplayFragment::HiddenToolCompleted => {
            // These should not be converted to content blocks
            // Return empty text as placeholder
            acp::ContentBlock::Text(acp::TextContent::new(String::new()))
        }
    }
}

/// Map tool name to ACP ToolKind
pub fn map_tool_kind(tool_name: &str) -> acp::ToolKind {
    match tool_name {
        "read_files" | "list_files" => acp::ToolKind::Read,
        "write_file" | "edit" | "replace_in_file" => acp::ToolKind::Edit,
        "execute_command" => acp::ToolKind::Execute,
        "web_search" | "glob_files" | "search_files" | "perplexity_ask" => acp::ToolKind::Search,
        _ => acp::ToolKind::Other,
    }
}

/// Map tool status
pub fn map_tool_status(status: crate::ui::ToolStatus) -> acp::ToolCallStatus {
    match status {
        crate::ui::ToolStatus::Pending => acp::ToolCallStatus::Pending,
        crate::ui::ToolStatus::Running => acp::ToolCallStatus::InProgress,
        crate::ui::ToolStatus::Success => acp::ToolCallStatus::Completed,
        crate::ui::ToolStatus::Error => acp::ToolCallStatus::Failed,
    }
}

/// Convert prompt content blocks from ACP to internal llm::ContentBlock format
pub fn convert_prompt_to_content_blocks(
    prompt: Vec<acp::ContentBlock>,
    base_path: Option<&std::path::Path>,
) -> Vec<llm::ContentBlock> {
    prompt
        .into_iter()
        .filter_map(|block| match block {
            acp::ContentBlock::Text(text_content) => {
                Some(llm::ContentBlock::new_text(text_content.text))
            }

            acp::ContentBlock::Image(image_content) => Some(llm::ContentBlock::Image {
                media_type: image_content.mime_type,
                data: image_content.data,
                start_time: None,
                end_time: None,
            }),

            acp::ContentBlock::Resource(embedded_resource) => {
                // Convert embedded resource to text with file context
                convert_embedded_resource_to_text(embedded_resource, base_path)
            }

            // Other content types (ResourceLink, Audio) not yet supported
            _ => None,
        })
        .collect()
}

/// Convert an embedded resource to a text content block with file context
fn convert_embedded_resource_to_text(
    embedded_resource: acp::EmbeddedResource,
    base_path: Option<&std::path::Path>,
) -> Option<llm::ContentBlock> {
    match embedded_resource.resource {
        acp::EmbeddedResourceResource::TextResourceContents(text_resource) => {
            // Format the resource as a code block with file path context
            let uri = &text_resource.uri;
            let text = &text_resource.text;

            // Extract file path and line range from URI if present
            // URI format: file:///path/to/file.ext#L1:10 or file:///path/to/file.ext
            let (file_path, line_info) = parse_file_uri(uri);

            // Make path relative to base_path if possible
            let display_path = make_relative_path(&file_path, base_path);

            // Build a contextual text block using read_files compatible format: path:start-end
            let path_with_lines = if let Some(lines) = line_info {
                format!("{display_path}:{lines}")
            } else {
                display_path
            };

            let formatted_text = format!("Content from `{path_with_lines}`:\n```\n{text}\n```");

            Some(llm::ContentBlock::new_text(formatted_text))
        }
        acp::EmbeddedResourceResource::BlobResourceContents(blob_resource) => {
            // For blob resources, we can try to include them as base64 data
            // but for now just note that it's a binary file
            let uri = &blob_resource.uri;
            let (file_path, _) = parse_file_uri(uri);

            let display_path = make_relative_path(&file_path, base_path);

            let formatted_text =
                format!("[Binary content from `{display_path}` - base64 encoded, not displayed]");

            Some(llm::ContentBlock::new_text(formatted_text))
        }
        // EmbeddedResourceResource is non-exhaustive, handle unknown variants
        _ => None,
    }
}

/// Parse a file URI to extract the path and optional line range
fn parse_file_uri(uri: &str) -> (String, Option<String>) {
    // Handle file:// URIs
    let path_part = if let Some(stripped) = uri.strip_prefix("file://") {
        stripped.to_string()
    } else {
        uri.to_string()
    };

    // Split on # to get the fragment (line info)
    if let Some((path, fragment)) = path_part.split_once('#') {
        // Fragment might be like "L14:36" meaning lines 14-36
        let line_info = fragment
            .strip_prefix('L')
            .map(|s| s.replace(':', "-"))
            .unwrap_or_else(|| fragment.to_string());

        (path.to_string(), Some(line_info))
    } else {
        (path_part, None)
    }
}

/// Make an absolute path relative to a base path if possible.
/// Returns the original path as a string if it can't be made relative.
fn make_relative_path(absolute_path: &str, base_path: Option<&std::path::Path>) -> String {
    let Some(base) = base_path else {
        return absolute_path.to_string();
    };

    let path = std::path::Path::new(absolute_path);

    // Try to strip the base path prefix
    path.strip_prefix(base)
        .map(|rel| rel.to_string_lossy().to_string())
        .unwrap_or_else(|_| absolute_path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::streaming::DisplayFragment;

    #[test]
    fn prompt_conversion_handles_text_and_images() {
        let prompt = vec![
            acp::ContentBlock::Text(acp::TextContent::new("hello")),
            acp::ContentBlock::Image(acp::ImageContent::new("image-data", "image/png")),
        ];

        let blocks = convert_prompt_to_content_blocks(prompt, None);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            llm::ContentBlock::Text { text, .. } => assert_eq!(text, "hello"),
            other => panic!("expected text block, got {other:?}"),
        }
        match &blocks[1] {
            llm::ContentBlock::Image {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "image-data");
            }
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_fragments_convert_to_empty_text() {
        let fragment = DisplayFragment::ReasoningSummaryDelta("thought".into());
        let block = fragment_to_content_block(&fragment);
        match block {
            acp::ContentBlock::Text(text) => assert!(text.text.is_empty()),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn map_tool_kind_matches_known_names() {
        assert_eq!(map_tool_kind("execute_command"), acp::ToolKind::Execute);
        assert_eq!(map_tool_kind("read_files"), acp::ToolKind::Read);
        assert_eq!(map_tool_kind("unknown_tool"), acp::ToolKind::Other);
    }

    #[test]
    fn prompt_conversion_handles_embedded_resources_without_base_path() {
        // TextResourceContents::new takes (text, uri)
        let text_resource = acp::TextResourceContents::new(
            "fn main() {\n    println!(\"Hello\");\n}",
            "file:///path/to/file.rs#L10:20",
        );
        let embedded = acp::EmbeddedResource::new(
            acp::EmbeddedResourceResource::TextResourceContents(text_resource),
        );
        let prompt = vec![acp::ContentBlock::Resource(embedded)];

        // Without base_path, uses absolute path
        let blocks = convert_prompt_to_content_blocks(prompt, None);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            llm::ContentBlock::Text { text, .. } => {
                // Format: "Content from `path:start-end`:\n```\n<code>\n```"
                assert_eq!(
                    text,
                    "Content from `/path/to/file.rs:10-20`:\n```\nfn main() {\n    println!(\"Hello\");\n}\n```"
                );
            }
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn prompt_conversion_handles_embedded_resources_with_base_path() {
        let text_resource = acp::TextResourceContents::new(
            "fn main() {}",
            "file:///workspace/project/src/main.rs#L1:5",
        );
        let embedded = acp::EmbeddedResource::new(
            acp::EmbeddedResourceResource::TextResourceContents(text_resource),
        );
        let prompt = vec![acp::ContentBlock::Resource(embedded)];

        // With base_path, produces relative path (consistent with read_files tool)
        let base = std::path::Path::new("/workspace/project");
        let blocks = convert_prompt_to_content_blocks(prompt, Some(base));
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            llm::ContentBlock::Text { text, .. } => {
                assert_eq!(
                    text,
                    "Content from `src/main.rs:1-5`:\n```\nfn main() {}\n```"
                );
            }
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn parse_file_uri_extracts_path_and_lines() {
        let (path, lines) = super::parse_file_uri("file:///Users/test/code.rs#L14:36");
        assert_eq!(path, "/Users/test/code.rs");
        assert_eq!(lines, Some("14-36".to_string()));

        let (path2, lines2) = super::parse_file_uri("file:///simple/path.txt");
        assert_eq!(path2, "/simple/path.txt");
        assert!(lines2.is_none());
    }
}
