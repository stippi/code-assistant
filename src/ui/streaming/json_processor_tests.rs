#[cfg(test)]
mod json_processor_tests {
    use crate::llm::StreamingChunk;
    use crate::ui::streaming::test_utils::{assert_fragments_match, TestUI};
    use crate::ui::streaming::{JsonStreamProcessor, StreamProcessorTrait};
    use crate::ui::DisplayFragment;
    use std::sync::Arc;

    // Test helper to process JSON chunks with the JSON processor
    fn process_json_chunks(
        chunks: &[&str],
        tool_name: &str,
        tool_id: &str,
    ) -> Vec<DisplayFragment> {
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

    #[test]
    fn test_basic_json_param_parsing() {
        let json = r#"{"path": "src/main.rs"}"#;
        let chunks = vec![json];
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
    fn test_multi_param_json_parsing() {
        let json = r#"{"regex": "fn main", "paths": ["src", "lib"]}"#;
        let chunks = vec![json];

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
    fn test_chunked_json_processing() {
        // Split JSON into multiple chunks to test streaming
        let chunks = vec![
            r#"{"pa"#,
            r#"th": "src/m"#,
            r#"ain.rs"#,
            r#"", "max_depth": 2}"#,
        ];

        let expected_fragments = vec![
            DisplayFragment::ToolName {
                name: "read_files".to_string(),
                id: "read-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "path".to_string(),
                value: "src/main.rs".to_string(),
                tool_id: "read-123".to_string(),
            },
            DisplayFragment::ToolParameter {
                name: "max_depth".to_string(),
                value: "2".to_string(),
                tool_id: "read-123".to_string(),
            },
        ];

        let fragments = process_json_chunks(&chunks, "read_files", "read-123");
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_large_parameter_value_streaming() {
        // Test with a large parameter value to ensure it gets streamed incrementally
        let large_content = "This is a very large content that should be streamed incrementally rather than waiting for the entire value to be complete.";

        let json_start = r#"{"content": ""#;
        let json_end = r#"", "path": "test.txt"}"#;

        // Split the large content into small chunks to test streaming
        let content_chunks: Vec<&str> = large_content
            .as_bytes()
            .chunks(10)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();

        // Create JSON chunks
        let mut chunks = vec![json_start];
        chunks.extend(content_chunks);
        chunks.push(json_end);

        // Expected fragments should include incremental updates for the large content
        let mut expected_fragments = vec![DisplayFragment::ToolName {
            name: "write_file".to_string(),
            id: "write-123".to_string(),
        }];

        // The TestUI's merge_fragments will combine the parameter values
        expected_fragments.push(DisplayFragment::ToolParameter {
            name: "content".to_string(),
            value: large_content.to_string(),
            tool_id: "write-123".to_string(),
        });

        expected_fragments.push(DisplayFragment::ToolParameter {
            name: "path".to_string(),
            value: "test.txt".to_string(),
            tool_id: "write-123".to_string(),
        });

        let fragments = process_json_chunks(&chunks.as_slice(), "write_file", "write-123");
        assert_fragments_match(&expected_fragments, &fragments);
    }

    #[test]
    fn test_escaped_quotes_in_json() {
        let json = r#"{"content": "Line with \"quoted\" text inside."}"#;
        let chunks = vec![json];

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
        let chunks = vec![json];

        // Due to streaming, nested objects might be split into multiple fragments
        let fragments = process_json_chunks(&chunks, "list_files", "list-123");

        // Verify tool name
        assert!(fragments.iter().any(|fragment| {
            match fragment {
                DisplayFragment::ToolName { name, id } => name == "list_files" && id == "list-123",
                _ => false,
            }
        }));

        // Check that options parameter fragments combined contain the nested object properties
        let option_values: Vec<String> = fragments
            .iter()
            .filter_map(|fragment| match fragment {
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    if name == "options" && tool_id == "list-123" {
                        Some(value.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();

        let combined_options = option_values.join("");
        assert!(combined_options.contains("recursive"));
        assert!(combined_options.contains("true"));
        assert!(combined_options.contains("followSymlinks"));
        assert!(combined_options.contains("false"));
    }
}
