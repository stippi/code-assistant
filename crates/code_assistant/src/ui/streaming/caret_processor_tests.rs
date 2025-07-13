
//! Tests for the caret stream processor
//!
//! # Test Strategy Overview
//!
//! These tests validate the caret processor's streaming behavior, particularly
//! its ability to handle chunked input where caret syntax may be split across
//! chunk boundaries at arbitrary positions.
//!
//! ## Test Categories
//!
//! ### 1. Basic Functionality Tests
//! - Simple tool invocations
//! - Parameter parsing
//! - Message fragment extraction
//!
//! ### 2. Streaming & Chunking Tests
//! - Tool syntax split across chunks
//! - Incomplete syntax at buffer boundaries
//! - Various chunk sizes to catch edge cases
//!
//! ### 3. Edge Case & Validation Tests
//! - False positive prevention (^^^not_a_tool in middle of line)
//! - Incomplete tool syntax handling
//! - Raw vs merged fragment comparison
//!
//! ## Key Testing Insights
//!
//! ### The Chunking Challenge
//!
//! The most important tests verify that caret syntax works correctly when
//! split across arbitrary chunk boundaries. For example:
//!
//! Input: "Hello\n^^^tool_name\nparam: value\n^^^"
//!
//! Could be chunked as:
//! - Chunk 1: "Hel"
//! - Chunk 2: "lo\n^"
//! - Chunk 3: "^^tool_na"
//! - Chunk 4: "me\nparam: val"
//! - Chunk 5: "ue\n^^^"
//!
//! The processor must:
//! 1. Emit "Hello\n" immediately (not caret syntax)
//! 2. Buffer "^" then "^^tool_na" then recognize complete "^^^tool_name"
//! 3. Process parameter and tool end correctly
//! 4. Produce same result as non-chunked processing
//!
//! ### State Awareness Testing
//!
//! Tests must verify that the processor correctly distinguishes between:
//! - Lines outside tool blocks (mostly emit as plain text)
//! - Lines inside tool blocks (parameter parsing needed)
//! - Invalid caret syntax (should not trigger tool processing)
//!
//! ### Fragment Comparison Strategy
//!
//! We test both:
//! - **Raw fragments**: Individual pieces emitted during streaming
//! - **Merged fragments**: Final result after TestUI merges adjacent text
//!
//! This catches issues where streaming produces correct individual pieces
//! but they don't merge to the expected final result.
//!
//! ## Implementation Roadmap
//!
//! Based on the current test results, the implementation needs:
//!
//! ### 1. Complete Buffering Logic ⚠️
//! - Fix incomplete line completion when new chunks arrive
//! - Ensure buffered "^^^" patterns get re-evaluated
//! - Handle tool end processing in chunked scenarios
//!
//! ### 2. Parameter Parsing 📋
//! - Array syntax: `key: [` followed by elements, ended by `]`
//! - Multiline syntax: `key ---` followed by content, ended by `--- key`
//! - Parameter validation and error handling
//!
//! ### 3. State Management Improvements 🔄
//! - Better tracking of multiline parameter collection
//! - Array element collection state
//! - Error recovery from malformed syntax
//!
//! ### 4. Edge Case Handling 🛡️
//! - Tool syntax at very end of input (no trailing newline)
//! - Nested arrays or complex parameter structures
//! - Invalid syntax within tool blocks
//!
//! ### 5. Performance Optimizations ⚡
//! - Reduce regex compilations
//! - More efficient string handling for large multiline parameters
//! - Memory usage optimization for long-running streams
//!
//! ## Test Coverage Goals
//!
//! - ✅ Basic text processing with whitespace preservation
//! - ✅ Simple tool recognition (non-chunked)
//! - ⚠️ Chunked tool processing (needs implementation)
//! - 📋 Array parameter parsing (needs implementation)
//! - 📋 Multiline parameter parsing (needs implementation)
//! - 📋 Complex chunking scenarios (needs implementation)
//! - 📋 Error handling and recovery (needs implementation)

use super::test_utils::{assert_fragments_match, chunk_str, TestUI};
use crate::ui::streaming::{CaretStreamProcessor, DisplayFragment, StreamProcessorTrait};
use crate::ui::UserInterface;
use llm::{Message, MessageContent, MessageRole, StreamingChunk};
use std::sync::Arc;

/// Process input text with a stream processor, breaking it into chunks
///
/// This is the core test utility that simulates real streaming conditions.
/// It takes arbitrary text and splits it into chunks of the specified size,
/// then processes each chunk individually through the caret processor.
///
/// # Why This Matters
///
/// In real usage, streaming content arrives in unpredictable chunks:
/// - Network packets of varying sizes
/// - LLM token boundaries
/// - Buffer boundaries in the HTTP stack
///
/// The caret processor must produce identical results regardless of how
/// the input is chunked. This function lets us test that guarantee by
/// trying many different chunk sizes on the same input.
///
/// # Parameters
/// - `text`: The complete input to process
/// - `chunk_size`: Size of each chunk (in characters)
///
/// # Returns
/// TestUI containing all emitted fragments, both raw and merged
fn process_chunked_text(text: &str, chunk_size: usize) -> TestUI {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Split text into small chunks and process each one
    for chunk in chunk_str(text, chunk_size) {
        if let Err(e) = processor.process(&StreamingChunk::Text(chunk)) {
            // Unlike XML processor, caret processor doesn't have tool limits yet
            panic!("Unexpected error: {}", e);
        }
    }

    test_ui
}

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

#[test]
fn test_regex_behavior_with_partial_matches() {
    // Test find() vs captures() behavior with partial text
    use regex::Regex;
    let tool_regex = Regex::new(r"(?m)^\^\^\^([a-zA-Z0-9_]+)$").unwrap();

    let test_cases = [
        ("^^^real_tool\n", true, Some("real_tool")),
        ("^^^real", false, None), // Partial - no end of line
        ("^^^real_", false, None), // Partial - no end of line
        ("text\n^^^real", false, None), // Partial - no end of line
        ("text\n^^^real_tool", true, Some("real_tool")), // Complete at end of string
    ];

    for (input, should_find, expected_capture) in test_cases {
        let find_result = tool_regex.find(input).is_some();
        let capture_result = tool_regex.captures(input).and_then(|caps| caps.get(1)).map(|m| m.as_str());

        println!("Input: '{}' -> Find: {}, Capture: {:?}", input, find_result, capture_result);
        assert_eq!(find_result, should_find, "Find result mismatch for: '{}'", input);
        assert_eq!(capture_result, expected_capture, "Capture result mismatch for: '{}'", input);
    }
}

/// Test that demonstrates the core line-oriented requirement of caret syntax
///
/// # Critical Behavior Validation
///
/// This test verifies that:
/// 1. "^^^not_a_tool" in middle of line is NOT processed as tool syntax
/// 2. "^^^real_tool" at start of line IS processed as tool syntax
/// 3. Chunking doesn't affect this behavior
///
/// # Why This Test Is Essential
///
/// This prevents false positives where caret-like text in regular content
/// accidentally triggers tool processing. The processor must only recognize
/// caret syntax when it appears at the beginning of a line.
///
/// # Current Status: ⚠️ NEEDS IMPLEMENTATION
///
/// The test currently fails for chunked processing because:
/// - The basic caret line recognition works
/// - But incomplete lines aren't properly completed when new chunks arrive
/// - Tool end processing is missing in chunked scenarios
///
/// # Expected vs Actual (Chunked)
/// Expected: [PlainText, ToolName, ToolEnd]
/// Actual: [PlainText] (incomplete - tool syntax held in buffer)
#[test]
fn test_caret_must_start_at_line_beginning() {
    // Test that caret syntax in the middle of a line is NOT recognized as tool syntax
    // Add trailing newline so the closing ^^^ is processed as a complete line
    let input = "Some text ^^^not_a_tool and more text\n^^^real_tool\n^^^\n";

    let expected_fragments = vec![
        DisplayFragment::PlainText("Some text ^^^not_a_tool and more text\n".to_string()),
        DisplayFragment::ToolName {
            name: "real_tool".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    // First test with no chunking to see if it works at all
    let test_ui = process_chunked_text(input, input.len());
    let fragments = test_ui.get_fragments();
    println!("No chunking - Actual fragments: {:?}", fragments);

    // Then test with chunking
    let test_ui = process_chunked_text(input, 5);
    let fragments = test_ui.get_fragments();
    println!("With chunking - Actual fragments: {:?}", fragments);

    assert_fragments_match(&expected_fragments, &fragments);
}

#[test]
fn test_simple_text_processing() {
    // Test that simple text without caret syntax works correctly
    let input = "Hello world\nThis is a test\n";

    let expected_fragments = vec![
        DisplayFragment::PlainText("Hello world\nThis is a test\n".to_string()),
    ];

    let test_ui = process_chunked_text(input, 1);
    let raw_fragments = test_ui.get_raw_fragments();
    let merged_fragments = test_ui.get_fragments();

    println!("Simple text - Raw fragments: {:?}", raw_fragments);
    println!("Simple text - Merged fragments: {:?}", merged_fragments);

    // Check a few raw fragments to see what's being emitted
    if raw_fragments.len() > 5 {
        println!("First few raw fragments:");
        for (i, frag) in raw_fragments.iter().take(5).enumerate() {
            println!("  [{}]: {:?}", i, frag);
        }
    }

    assert_fragments_match(&expected_fragments, &merged_fragments);
}

/// Test the most complex chunking scenario: tool syntax spanning multiple chunks
///
/// # What This Tests
///
/// This is the "stress test" for the streaming processor. It validates that
/// complete tool invocations work correctly even when the caret syntax is
/// split across chunk boundaries at every possible position.
///
/// # Chunking Scenarios Tested
///
/// With chunk size 1, the input gets split as:
/// "L", "e", "t", " ", "m", "e", " ", "h", "e", "l", "p", " ", "y", "o", "u", ".", "\n",
/// "^", "^", "^", "r", "e", "a", "d", "_", "f", "i", "l", "e", "s", "\n",
/// "p", "r", "o", "j", "e", "c", "t", ":", " ", "t", "e", "s", "t", "\n",
/// "^", "^", "^", "\n"
///
/// The processor must:
/// 1. Emit regular text immediately: "Let me help you.\n"
/// 2. Buffer and recognize: "^^^read_files" as complete tool start
/// 3. Parse parameter: "project: test"
/// 4. Recognize tool end: "^^^"
///
/// # Implementation Requirements
///
/// For this test to pass, the processor needs:
/// 1. Proper buffering of incomplete "^^^" patterns
/// 2. Recognition when buffered content becomes complete
/// 3. State transitions: outside tool → inside tool → outside tool
/// 4. Parameter parsing within tool blocks
///
/// # Current Status: ⚠️ NEEDS IMPLEMENTATION
///
/// Currently fails because buffered caret lines are never re-evaluated
/// when new chunks arrive to complete them.
#[test]
fn test_caret_chunked_across_tool_opening() {
    // Test that tool opening can be split across chunks at various positions
    // Add trailing newline so tool end is complete
    let input = "Let me help you.\n^^^read_files\nproject: test\n^^^\n";

    let expected_fragments = vec![
        DisplayFragment::PlainText("Let me help you.\n".to_string()),
        DisplayFragment::ToolName {
            name: "read_files".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "project".to_string(),
            value: "test".to_string(),
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    // Test with different chunk sizes to catch edge cases
    for chunk_size in [1, 2, 3, 5, 7, 10] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();

        println!("Chunk size: {}, Fragments: {:?}", chunk_size, fragments);
        assert_fragments_match(&expected_fragments, &fragments);
    }
}

#[test]
fn test_caret_chunked_across_tool_closing() {
    // Test that tool closing can be split across chunks
    let input = "^^^list_projects\n^^^";

    let expected_fragments = vec![
        DisplayFragment::ToolName {
            name: "list_projects".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    // Test with very small chunks that will split the closing "^^^"
    for chunk_size in [1, 2, 3] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }
}

#[test]
fn test_caret_array_syntax_proper_formatting() {
    // Test that arrays must have proper formatting with ] on its own line
    let input = "^^^read_files\nproject: test\npaths: [\nsrc/main.rs\nCargo.toml\n]\n^^^";

    let expected_fragments = vec![
        DisplayFragment::ToolName {
            name: "read_files".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "project".to_string(),
            value: "test".to_string(),
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "paths".to_string(),
            value: "[src/main.rs, Cargo.toml]".to_string(), // This is how the current implementation formats arrays
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    let test_ui = process_chunked_text(input, 3);
    let fragments = test_ui.get_fragments();

    assert_fragments_match(&expected_fragments, &fragments);
}

#[test]
fn test_caret_array_syntax_chunked() {
    // Test array syntax when chunked at various positions
    let input = "^^^read_files\nproject: test\npaths: [\nsrc/main.rs\nCargo.toml\n]\n^^^";

    let expected_fragments = vec![
        DisplayFragment::ToolName {
            name: "read_files".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "project".to_string(),
            value: "test".to_string(),
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "paths".to_string(),
            value: "[src/main.rs, Cargo.toml]".to_string(),
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    // Test with various chunk sizes to test chunking across array boundaries
    for chunk_size in [1, 2, 4, 8] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }
}

#[test]
fn test_caret_multiline_parameter_chunked() {
    // Test multiline parameters when chunked across the --- markers
    let input = "^^^write_file\nproject: test\ncontent ---\nHello\nWorld\n--- content\n^^^";

    let expected_fragments = vec![
        DisplayFragment::ToolName {
            name: "write_file".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "project".to_string(),
            value: "test".to_string(),
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "content".to_string(),
            value: "Hello\nWorld".to_string(),
            tool_id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    // Test with chunks that will split across the --- markers
    for chunk_size in [1, 3, 5, 7] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();
        assert_fragments_match(&expected_fragments, &fragments);
    }
}

#[test]
fn test_caret_false_positive_prevention() {
    // Test that text containing ^^^ patterns that aren't valid tools is handled correctly
    let input = "Here's some code:\n```\n^^^not-a-tool\n```\nAnd here's a real tool:\n^^^list_projects\n^^^";

    let expected_fragments = vec![
        DisplayFragment::PlainText("Here's some code:\n```\n^^^not-a-tool\n```\nAnd here's a real tool:".to_string()),
        DisplayFragment::ToolName {
            name: "list_projects".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "ignored".to_string(),
        },
    ];

    let test_ui = process_chunked_text(input, 4);
    let fragments = test_ui.get_fragments();

    assert_fragments_match(&expected_fragments, &fragments);
}

#[test]
fn test_caret_incomplete_tool_at_buffer_end() {
    // Test that incomplete tool syntax at the end of a buffer is not processed prematurely
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Send partial tool syntax
    processor.process(&StreamingChunk::Text("^^^list_proj".to_string())).unwrap();

    // At this point, no tool should be emitted yet (it's incomplete)
    let fragments = test_ui.get_fragments();
    assert!(fragments.is_empty(), "No fragments should be emitted for incomplete tool");

    // Complete the tool
    processor.process(&StreamingChunk::Text("ects\n^^^".to_string())).unwrap();

    // Now the tool should be complete
    let fragments = test_ui.get_fragments();
    assert!(fragments.len() >= 2, "Should have tool name and tool end");

    let tool_name = fragments.iter().find(|f| {
        matches!(f, DisplayFragment::ToolName { name, .. } if name == "list_projects")
    });
    assert!(tool_name.is_some(), "Should find the complete tool name");
}

#[test]
fn test_caret_raw_vs_merged_fragments() {
    // Test that raw fragments (before merging) contain individual pieces
    // while merged fragments combine adjacent text
    let input = "Let me\n help you\n\n^^^read_files\nproject: test\npath: src/main.rs\n^^^";

    let test_ui = process_chunked_text(input, 3); // Small chunks to create multiple text fragments

    let raw_fragments = test_ui.get_raw_fragments();
    let merged_fragments = test_ui.get_fragments();

    // Raw fragments should have more individual text pieces
    let raw_text_count = raw_fragments.iter().filter(|f| matches!(f, DisplayFragment::PlainText(_))).count();
    let merged_text_count = merged_fragments.iter().filter(|f| matches!(f, DisplayFragment::PlainText(_))).count();

    // Due to chunking, we should have more raw text fragments than merged ones
    assert!(raw_text_count >= merged_text_count,
        "Raw fragments ({}) should have at least as many text fragments as merged ({})",
        raw_text_count, merged_text_count);

    // The merged result should still be correct
    let expected_fragments = vec![
        DisplayFragment::PlainText("Let me help you".to_string()),
        DisplayFragment::ToolName {
            name: "read_files".to_string(),
            id: "ignored".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "project".to_string(),
            value: "test".to_string(),
            tool_id: "ignored".to_string(),
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

    assert_fragments_match(&expected_fragments, &merged_fragments);
}
