use super::streaming::{DisplayFragment, StreamProcessor};
use crate::llm::StreamingChunk;
use crate::ui::{ToolStatus, UIError, UserInterface};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// A test UI that collects display fragments and merges them appropriately
#[derive(Clone)]
pub struct TestUI {
    fragments: Arc<Mutex<VecDeque<DisplayFragment>>>,
}

impl TestUI {
    pub fn new() -> Self {
        Self {
            fragments: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn get_fragments(&self) -> Vec<DisplayFragment> {
        let guard = self.fragments.lock().unwrap();
        guard.iter().cloned().collect()
    }

    // Attempt to merge a new fragment with the last one if they are of the same type
    fn merge_fragments(last: &mut DisplayFragment, new: &DisplayFragment) -> bool {
        match (last, new) {
            // Merge plain text fragments
            (DisplayFragment::PlainText(last_text), DisplayFragment::PlainText(new_text)) => {
                last_text.push_str(new_text);
                true
            }

            // Merge thinking text fragments
            (DisplayFragment::ThinkingText(last_text), DisplayFragment::ThinkingText(new_text)) => {
                last_text.push_str(new_text);
                true
            }

            // Merge tool parameters with the same name and tool_id
            (
                DisplayFragment::ToolParameter {
                    name: last_name,
                    value: last_value,
                    tool_id: last_id,
                },
                DisplayFragment::ToolParameter {
                    name: new_name,
                    value: new_value,
                    tool_id: new_id,
                },
            ) => {
                if last_name == new_name && last_id == new_id {
                    last_value.push_str(new_value);
                    true
                } else {
                    false
                }
            }

            // No other fragments can be merged
            _ => false,
        }
    }
}

#[async_trait]
impl UserInterface for TestUI {
    async fn display(&self, _message: crate::ui::UIMessage) -> Result<(), UIError> {
        Ok(())
    }

    async fn get_input(&self, _prompt: &str) -> Result<String, UIError> {
        Ok(String::new())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        let mut guard = self.fragments.lock().unwrap();

        // Check if we can merge this fragment with the previous one
        if let Some(last_fragment) = guard.back_mut() {
            if Self::merge_fragments(last_fragment, fragment) {
                // Successfully merged, don't add a new fragment
                return Ok(());
            }
        }

        // If we couldn't merge, add the new fragment
        guard.push_back(fragment.clone());
        Ok(())
    }

    async fn update_memory(&self, _memory: &crate::types::WorkingMemory) -> Result<(), UIError> {
        // Test implementation does nothing with memory updates
        Ok(())
    }

    async fn update_tool_status(
        &self,
        _tool_id: &str,
        _status: ToolStatus,
        _message: Option<String>,
    ) -> Result<(), UIError> {
        // Test implementation does nothing with tool status
        Ok(())
    }

    async fn begin_llm_request(&self) -> Result<u64, UIError> {
        // For tests, return a fixed request ID
        Ok(42)
    }

    async fn end_llm_request(&self, _request_id: u64) -> Result<(), UIError> {
        // Mock implementation does nothing with request completion
        Ok(())
    }
}

// Helper function to split text into small chunks for testing tag handling
fn chunk_str(s: &str, chunk_size: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut chunks = Vec::new();

    for chunk in chars.chunks(chunk_size) {
        chunks.push(chunk.iter().collect::<String>());
    }

    chunks
}

// Process input text with a stream processor, breaking it into chunks
fn process_chunked_text(text: &str, chunk_size: usize) -> TestUI {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

    let mut processor = StreamProcessor::new(ui_arc);

    // Split text into small chunks and process each one
    for chunk in chunk_str(text, chunk_size) {
        processor.process(&StreamingChunk::Text(chunk)).unwrap();
    }

    test_ui
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function to print fragments for debugging
    fn print_fragments(fragments: &[DisplayFragment]) {
        println!("Collected {} fragments:", fragments.len());
        for (i, fragment) in fragments.iter().enumerate() {
            match fragment {
                DisplayFragment::PlainText(text) => println!("  [{i}] PlainText: {text}"),
                DisplayFragment::ThinkingText(text) => println!("  [{i}] ThinkingText: {text}"),
                DisplayFragment::ToolName { name, id } => {
                    println!("  [{i}] ToolName: {name} (id: {id})")
                }
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => println!("  [{i}] ToolParam: {name}={value} (tool_id: {tool_id})"),
                DisplayFragment::ToolEnd { id } => println!("  [{i}] ToolEnd: (id: {id})"),
            }
        }
    }

    // Helper function to check if two fragments match in content (ignoring IDs)
    fn fragments_match(expected: &DisplayFragment, actual: &DisplayFragment) -> bool {
        match (expected, actual) {
            (
                DisplayFragment::PlainText(expected_text),
                DisplayFragment::PlainText(actual_text),
            ) => expected_text == actual_text,
            (
                DisplayFragment::ThinkingText(expected_text),
                DisplayFragment::ThinkingText(actual_text),
            ) => expected_text == actual_text,
            (
                DisplayFragment::ToolName {
                    name: expected_name,
                    ..
                },
                DisplayFragment::ToolName {
                    name: actual_name, ..
                },
            ) => expected_name == actual_name,
            (
                DisplayFragment::ToolParameter {
                    name: expected_name,
                    value: expected_value,
                    ..
                },
                DisplayFragment::ToolParameter {
                    name: actual_name,
                    value: actual_value,
                    ..
                },
            ) => expected_name == actual_name && expected_value == actual_value,
            (DisplayFragment::ToolEnd { .. }, DisplayFragment::ToolEnd { .. }) => true,
            _ => false,
        }
    }

    // Helper function to assert that actual fragments match expected fragments
    fn assert_fragments_match(expected: &[DisplayFragment], actual: &[DisplayFragment]) {
        assert_eq!(
            expected.len(),
            actual.len(),
            "Different number of fragments. Expected {}, got {}",
            expected.len(),
            actual.len()
        );

        for (i, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
            assert!(
                fragments_match(expected, actual),
                "Fragment mismatch at position {}: \nExpected: {:?}\nActual: {:?}",
                i,
                expected,
                actual
            );
        }
    }

    #[test]
    fn test_param_tag_hiding() {
        let input = "<thinking>The user has not provided a task.</thinking>\nI'll use the ask_user tool.\n<tool:ask_user>\n<param:question>What would you like to know?</param:question>\n</tool:ask_user>";

        // Define expected fragments
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

        // Get fragments and print for debugging
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check that fragments match expected sequence
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_text_and_tool_in_one_line() -> Result<()> {
        let input = "Let me read some files for you using <tool:read_files><param:path>src/main.rs</param:path></tool:read_files>";

        // Define expected fragments
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

        // Get fragments and print for debugging
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check that fragments match expected sequence
        assert_fragments_match(&expected_fragments, &fragments);

        Ok(())
    }

    #[test]
    fn test_complex_tool_call_with_multiple_params_and_linebreaks() -> Result<()> {
        let input = "I understand.\n\nLet me search for specific files\n<tool:search_files>\n<param:regex>main function</param:regex>\n</tool:search_files>";

        // Define expected fragments
        let expected_fragments = vec![
            DisplayFragment::PlainText(
                "I understand.\n\nLet me search for specific files".to_string(),
            ),
            DisplayFragment::ToolName {
                name: "search_files".to_string(),
                id: "ignored".to_string(),
            },
            // One parameter - regex
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

        // Get fragments and print for debugging
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check that fragments match expected sequence
        assert_fragments_match(&expected_fragments, &fragments);

        Ok(())
    }

    #[test]
    fn test_complex_tool_call_with_brackets() -> Result<()> {
        let input = "I'll replace condition.\n<tool:replace_in_file>\n<param:path>src/main.ts</param:path>\n<param:diff><<<<<<< SEARCH\nif a > b {\n=======\nif b <= a {\n>>>>>>> REPLACE</param:diff>\n</tool:replace_in_file>";

        // Define expected fragments - order of parameters might vary
        let expected_fragments = vec![
            DisplayFragment::PlainText("I'll replace condition.".to_string()),
            DisplayFragment::ToolName {
                name: "replace_in_file".to_string(),
                id: "ignored".to_string(),
            },
            // Parameters in expected order
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

        // Get fragments and print for debugging
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check that fragments match expected sequence
        assert_fragments_match(&expected_fragments, &fragments);

        Ok(())
    }

    #[test]
    fn test_thinking_tag_handling() -> Result<()> {
        let input =
            "Let me think about this.\n<thinking>This is a complex problem.</thinking>\nI've decided.";

        // Define expected fragments
        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me think about this.".to_string()),
            DisplayFragment::ThinkingText("This is a complex problem.".to_string()),
            DisplayFragment::PlainText("I've decided.".to_string()),
        ];

        // Process with small chunks
        let test_ui = process_chunked_text(input, 5);

        // Get fragments and print for debugging
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check that fragments match expected sequence
        assert_fragments_match(&expected_fragments, &fragments);

        Ok(())
    }

    #[test]
    fn test_simple_text_processing() -> Result<()> {
        let input = "Hello, world!";

        // Define expected fragments
        let expected_fragments = vec![DisplayFragment::PlainText("Hello, world!".to_string())];

        // Process with small chunks
        let test_ui = process_chunked_text(input, 3);

        // Get fragments
        let fragments = test_ui.get_fragments();

        // Check that fragments match expected sequence
        assert_fragments_match(&expected_fragments, &fragments);

        Ok(())
    }
}
