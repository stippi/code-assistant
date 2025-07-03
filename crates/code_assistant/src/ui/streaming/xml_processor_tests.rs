use super::test_utils::{assert_fragments_match, chunk_str, TestUI};
use super::{DisplayFragment, StreamProcessorTrait, XmlStreamProcessor};
use crate::ui::UserInterface;
use llm::StreamingChunk;
use std::sync::Arc;

// Process input text with a stream processor, breaking it into chunks
fn process_chunked_text(text: &str, chunk_size: usize) -> TestUI {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

    let mut processor = XmlStreamProcessor::new(ui_arc, 42);

    // Split text into small chunks and process each one
    for chunk in chunk_str(text, chunk_size) {
        processor.process(&StreamingChunk::Text(chunk)).unwrap();
    }

    test_ui
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_param_tag_hiding() {
        let input = "<thinking>The user has not provided a task.</thinking>\nI'll use the ask_user tool.\n<tool:ask_user>\n<param:question>What would you like to know?</param:question>\n</tool:ask_user>";

        let expected_fragments = vec![
            DisplayFragment::ThinkingText("The user has not provided a task.".to_string()),
            DisplayFragment::PlainText("I'll use the ask_user tool.".to_string()),
            DisplayFragment::ToolName {
                name: "ask_user".to_string(),
                id: "ignored".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "question".to_string(),
                value: "What would you like to know?".to_string(),
                tool_id: "ignored".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "ignored".to_string(),
            },
        ];

        // Process with very small chunks (3 chars each) to test tag handling across chunks
        let test_ui = process_chunked_text(input, 3);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_text_and_tool_in_one_line() {
        let input = "Let me read some files for you using <tool:read_files><param:path>src/main.rs</param:path></tool:read_files>";

        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me read some files for you using ".to_string()),
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "ignored".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "src/main.rs".to_string(),
                tool_id: "ignored".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "ignored".to_string(),
            },
        ];

        // Process with chunk size that splits the tool tag
        let test_ui = process_chunked_text(input, 10);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_text_and_multiple_tools() {
        let input = concat!(
            "Let me read a file:\n",
            "\n",
            "<tool:read_files>\n",
            "<param:project>test</param:project>\n",
            "<param:path>src/main.rs</param:path>\n",
            "</tool:read_files>\n",
            "\n",
            "And replace something in it:\n",
            "\n",
            "<tool:replace_in_file>\n",
            "<param:project>test</param:project>\n",
            "<param:path>src/main.rs</param:path>\n",
            "<param:diff>\n",
            ">>>>>>> SEARCH\n",
            "use tracing::warn;\n",
            "=======\n",
            "use tracing::{error, warn};\n",
            "<<<<<<< REPLACE\n",
            "</param:path>\n",
            "</tool:replace_in_file>\n",
        );

        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me read a file:".to_string()),
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "project".to_string(),
                value: "test".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "src/main.rs".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::PlainText("\nAnd replace something in it:".to_string()),
            DisplayFragment::ToolName {
                name: "replace_in_file".to_string(),
                id: "tool-42-2".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "project".to_string(),
                value: "test".to_string(),
                tool_id: "tool-42-2".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "src/main.rs".to_string(),
                tool_id: "tool-42-2".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "diff".to_string(),
                value: concat!(
                    ">>>>>>> SEARCH\n",
                    "use tracing::warn;\n",
                    "=======\n",
                    "use tracing::{error, warn};",
                    "<<<<<<< REPLACE"
                )
                .to_string(),
                tool_id: "tool-42-2".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-2".to_string(),
            },
        ];

        // Process with chunk size that splits the tool tag
        let test_ui = process_chunked_text(input, 10);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_complex_tool_call_with_multiple_params_and_linebreaks() {
        let input = "I understand.\n\nLet me search for specific files\n<tool:search_files>\n<param:regex>main function</param:regex>\n</tool:search_files>";

        let expected_fragments = vec![
            DisplayFragment::PlainText(
                "I understand.\n\nLet me search for specific files".to_string(),
            ),
            DisplayFragment::ToolName {
                name: "search_files".to_string(),
                id: "ignored".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "regex".to_string(),
                value: "main function".to_string(),
                tool_id: "ignored".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "ignored".to_string(),
            },
        ];

        // Process with chunk size that splits both tags and content
        let test_ui = process_chunked_text(input, 12);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_complex_tool_call_with_brackets() {
        let input = "I'll replace condition.\n<tool:replace_in_file>\n<param:path>src/main.ts</param:path>\n<param:diff>\n<<<<<<< SEARCH\nif a > b {\n=======\nif b <= a {\n>>>>>>> REPLACE\n</param:diff>\n</tool:replace_in_file>";

        let expected_fragments = vec![
            DisplayFragment::PlainText("I'll replace condition.".to_string()),
            DisplayFragment::ToolName {
                name: "replace_in_file".to_string(),
                id: "ignored".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "src/main.ts".to_string(),
                tool_id: "ignored".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "diff".to_string(),
                value: "<<<<<<< SEARCH\nif a > b {\n=======\nif b <= a {\n>>>>>>> REPLACE"
                    .to_string(),
                tool_id: "ignored".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "ignored".to_string(),
            },
        ];

        // Process with chunk size that splits both tags and content
        let test_ui = process_chunked_text(input, 12);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_thinking_tag_handling() {
        let input =
            "Let me think about this.\n<thinking>This is a complex problem.</thinking>\nI've decided.";

        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me think about this.".to_string()),
            DisplayFragment::ThinkingText("This is a complex problem.".to_string()),
            DisplayFragment::PlainText("I've decided.".to_string()),
        ];

        // Process with small chunks
        let test_ui = process_chunked_text(input, 5);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_simple_text_processing() {
        let input = "Hello, world!";

        // Define expected fragments
        let expected_fragments = vec![DisplayFragment::PlainText("Hello, world!".to_string())];

        // Process with small chunks
        let test_ui = process_chunked_text(input, 3);

        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }

    // Tests for the new extract_fragments_from_message method
    #[test]
    fn test_extract_fragments_from_text_message_with_xml_tags() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Create a message with text content containing XML-style tags
        let message = llm::Message {
            role: llm::MessageRole::Assistant,
            content: llm::MessageContent::Text(
                "I'll help you. <thinking>Let me plan this.</thinking> Here's what I'll do: <tool:read_files><param:path>main.rs</param:path></tool:read_files>".to_string()
            ),
            request_id: Some(1u64),
            usage: None,
        };

        let fragments = processor.extract_fragments_from_message(&message).unwrap();

        let expected_fragments = vec![
            DisplayFragment::PlainText("I'll help you.".to_string()),
            DisplayFragment::ThinkingText("Let me plan this.".to_string()),
            DisplayFragment::PlainText("Here's what I'll do:".to_string()),
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "xml_tool_id".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "main.rs".to_string(),
                tool_id: "xml_tool_id".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "xml_tool_id".to_string(),
            },
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_extract_fragments_from_structured_message_converted_to_xml_style() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Create a message with structured content including tool use
        // This tests conversion from JSON ToolUse to XML-style fragments
        let tool_input = serde_json::json!({
            "project": "code-assistant",
            "paths": ["src/main.rs", "Cargo.toml"]
        });

        let message = llm::Message {
            role: llm::MessageRole::Assistant,
            content: llm::MessageContent::Structured(vec![
                llm::ContentBlock::Text {
                    text: "I'll search the files.".to_string(),
                },
                llm::ContentBlock::ToolUse {
                    id: "search_456".to_string(),
                    name: "search_files".to_string(),
                    input: tool_input,
                },
            ]),
            request_id: Some(1u64),
            usage: None,
        };

        let fragments = processor.extract_fragments_from_message(&message).unwrap();

        let expected_fragments = vec![
            DisplayFragment::PlainText("I'll search the files.".to_string()),
            DisplayFragment::ToolName {
                name: "search_files".to_string(),
                id: "search_456".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "project".to_string(),
                value: "code-assistant".to_string(),
                tool_id: "search_456".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "paths".to_string(),
                value: "[\"src/main.rs\",\"Cargo.toml\"]".to_string(),
                tool_id: "search_456".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "search_456".to_string(),
            },
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_extract_fragments_from_mixed_structured_message() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Create a message with mixed content blocks
        let message = llm::Message {
            role: llm::MessageRole::Assistant,
            content: llm::MessageContent::Structured(vec![
                llm::ContentBlock::Thinking {
                    thinking: "I should write a file.".to_string(),
                    signature: "sig".to_string(),
                },
                llm::ContentBlock::Text {
                    text: "Let me create the file. <thinking>What content should I write?</thinking> I'll write something useful.".to_string()
                },
                llm::ContentBlock::ToolUse {
                    id: "write_789".to_string(),
                    name: "write_file".to_string(),
                    input: serde_json::json!({
                        "path": "test.txt",
                        "content": "Hello XML world!"
                    }),
                },
            ]),
            request_id: Some(1u64),
            usage: None,
        };

        let fragments = processor.extract_fragments_from_message(&message).unwrap();

        let expected_fragments = vec![
            DisplayFragment::ThinkingText("I should write a file.".to_string()),
            DisplayFragment::PlainText("Let me create the file.".to_string()),
            DisplayFragment::ThinkingText("What content should I write?".to_string()),
            DisplayFragment::PlainText("I'll write something useful.".to_string()),
            DisplayFragment::ToolName {
                name: "write_file".to_string(),
                id: "write_789".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "test.txt".to_string(),
                tool_id: "write_789".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "content".to_string(),
                value: "Hello XML world!".to_string(),
                tool_id: "write_789".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "write_789".to_string(),
            },
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_mismatched_tool_closing_tag() {
        // Test that mismatched tool closing tags don't cause empty tool ID errors
        let input = "<tool:read_files>\n<param:path>test.txt</param:path>\n</tool:different_name>";

        let test_ui = process_chunked_text(input, 3);
        let fragments = test_ui.get_fragments();

        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "test.txt".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(), // Should still end with the original tool ID
            },
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_orphaned_tool_closing_tag() {
        // Test that tool closing tags without corresponding opening tags are ignored gracefully
        let input = "Some text here </tool:nonexistent> more text after";

        let test_ui = process_chunked_text(input, 5);
        let fragments = test_ui.get_fragments();

        // The text gets combined into a single fragment since the orphaned closing tag is just ignored
        let expected_fragments = vec![DisplayFragment::PlainText(
            "Some text here more text after".to_string(),
        )];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_user_messages_not_parsed_for_xml_tags() {
        // Test that user messages with XML-like content are treated as plain text
        use llm::{Message, MessageContent, MessageRole};

        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Create a user message with XML-like content
        let user_message = Message {
            role: MessageRole::User,
            content: MessageContent::Text(
                "Please use <tool:read_files> to read <param:path>test.txt</param:path> and show me <thinking>what should I do</thinking>".to_string()
            ),
            request_id: None,
            usage: None,
        };

        let fragments = processor
            .extract_fragments_from_message(&user_message)
            .unwrap();

        // Should be treated as a single PlainText fragment, not parsed for XML tags
        let expected_fragments = vec![
            DisplayFragment::PlainText(
                "Please use <tool:read_files> to read <param:path>test.txt</param:path> and show me <thinking>what should I do</thinking>".to_string()
            ),
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_param_tags_outside_tool_context_treated_as_plain_text() {
        // Test that parameter tags outside tool context are rendered as plain text
        let input = "Some text <param:invalid>parameter content</param:invalid> more text";

        let test_ui = process_chunked_text(input, 5);
        let fragments = test_ui.get_fragments();

        // Should be treated as plain text since there's no surrounding tool
        let expected_fragments = vec![DisplayFragment::PlainText(
            "Some text <param:invalid>parameter content</param:invalid> more text".to_string(),
        )];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_param_tags_before_tool_context_treated_as_plain_text() {
        // Test parameter tags that appear before any tool is opened
        let input = "<param:orphaned>content</param:orphaned>\n<tool:read_files>\n<param:path>test.txt</param:path>\n</tool:read_files>";

        let test_ui = process_chunked_text(input, 8);
        let fragments = test_ui.get_fragments();

        let expected_fragments = vec![
            // Orphaned parameter tags should be plain text
            DisplayFragment::PlainText("<param:orphaned>content</param:orphaned>".to_string()),
            // Valid tool and parameter
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "test.txt".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(),
            },
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_param_tags_after_tool_context_treated_as_plain_text() {
        // Test parameter tags that appear after a tool is closed
        let input = "<tool:read_files>\n<param:path>test.txt</param:path>\n</tool:read_files>\n<param:orphaned>content</param:orphaned>";

        let test_ui = process_chunked_text(input, 10);
        let fragments = test_ui.get_fragments();

        let expected_fragments = vec![
            // Valid tool and parameter
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "test.txt".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(),
            },
            // Orphaned parameter tags should be plain text
            DisplayFragment::PlainText("<param:orphaned>content</param:orphaned>".to_string()),
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_unclosed_param_tag_with_tool_closing() {
        // Test the specific scenario where a parameter tag is not closed before tool ends
        let input = "<tool:edit_file>\n<param:diff>some content without closing tag\n</tool:edit_file>";

        let test_ui = process_chunked_text(input, 12);
        let fragments = test_ui.get_fragments();

        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "edit_file".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "diff".to_string(),
                value: "some content without closing tag".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(),
            },
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_param_end_tag_without_param_start() {
        // Test parameter end tags that appear without corresponding start tags
        let input = "Some text </param:invalid> more text";

        let test_ui = process_chunked_text(input, 6);
        let fragments = test_ui.get_fragments();

        // Should be treated as plain text since there's no corresponding param start
        let expected_fragments = vec![DisplayFragment::PlainText(
            "Some text </param:invalid> more text".to_string(),
        )];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_multiple_tools_with_malformed_params() {
        // Test multiple tools where some have malformed parameter tags
        let input = concat!(
            "<param:orphaned1>before tools</param:orphaned1>\n",
            "<tool:first_tool>\n",
            "<param:valid>content</param:valid>\n", 
            "</tool:first_tool>\n",
            "<param:orphaned2>between tools</param:orphaned2>\n",
            "<tool:second_tool>\n",
            "<param:also_valid>more content\n",  // Missing closing tag
            "</tool:second_tool>\n",
            "<param:orphaned3>after tools</param:orphaned3>"
        );

        let test_ui = process_chunked_text(input, 15);
        let fragments = test_ui.get_fragments();

        let expected_fragments = vec![
            // Orphaned parameter before tools
            DisplayFragment::PlainText("<param:orphaned1>before tools</param:orphaned1>".to_string()),
            // First valid tool
            DisplayFragment::ToolName {
                name: "first_tool".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "valid".to_string(),
                value: "content".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(),
            },
            // Orphaned parameter between tools
            DisplayFragment::PlainText("<param:orphaned2>between tools</param:orphaned2>".to_string()),
            // Second tool with unclosed parameter
            DisplayFragment::ToolName {
                name: "second_tool".to_string(),
                id: "tool-42-2".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "also_valid".to_string(),
                value: "more content".to_string(),
                tool_id: "tool-42-2".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-2".to_string(),
            },
            // Orphaned parameter after tools
            DisplayFragment::PlainText("<param:orphaned3>after tools</param:orphaned3>".to_string()),
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_tool_limit_detection() {
        // Test that second tool start triggers tool limit error
        let input = concat!(
            "<tool:read_files>\\n",
            "<param:path>test.txt</param:path>\\n",
            "</tool:read_files>\\n",
            "<tool:write_file>\\n",  // This should trigger the tool limit
            "<param:path>output.txt</param:path>\\n",
            "</tool:write_file>"
        );

        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Process the input and expect an error
        let result = processor.process(&StreamingChunk::Text(input.to_string()));
        
        // Should get a tool limit error
        assert!(result.is_err(), "Expected tool limit error");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Tool limit reached"),
            "Error should mention tool limit: {}",
            error_msg
        );

        // Should have processed the first tool successfully
        let fragments = test_ui.get_fragments();
        assert!(fragments.len() >= 3, "Should have at least tool name, parameter, and tool end for first tool");
        
        // First fragment should be the first tool
        assert!(matches!(
            fragments[0],
            DisplayFragment::ToolName { ref name, .. } if name == "read_files"
        ));
    }
}
