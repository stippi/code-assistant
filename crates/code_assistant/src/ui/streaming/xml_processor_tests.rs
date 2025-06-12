use super::test_utils::{assert_fragments_match, chunk_str, TestUI};
use super::{DisplayFragment, StreamProcessorTrait, XmlStreamProcessor};
use crate::ui::UserInterface;
use llm::StreamingChunk;
use std::sync::Arc;

// Process input text with a stream processor, breaking it into chunks
fn process_chunked_text(text: &str, chunk_size: usize) -> TestUI {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

    let mut processor = XmlStreamProcessor::new(ui_arc);

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
        let mut processor = XmlStreamProcessor::new(ui_arc);

        // Create a message with text content containing XML-style tags
        let message = llm::Message {
            role: llm::MessageRole::Assistant,
            content: llm::MessageContent::Text(
                "I'll help you. <thinking>Let me plan this.</thinking> Here's what I'll do: <tool:read_files><param:path>main.rs</param:path></tool:read_files>".to_string()
            ),
            request_id: Some(1u64),
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
        let mut processor = XmlStreamProcessor::new(ui_arc);

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
        let mut processor = XmlStreamProcessor::new(ui_arc);

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
}
