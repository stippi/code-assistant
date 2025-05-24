#[cfg(test)]
mod tests {
    use super::super::test_utils::{assert_fragments_match, TestUI};
    use super::super::{JsonStreamProcessor, StreamProcessorTrait};
    use crate::ui::DisplayFragment;
    use llm::StreamingChunk;
    use std::sync::Arc;

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
