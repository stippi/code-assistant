use super::streaming::{DisplayFragment, StreamProcessor};
use crate::llm::StreamingChunk;
use crate::ui::{UIError, UserInterface};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// A test UI that collects display fragments
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
        guard.push_back(fragment.clone());
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

    // Test that parameter end tags are correctly processed and not shown
    #[test]
    fn test_param_tag_hiding() {
        let input = "<thinking>The user has not provided a task.</thinking>\nI'll use the ask_user tool.\n<tool:ask_user>\n<param:question>What would you like to know?</param:question>\n</tool:ask_user>";

        // Process with very small chunks (3 chars each) to test tag handling across chunks
        let test_ui = process_chunked_text(input, 3);

        // Get and verify the fragments
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check if we find parameter content in tool parameter fragments
        let mut found_param_content = false;
        let mut found_param_end_tag = false;

        for fragment in &fragments {
            match fragment {
                DisplayFragment::ToolParameter { value, .. } => {
                    if value.contains("What would you like to know?") {
                        found_param_content = true;
                    }
                }
                DisplayFragment::PlainText(text) => {
                    if text.contains("</param:question>") {
                        found_param_end_tag = true;
                    }
                }
                _ => {}
            }
        }

        assert!(found_param_content, "Parameter content should be visible");
        assert!(
            !found_param_end_tag,
            "Parameter end tag should not be visible"
        );
    }

    #[test]
    fn test_tool_tag_across_chunks() -> Result<()> {
        let input = "Let me read some files for you using <tool:read_files><param:paths>[\"src/main.rs\"]</param:paths></tool:read_files>";

        // Process with chunk size that splits the tool tag
        let test_ui = process_chunked_text(input, 10);

        // Get and verify the fragments
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check for specific fragment types
        let mut found_tool_name = false;
        let mut found_tool_param = false;
        let mut found_tool_end = false;

        for fragment in &fragments {
            match fragment {
                DisplayFragment::ToolName { name, .. } => {
                    found_tool_name = true;
                    assert_eq!(name, "read_files", "Should extract correct tool name");
                }
                DisplayFragment::ToolParameter { name, value, .. } => {
                    found_tool_param = true;
                    assert_eq!(name, "paths", "Should extract correct param name");
                    assert!(
                        value.contains("[\"src/main.rs\"]"),
                        "Should contain correct param value"
                    );
                }
                DisplayFragment::ToolEnd { .. } => {
                    found_tool_end = true;
                }
                _ => {}
            }
        }

        assert!(found_tool_name, "Should find tool name fragment");
        assert!(found_tool_param, "Should find tool parameter fragment");
        assert!(found_tool_end, "Should find tool end fragment");

        Ok(())
    }

    #[test]
    fn test_complex_tool_call_with_multiple_params() -> Result<()> {
        let input = "Let me search for specific files <tool:search_files><param:query>main function</param:query><param:path>src</param:path><param:case_sensitive>false</param:case_sensitive></tool:search_files>";

        // Process with chunk size that splits both tags and content
        let test_ui = process_chunked_text(input, 12);

        // Get and verify the fragments
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check for specific fragment patterns
        let mut found_tool_name = false;
        let mut param_names = Vec::new();

        for fragment in &fragments {
            match fragment {
                DisplayFragment::ToolName { name, .. } => {
                    found_tool_name = true;
                    assert_eq!(name, "search_files", "Should extract correct tool name");
                }
                DisplayFragment::ToolParameter { name, .. } => {
                    param_names.push(name.clone());
                }
                _ => {}
            }
        }

        assert!(found_tool_name, "Should find tool name fragment");
        assert_eq!(param_names.len(), 3, "Should find 3 parameter fragments");
        assert!(
            param_names.contains(&"query".to_string()),
            "Should have 'query' parameter"
        );
        assert!(
            param_names.contains(&"path".to_string()),
            "Should have 'path' parameter"
        );
        assert!(
            param_names.contains(&"case_sensitive".to_string()),
            "Should have 'case_sensitive' parameter"
        );

        Ok(())
    }

    #[test]
    fn test_thinking_tag_handling() -> Result<()> {
        let input = "Let me think about this.<thinking>This is a complex problem that requires careful analysis.</thinking>I've considered all options.";

        // Process with small chunks
        let test_ui = process_chunked_text(input, 5);

        // Get and verify the fragments
        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        // Check that thinking text is properly tagged
        let mut found_plain_text = false;
        let mut found_thinking_text = false;

        for fragment in &fragments {
            match fragment {
                DisplayFragment::PlainText(text) => {
                    if text.contains("Let me think") || text.contains("I've considered") {
                        found_plain_text = true;
                    }
                    // Thinking tags should not appear in plain text
                    assert!(
                        !text.contains("<thinking>"),
                        "Thinking tag should not be visible"
                    );
                    assert!(
                        !text.contains("</thinking>"),
                        "Thinking end tag should not be visible"
                    );
                }
                DisplayFragment::ThinkingText(text) => {
                    if text.contains("complex problem") {
                        found_thinking_text = true;
                    }
                }
                _ => {}
            }
        }

        assert!(found_plain_text, "Should find plain text content");
        assert!(found_thinking_text, "Should find thinking text content");

        Ok(())
    }
}
