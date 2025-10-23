use crate::ui::streaming::DisplayFragment;
use agent_client_protocol as acp;

/// Convert a DisplayFragment to an ACP ContentBlock
pub fn fragment_to_content_block(fragment: &DisplayFragment) -> acp::ContentBlock {
    match fragment {
        DisplayFragment::PlainText(text) => acp::ContentBlock::Text(acp::TextContent {
            annotations: None,
            text: text.clone(),
            meta: None,
        }),
        // Thinking text is just regular text in ACP (no special annotation)
        DisplayFragment::ThinkingText(text) => acp::ContentBlock::Text(acp::TextContent {
            annotations: None,
            text: text.clone(),
            meta: None,
        }),
        DisplayFragment::Image { media_type, data } => {
            acp::ContentBlock::Image(acp::ImageContent {
                annotations: None,
                data: data.clone(),
                mime_type: media_type.clone(),
                uri: None,
                meta: None,
            })
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
        | DisplayFragment::ContextCompaction { .. } => {
            // These should not be converted to content blocks
            // Return empty text as placeholder
            acp::ContentBlock::Text(acp::TextContent {
                annotations: None,
                text: String::new(),
                meta: None,
            })
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
pub fn convert_prompt_to_content_blocks(prompt: Vec<acp::ContentBlock>) -> Vec<llm::ContentBlock> {
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
            // Other content types not yet supported
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::streaming::DisplayFragment;

    #[test]
    fn prompt_conversion_handles_text_and_images() {
        let prompt = vec![
            acp::ContentBlock::Text(acp::TextContent {
                annotations: None,
                text: "hello".into(),
                meta: None,
            }),
            acp::ContentBlock::Image(acp::ImageContent {
                annotations: None,
                data: "image-data".into(),
                mime_type: "image/png".into(),
                uri: None,
                meta: None,
            }),
        ];

        let blocks = convert_prompt_to_content_blocks(prompt);
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
}
