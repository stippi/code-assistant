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
        match processor.process(&StreamingChunk::Text(chunk)) {
            Ok(()) => continue,
            Err(e) if e.to_string().contains("Tool limit reached") => {
                // Tool limit reached, stop processing - this is expected behavior
                break;
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    test_ui
}

#[cfg(test)]
mod tests {
    use crate::ui::streaming::test_utils::print_fragments;

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
        // Test that tool limit error occurs after first tool completes, preventing second tool processing
        let input = concat!(
            "Let me read a file:\n",
            "\n",
            "<tool:read_files>\n",
            "<param:project>test</param:project>\n",
            "<param:path>src/main.rs</param:path>\n",
            "</tool:read_files>\n", // Tool limit error should occur here
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
            "Error should mention tool limit: {error_msg}"
        );

        // Should have processed the first tool completely, but not the second tool
        let fragments = test_ui.get_fragments();

        // Verify first tool is present and complete
        let first_tool_fragments: Vec<_> = fragments
            .iter()
            .filter(|f| match f {
                DisplayFragment::ToolName { name, .. } if name == "read_files" => true,
                DisplayFragment::ToolParameter { tool_id, .. } if tool_id == "tool-42-1" => true,
                DisplayFragment::ToolEnd { id } if id == "tool-42-1" => true,
                _ => false,
            })
            .collect();

        assert!(
            first_tool_fragments.len() >= 3,
            "Should have at least tool name, parameter(s), and tool end for first tool"
        );

        // Verify second tool is NOT present (no fragments with tool-42-2 ID)
        let second_tool_fragments: Vec<_> = fragments
            .iter()
            .filter(|f| match f {
                DisplayFragment::ToolName { name, .. } if name == "replace_in_file" => true,
                DisplayFragment::ToolParameter { tool_id, .. } if tool_id == "tool-42-2" => true,
                DisplayFragment::ToolEnd { id } if id == "tool-42-2" => true,
                _ => false,
            })
            .collect();

        assert_eq!(
            second_tool_fragments.len(),
            0,
            "Second tool should not be processed due to tool limit"
        );
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
        // Test that processing stops after tool completes, preventing orphaned param processing
        let input = "<tool:read_files>\n<param:path>test.txt</param:path>\n</tool:read_files>\n<param:orphaned>content</param:orphaned>";

        let test_ui = process_chunked_text(input, 10);
        let fragments = test_ui.get_fragments();

        let expected_fragments = vec![
            // Valid tool and parameter - processing stops after ToolEnd
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
            // Note: orphaned parameter after tool is NOT processed due to tool limit
        ];

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_unclosed_param_tag_with_tool_closing() {
        // Test the specific scenario where a parameter tag is not closed before tool ends
        let input =
            "<tool:edit_file>\n<param:diff>some content without closing tag\n</tool:edit_file>";

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
        // Test that tool limit error occurs after first tool, preventing second tool processing
        let input = concat!(
            "<param:orphaned1>before tools</param:orphaned1>\n",
            "<tool:first_tool>\n",
            "<param:valid>content</param:valid>\n",
            "</tool:first_tool>\n", // Tool limit error should occur here
            "<param:orphaned2>between tools</param:orphaned2>\n",
            "<tool:second_tool>\n",
            "<param:also_valid>more content\n", // Missing closing tag
            "</tool:second_tool>\n",
            "<param:orphaned3>after tools</param:orphaned3>"
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
            "Error should mention tool limit: {error_msg}"
        );

        // Should have processed orphaned param, first tool completely, but not the second tool
        let fragments = test_ui.get_fragments();

        // Verify orphaned parameter before first tool is present
        let orphaned_before: Vec<_> = fragments
            .iter()
            .filter(|f| matches!(f, DisplayFragment::PlainText(text) if text.contains("orphaned1")))
            .collect();
        assert!(
            !orphaned_before.is_empty(),
            "Should have orphaned parameter before first tool"
        );

        // Verify first tool is present and complete
        let first_tool_fragments: Vec<_> = fragments
            .iter()
            .filter(|f| match f {
                DisplayFragment::ToolName { name, .. } if name == "first_tool" => true,
                DisplayFragment::ToolParameter { tool_id, .. } if tool_id == "tool-42-1" => true,
                DisplayFragment::ToolEnd { id } if id == "tool-42-1" => true,
                _ => false,
            })
            .collect();

        assert!(
            first_tool_fragments.len() >= 3,
            "Should have at least tool name, parameter, and tool end for first tool"
        );

        // Verify second tool is NOT present (no fragments with tool-42-2 ID or second_tool name)
        let second_tool_fragments: Vec<_> = fragments
            .iter()
            .filter(|f| match f {
                DisplayFragment::ToolName { name, .. } if name == "second_tool" => true,
                DisplayFragment::ToolParameter { tool_id, .. } if tool_id == "tool-42-2" => true,
                DisplayFragment::ToolEnd { id } if id == "tool-42-2" => true,
                _ => false,
            })
            .collect();

        assert_eq!(
            second_tool_fragments.len(),
            0,
            "Second tool should not be processed due to tool limit"
        );

        // Verify orphaned parameter between tools is NOT present (processing stopped after first tool)
        let orphaned_between: Vec<_> = fragments
            .iter()
            .filter(|f| matches!(f, DisplayFragment::PlainText(text) if text.contains("orphaned2")))
            .collect();
        assert_eq!(
            orphaned_between.len(),
            0,
            "Should not process content after first tool due to tool limit"
        );

        // Verify orphaned parameter after all tools is NOT present (processing stopped at second tool start)
        let orphaned_after: Vec<_> = fragments
            .iter()
            .filter(|f| matches!(f, DisplayFragment::PlainText(text) if text.contains("orphaned3")))
            .collect();
        assert_eq!(
            orphaned_after.len(),
            0,
            "Should not process content after tool limit is triggered"
        );
    }

    #[test]
    fn test_tool_limit_detection() {
        // Test that tool limit error occurs after first tool ends (not at second tool start)
        let input = concat!(
            "Some text before first tool\n",
            "<tool:read_files>\n",
            "<param:path>test.txt</param:path>\n",
            "</tool:read_files>\n", // Tool limit error should occur here
            "Some text after first tool\n",
            "<tool:write_file>\n",
            "<param:path>output.txt</param:path>\n",
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
            "Error should mention tool limit: {error_msg}"
        );

        let expected_fragments = vec![
            DisplayFragment::PlainText("Some text before first tool".to_string()),
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

        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_tool_limit_detection_with_realistic_chunks() {
        let chunks = vec![
            "I'll help",
            " you ref",
            "actor the A",
            "nthropic client:",
            "\n\n<tool",
            ":",
            "rea",
            "d_",
            "files",
            ">\n<param",
            ":",
            "project",
            ">",
            "code-assistant",
            "</param:project>",
            "\n<param:",
            "path",
            ">",
            "crates/ll",
            "m/src/",
            "anthropic.rs",
            "</param:path",
            ">\n</tool",
            ":read_files",
            ">\n\n---",
            ">\n\n---",
        ];

        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Split text into small chunks and process each one
        for chunk in chunks {
            match processor.process(&StreamingChunk::Text(chunk.to_string())) {
                Ok(()) => continue,
                Err(e) if e.to_string().contains("Tool limit reached") => {
                    // Tool limit reached, stop processing - this is expected behavior
                    break;
                }
                Err(e) => panic!("Unexpected error: {e}"),
            }
        }

        let expected_fragments = vec![
            DisplayFragment::PlainText("I'll help you refactor the Anthropic client:".to_string()),
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "project".to_string(),
                value: "code-assistant".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "crates/llm/src/anthropic.rs".to_string(),
                tool_id: "tool-42-1".to_string(),
            },
            DisplayFragment::ToolEnd {
                id: "tool-42-1".to_string(),
            },
        ];

        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_smart_filter_allows_content_after_read_tools() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Process a complete read tool block followed by text
        // Read tools should allow content after them according to SmartToolFilter
        let input = "<tool:read_files><param:project>test</param:project></tool:read_files>";
        let result = processor.process(&StreamingChunk::Text(input.to_string()));
        assert!(result.is_ok(), "Initial tool processing should succeed");

        // Now try to process text after the complete read tool block
        let additional_text = "This should be allowed after read tools";
        let result = processor.process(&StreamingChunk::Text(additional_text.to_string()));

        // Should succeed - content is allowed after read tools
        assert!(
            result.is_ok(),
            "Should allow content after read tools with SmartToolFilter"
        );

        // Send StreamingComplete to flush any buffered content
        let result = processor.process(&StreamingChunk::StreamingComplete);
        assert!(result.is_ok(), "StreamingComplete should succeed");

        let fragments = test_ui.get_fragments();

        // Should have 4 fragments: ToolName, ToolParameter, ToolEnd, PlainText
        assert_eq!(fragments.len(), 4);
        assert!(matches!(fragments[0], DisplayFragment::ToolName { .. }));
        assert!(matches!(
            fragments[1],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(fragments[2], DisplayFragment::ToolEnd { .. }));

        // The additional text should be buffered and emitted as PlainText
        if let DisplayFragment::PlainText(text) = &fragments[3] {
            assert!(text.contains("This should be allowed after read tools"));
        } else {
            panic!("Expected PlainText fragment for additional content");
        }
    }

    #[test]
    fn test_smart_filter_allows_chaining_read_tools() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Process first read tool
        let input1 = "<tool:read_files><param:project>test</param:project></tool:read_files>";
        let result = processor.process(&StreamingChunk::Text(input1.to_string()));
        assert!(result.is_ok(), "First read tool should succeed");

        // Process text between tools
        let between_text = "Now let me list the files:";
        let result = processor.process(&StreamingChunk::Text(between_text.to_string()));
        assert!(result.is_ok(), "Text between read tools should be allowed");

        // Process second read tool
        let input2 = "<tool:list_files><param:project>test</param:project></tool:list_files>";
        let result = processor.process(&StreamingChunk::Text(input2.to_string()));
        assert!(result.is_ok(), "Second read tool should be allowed");

        // Send StreamingComplete to flush any buffered content
        let result = processor.process(&StreamingChunk::StreamingComplete);
        assert!(result.is_ok(), "StreamingComplete should succeed");

        let fragments = test_ui.get_fragments();

        // Should have fragments for both tools plus the text between
        // First tool: ToolName, ToolParameter, ToolEnd
        // Text: PlainText
        // Second tool: ToolName, ToolParameter, ToolEnd
        assert_eq!(fragments.len(), 7);

        // Verify the sequence
        assert!(matches!(fragments[0], DisplayFragment::ToolName { .. }));
        assert!(matches!(
            fragments[1],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(fragments[2], DisplayFragment::ToolEnd { .. }));
        assert!(matches!(fragments[3], DisplayFragment::PlainText(_)));
        assert!(matches!(fragments[4], DisplayFragment::ToolName { .. }));
        assert!(matches!(
            fragments[5],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(fragments[6], DisplayFragment::ToolEnd { .. }));
    }

    #[test]
    fn test_smart_filter_blocks_write_tool_after_read_tool() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Process first read tool
        let input1 = "<tool:read_files><param:project>test</param:project></tool:read_files>";
        let result = processor.process(&StreamingChunk::Text(input1.to_string()));
        assert!(result.is_ok(), "First read tool should succeed");

        // Process text between tools
        let between_text = "Now let me write to a file:";
        let result = processor.process(&StreamingChunk::Text(between_text.to_string()));
        assert!(result.is_ok(), "Text between tools should be buffered");

        // Try to process a write tool - this should be blocked
        let write_tool = "<tool:write_file><param:project>test</param:project><param:path>output.txt</param:path><param:content>test</param:content></tool:write_file>";
        let result = processor.process(&StreamingChunk::Text(write_tool.to_string()));

        // Should get the blocking error
        assert!(
            result.is_err(),
            "Write tool after read tool should be blocked by SmartToolFilter"
        );

        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Tool limit reached"),
            "Error should mention tool limit"
        );

        let fragments = test_ui.get_fragments();

        // Should only have fragments for the first read tool
        // The buffered text and write tool should have been discarded
        assert_eq!(fragments.len(), 3);
        assert!(matches!(fragments[0], DisplayFragment::ToolName { .. }));
        assert!(matches!(
            fragments[1],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(fragments[2], DisplayFragment::ToolEnd { .. }));
    }

    #[test]
    fn test_smart_filter_blocks_write_tool_immediately() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
        let mut processor = XmlStreamProcessor::new(ui_arc, 42);

        // Process a complete write tool block followed by text
        // Write tools should block further content according to SmartToolFilter
        let input = "<tool:write_file><param:project>test</param:project><param:path>test.txt</param:path><param:content>hello</param:content></tool:write_file>";
        let result = processor.process(&StreamingChunk::Text(input.to_string()));
        assert!(result.is_ok(), "Initial tool processing should succeed");

        // Now try to process non-whitespace text after the complete tool block
        let additional_text = "This should be blocked";
        let result = processor.process(&StreamingChunk::Text(additional_text.to_string()));

        // Should get the blocking error
        assert!(
            result.is_err(),
            "Should get error when trying to emit non-whitespace after complete tool"
        );

        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Tool limit reached"),
            "Error should mention tool limit"
        );

        let fragments = test_ui.get_fragments();

        // Should have exactly 4 fragments: ToolName, 3 ToolParameters, ToolEnd
        // The additional text should NOT have been processed
        assert_eq!(fragments.len(), 5);
        assert!(matches!(fragments[0], DisplayFragment::ToolName { .. }));
        assert!(matches!(
            fragments[1],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(
            fragments[2],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(
            fragments[3],
            DisplayFragment::ToolParameter { .. }
        ));
        assert!(matches!(fragments[4], DisplayFragment::ToolEnd { .. }));
    }
}
