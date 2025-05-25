use crate::ui::streaming::test_utils::{
    assert_fragments_match, chunk_str, print_fragments, TestUI,
};
use crate::ui::streaming::{JsonStreamProcessor, StreamProcessorTrait};
use crate::ui::DisplayFragment;
use llm::StreamingChunk;
use std::sync::Arc;

// Helper function to process regular text chunks using the JSON processor
fn process_text_chunks(text: &str, chunk_size: usize) -> Vec<DisplayFragment> {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn crate::ui::UserInterface>);
    let mut processor = JsonStreamProcessor::new(ui_arc);

    // Split text into small chunks and process each one
    for chunk in chunk_str(text, chunk_size) {
        processor.process(&StreamingChunk::Text(chunk)).unwrap();
    }

    test_ui.get_fragments()
}

// Test helper to process JSON chunks with the JSON processor
fn process_json_chunks(chunks: &[String], tool_name: &str, tool_id: &str) -> Vec<DisplayFragment> {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn crate::ui::UserInterface>);
    let mut processor = JsonStreamProcessor::new(ui_arc);

    // Process each chunk
    for (i, chunk) in chunks.iter().enumerate() {
        let name = if i == 0 {
            Some(tool_name.to_string())
        } else {
            None
        };
        let id = if i == 0 {
            Some(tool_id.to_string())
        } else {
            None
        };

        processor
            .process(&StreamingChunk::InputJson {
                content: chunk.to_string(),
                tool_name: name,
                tool_id: id,
            })
            .unwrap();
    }

    test_ui.get_fragments()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_json_param_parsing() {
        let json = r#"{"path": "src/main.rs"}"#;
        let chunks = chunk_str(json, 5);

        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "test-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "src/main.rs".to_string(),
                tool_id: "test-123".to_string(),
            },
        ];

        let fragments = process_json_chunks(&chunks, "read_files", "test-123");
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_array_param_json_parsing() {
        let json = r#"{"regex": "fn main", "paths": ["src", "lib"]}"#;
        let chunks = chunk_str(json, 6);

        // Due to the streaming nature, the array parameter might be split into multiple fragments
        // that get merged by the TestUI. We just check that the fragments contain what we need.
        let fragments = process_json_chunks(&chunks, "search_files", "search-123");

        // Check if we have the tool name fragment
        assert!(fragments.iter().any(|fragment| {
            match fragment {
                DisplayFragment::ToolName { name, id } => {
                    name == "search_files" && id == "search-123"
                }
                _ => false,
            }
        }));

        // Check if we have the regex parameter
        assert!(fragments.iter().any(|fragment| {
            match fragment {
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => name == "regex" && value == "fn main" && tool_id == "search-123",
                _ => false,
            }
        }));

        // Check if all paths parameter fragments combined contain both "src" and "lib"
        let path_values: Vec<String> = fragments
            .iter()
            .filter_map(|fragment| match fragment {
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    if name == "paths" && tool_id == "search-123" {
                        Some(value.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();

        let combined_paths = path_values.join("");
        assert!(combined_paths.contains("src"));
        assert!(combined_paths.contains("lib"));
    }

    #[test]
    fn test_large_parameter_value_streaming() {
        // Test with a large parameter value to ensure it gets streamed incrementally
        let large_content = "This is a very large content that should be streamed incrementally rather than waiting for the entire value to be complete.";

        // Create the complete JSON
        let json = format!(r#"{{"content": "{}", "path": "test.txt"}}"#, large_content);

        // Split into small chunks using the helper
        let chunks = chunk_str(&json, 5);

        // Expected fragments should include incremental updates for the large content
        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "write_file".to_string(),
                id: "write-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "content".to_string(),
                value: large_content.to_string(),
                tool_id: "write-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "test.txt".to_string(),
                tool_id: "write-123".to_string(),
            },
        ];

        let fragments = process_json_chunks(&chunks, "write_file", "write-123");
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_escaped_quotes_in_json() {
        let json = r#"{"content": "Line with \"quoted\" text inside."}"#;
        let chunks = chunk_str(json, 5);

        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "write_file".to_string(),
                id: "write-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "content".to_string(),
                value: r#"Line with "quoted" text inside."#.to_string(),
                tool_id: "write-123".to_string(),
            },
        ];

        let fragments = process_json_chunks(&chunks, "write_file", "write-123");
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_nested_json_objects() {
        let json = r#"{"options": {"recursive": true, "followSymlinks": false}}"#;
        let chunks = chunk_str(json, 3);

        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "list_files".to_string(),
                id: "list-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "options".to_string(),
                value: r#"{"recursive": true, "followSymlinks": false}"#.to_string(),
                tool_id: "list-123".to_string(),
            },
        ];

        // Due to streaming, nested objects might be split into multiple fragments
        let fragments = process_json_chunks(&chunks, "list_files", "list-123");
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    // Tests for the text processing functionality with thinking tags

    #[test]
    fn test_simple_thinking_tag_handling() {
        let input = "Let me think about this.\n<thinking>\nThis is a complex problem.\n</thinking>\nI've decided.";

        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me think about this.".to_string()),
            DisplayFragment::ThinkingText("This is a complex problem.".to_string()),
            DisplayFragment::PlainText("I've decided.".to_string()),
        ];

        // Process with small chunks to test tag handling across chunks
        let fragments = process_text_chunks(input, 5);
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_multiple_thinking_blocks() {
        let input = "Working on it. <thinking>First consideration.</thinking> Progress.\n<thinking>Second consideration with\nmultiple lines.</thinking>Result.";

        let expected_fragments = vec![
            DisplayFragment::PlainText("Working on it.".to_string()),
            DisplayFragment::ThinkingText("First consideration.".to_string()),
            DisplayFragment::PlainText("Progress.".to_string()),
            DisplayFragment::ThinkingText("Second consideration with\nmultiple lines.".to_string()),
            DisplayFragment::PlainText("Result.".to_string()),
        ];

        // Use a larger chunk size
        let fragments = process_text_chunks(input, 10);
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_thinking_tag_with_partial_chunks() {
        // Test with thinking tags split across chunk boundaries
        let input = "Let me analyze: <thinking>This requires careful analysis of the problem.</thinking> Done.";

        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me analyze:".to_string()),
            DisplayFragment::ThinkingText(
                "This requires careful analysis of the problem.".to_string(),
            ),
            DisplayFragment::PlainText("Done.".to_string()),
        ];

        // Use a very small chunk size (3) to ensure tags get split
        let fragments = process_text_chunks(input, 3);
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_normal_text_without_thinking_tags() {
        let input = "This is just regular text without any special tags.";

        let expected_fragments = vec![DisplayFragment::PlainText(
            "This is just regular text without any special tags.".to_string(),
        )];

        let fragments = process_text_chunks(input, 8);
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_text_with_angle_brackets_but_not_thinking_tags() {
        let input = "This text has <angle brackets> but they're not thinking tags.";

        let expected_fragments = vec![DisplayFragment::PlainText(
            "This text has <angle brackets> but they're not thinking tags.".to_string(),
        )];

        let fragments = process_text_chunks(input, 10);
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_incomplete_thinking_tag_at_chunk_boundary() {
        // This test ensures proper handling of partially complete tags
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn crate::ui::UserInterface>);
        let mut processor = JsonStreamProcessor::new(ui_arc);

        // First chunk ends with incomplete tag
        processor
            .process(&StreamingChunk::Text("Let me think <thin".to_string()))
            .unwrap();
        // Second chunk continues the tag
        processor
            .process(&StreamingChunk::Text(
                "king>Analysis goes here.</thinkin".to_string(),
            ))
            .unwrap();
        // Third chunk completes the end tag
        processor
            .process(&StreamingChunk::Text("g> Done.".to_string()))
            .unwrap();

        let expected_fragments = vec![
            DisplayFragment::PlainText("Let me think".to_string()),
            DisplayFragment::ThinkingText("Analysis goes here.".to_string()),
            DisplayFragment::PlainText("Done.".to_string()),
        ];

        let fragments = test_ui.get_fragments();
        print_fragments(&fragments);

        assert_fragments_match(&expected_fragments, &fragments);
    }

    // Tests for chunked/partial input JSON

    #[test]
    fn test_realistic_anthropic_chunks() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn crate::ui::UserInterface>);
        let mut processor = JsonStreamProcessor::new(ui_arc);

        // Realistic chunks from Anthropic API - simplified
        let chunks = vec![
            // Tool start
            (
                Some("write_file"),
                Some("toolu_01UMyVAc3ZiT4V2jNAiBgRoq"),
                "",
            ),
            // Empty JSON start
            (None, None, ""),
            // Start of project parameter
            (None, None, "{\"project\":"),
            // Project value chunks
            (None, None, " \"code-assi"),
            (None, None, "stan"),
            (None, None, "t\""),
            // Path parameter start
            (None, None, ", \"path\": "),
            // Path value chunks
            (None, None, "\"vibe-codi"),
            (None, None, "ng.md\""),
            // Content parameter start
            (None, None, ", \"conte"),
            (None, None, "nt\": \"AI Coding"),
            (None, None, " Assistants"),
            (None, None, ": Augmenting Human Potential"),
            // Close the JSON
            (None, None, "\"}"),
        ];

        // Process each chunk
        for (tool_name, tool_id, content) in chunks {
            let chunk = StreamingChunk::InputJson {
                content: content.to_string(),
                tool_name: tool_name.map(|s| s.to_string()),
                tool_id: tool_id.map(|s| s.to_string()),
            };

            if let Err(e) = processor.process(&chunk) {
                eprintln!("Error processing chunk {}: {}", content, e);
            }
        }

        let fragments = test_ui.get_fragments();

        // Print for debugging
        println!("Collected {} fragments:", fragments.len());
        for (i, fragment) in fragments.iter().enumerate() {
            match fragment {
                DisplayFragment::ToolName { name, id } => {
                    println!("  [{}] ToolName: {} (id: {})", i, name, id);
                }
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    println!(
                        "  [{}] ToolParameter: {} = {} (tool_id: {})",
                        i, name, value, tool_id
                    );
                }
                _ => println!("  [{}] Other: {:?}", i, fragment),
            }
        }

        // Basic assertions
        assert!(
            fragments.len() >= 4,
            "Should have at least tool name + 3 parameters"
        );

        // Check tool name
        assert!(
            fragments.iter().any(|f| matches!(f,
                DisplayFragment::ToolName { name, id }
                if name == "write_file" && id == "toolu_01UMyVAc3ZiT4V2jNAiBgRoq"
            )),
            "Should have correct tool name"
        );

        // Check that all parameters are present with reasonable content
        let param_names: Vec<String> = fragments
            .iter()
            .filter_map(|f| match f {
                DisplayFragment::ToolParameter { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();

        println!("Found parameter names: {:?}", param_names);

        // Check for expected parameters (allowing for duplicates due to streaming)
        assert!(
            param_names.iter().any(|name| name == "project"),
            "Should have project parameter"
        );
        assert!(
            param_names.iter().any(|name| name == "path"),
            "Should have path parameter"
        );
        assert!(
            param_names.iter().any(|name| name == "content"),
            "Should have content parameter"
        );

        // Check project value
        let project_values: Vec<String> = fragments
            .iter()
            .filter_map(|f| match f {
                DisplayFragment::ToolParameter { name, value, .. } if name == "project" => {
                    Some(value.clone())
                }
                _ => None,
            })
            .collect();

        let combined_project = project_values.join("");
        assert!(
            combined_project.contains("code-assistant"),
            "Project value should contain code-assistant"
        );

        // Check path value
        let path_values: Vec<String> = fragments
            .iter()
            .filter_map(|f| match f {
                DisplayFragment::ToolParameter { name, value, .. } if name == "path" => {
                    Some(value.clone())
                }
                _ => None,
            })
            .collect();

        let combined_path = path_values.join("");
        assert!(
            combined_path.contains("vibe-coding.md"),
            "Path value should contain vibe-coding.md"
        );

        // Check content value
        let content_values: Vec<String> = fragments
            .iter()
            .filter_map(|f| match f {
                DisplayFragment::ToolParameter { name, value, .. } if name == "content" => {
                    Some(value.clone())
                }
                _ => None,
            })
            .collect();

        let combined_content = content_values.join("");
        assert!(
            combined_content.contains("AI Coding Assistants"),
            "Content should contain AI Coding Assistants"
        );
        assert!(
            combined_content.contains("Augmenting Human Potential"),
            "Content should contain the subtitle"
        );
    }

    #[test]
    fn test_parameter_name_parsing() {
        let test_ui = TestUI::new();
        let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn crate::ui::UserInterface>);
        let mut processor = JsonStreamProcessor::new(ui_arc);

        // Test the specific pattern that was causing "::" parameter names
        let chunks = vec![
            (Some("write_file"), Some("test-123"), ""),
            (None, None, r#"{"project": "code-assistant""#),
            // This comma + space pattern was being parsed as parameter name
            (None, None, r#", "path": "test.txt"}"#),
        ];

        for (tool_name, tool_id, content) in chunks {
            let chunk = StreamingChunk::InputJson {
                content: content.to_string(),
                tool_name: tool_name.map(|s| s.to_string()),
                tool_id: tool_id.map(|s| s.to_string()),
            };

            processor.process(&chunk).unwrap();
        }

        let fragments = test_ui.get_fragments();

        println!(
            "Parameter name test - Collected {} fragments:",
            fragments.len()
        );
        for (i, fragment) in fragments.iter().enumerate() {
            match fragment {
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    println!(
                        "  [{}] ToolParameter: {} = {} (tool_id: {})",
                        i, name, value, tool_id
                    );
                }
                _ => println!("  [{}] {:?}", i, fragment),
            }
        }

        // Make sure we don't get weird parameter names like "::"
        for fragment in &fragments {
            if let DisplayFragment::ToolParameter { name, .. } = fragment {
                assert!(!name.is_empty(), "Parameter name should not be empty");
                assert!(name != "::", "Parameter name should not be ::");
                assert!(name != ": ", "Parameter name should not be : ");
                assert!(
                    !name.contains(','),
                    "Parameter name should not contain comma"
                );
            }
        }

        // Check we get the correct parameter names
        let param_names: Vec<String> = fragments
            .iter()
            .filter_map(|f| match f {
                DisplayFragment::ToolParameter { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();

        assert!(
            param_names.iter().any(|name| name == "project"),
            "Should have project parameter"
        );
        assert!(
            param_names.iter().any(|name| name == "path"),
            "Should have path parameter"
        );
    }
}
