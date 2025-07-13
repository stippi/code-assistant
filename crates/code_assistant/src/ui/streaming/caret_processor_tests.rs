
//! Tests for the caret stream processor

use super::test_utils::{assert_fragments_match, TestUI};
use crate::ui::streaming::{CaretStreamProcessor, DisplayFragment, StreamProcessorTrait};
use crate::ui::UserInterface;
use llm::{Message, MessageContent, MessageRole, StreamingChunk};
use std::sync::Arc;

#[tokio::test]
async fn test_caret_simple_tool() {
    let test_ui = TestUI::new();
    let ui = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui, 123);

    // Simulate streaming chunks of a simple caret tool
    processor
        .process(&StreamingChunk::Text("^^^list_projects\n^^^\n".to_string()))
        .unwrap();

    let fragments = test_ui.get_fragments();
    assert!(fragments.len() >= 2);

    // Should have tool name and tool end
    assert!(matches!(
        fragments[0],
        DisplayFragment::ToolName { ref name, .. } if name == "list_projects"
    ));

    // Should end with ToolEnd
    assert!(matches!(fragments.last(), Some(DisplayFragment::ToolEnd { .. })));
}

#[tokio::test]
async fn test_caret_multiline_tool() {
    let test_ui = TestUI::new();
    let ui = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui, 123);

    // Simulate streaming chunks with multiline content
    let content = "^^^read_files\nproject: test\ncontent ---\nThis is multiline content\nwith several lines\n--- content\n^^^";
    processor
        .process(&StreamingChunk::Text(content.to_string()))
        .unwrap();

    let fragments = test_ui.get_fragments();
    assert!(fragments.len() >= 3);

    // Should have tool name
    assert!(matches!(
        fragments[0],
        DisplayFragment::ToolName { ref name, .. } if name == "read_files"
    ));

    // Should have content parameter with multiline value
    let content_param = fragments.iter().find(|f| {
        matches!(f, DisplayFragment::ToolParameter { name, value, .. }
            if name == "content" && value.contains("multiline content"))
    });
    assert!(content_param.is_some());

    // Should end with ToolEnd
    assert!(matches!(fragments.last(), Some(DisplayFragment::ToolEnd { .. })));
}

#[tokio::test]
async fn test_extract_fragments_from_complete_message() {
    let test_ui = TestUI::new();
    let ui = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui, 123);

    let message = Message {
        role: MessageRole::Assistant,
        content: MessageContent::Text("I'll create the file for you.\n\n^^^list_projects\n^^^".to_string()),
        request_id: None,
        usage: None,
    };

    let fragments = processor.extract_fragments_from_message(&message).unwrap();

    // Should have plain text, tool name, and tool end
    assert!(fragments.len() >= 3);

    // Check for plain text
    assert!(matches!(
        fragments[0],
        DisplayFragment::PlainText(ref text) if text.contains("I'll create")
    ));

    // Check for tool name
    let tool_name_fragment = fragments.iter().find(|f| {
        matches!(f, DisplayFragment::ToolName { name, .. } if name == "list_projects")
    });
    assert!(tool_name_fragment.is_some());

    // Check for tool end
    assert!(fragments.iter().any(|f| matches!(f, DisplayFragment::ToolEnd { .. })));
}
