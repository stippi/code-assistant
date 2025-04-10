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
}
