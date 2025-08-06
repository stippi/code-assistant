//! Tests for the caret stream processor
//!
//! # Current Test Status: 7/21 Passing ✅
//!
//! ## ✅ Passing Tests (Core Functionality Working)
//!
//! - `test_simple_text_processing` - Basic text without caret syntax ✅
//! - `test_caret_must_start_at_line_beginning` - Caret positioning validation ✅
//! - `test_caret_simple_tool` - Basic tool invocation ✅
//! - `test_extract_fragments_from_complete_message` - Message processing ✅
//! - All `tools::parse::tests::test_parse_caret_*` - Non-streaming parser ✅
//!
//! ## ❌ Failing Tests (Known Issues)
//!
//! ### Chunking Issues (High Priority)
//! - `test_caret_chunked_across_tool_closing` - **CRITICAL**: Tool end not processed with small chunks
//! - `test_caret_chunked_across_tool_opening` - Parameters emitted as PlainText instead of ToolParameter
//! - **Root Cause**: Buffering strategy too conservative for chunk sizes 2+
//!
//! ### Parameter Processing (High Priority)
//! - Tests expecting `ToolParameter` fragments get `PlainText` instead
//! - **Issue**: Inside tool blocks, `"project: test"` not recognized as parameter
//! - **Status**: Basic infrastructure exists, parsing logic incomplete
//!
//! ### Advanced Features (Medium Priority)
//! - `test_caret_array_syntax_*` - Array parameter parsing not implemented
//! - `test_caret_multiline_*` - Multiline parameter parsing not implemented
//! - **Status**: State tracking exists, but parsing logic missing
//!
//! ### Edge Cases (Low Priority)
//! - `test_caret_incomplete_tool_at_buffer_end` - Buffer finalization incomplete
//! - `test_caret_false_positive_prevention` - Some chunking edge cases
//!
//! # Test Strategy & Insights
//!
//! ## The Chunking Challenge: Critical Insight
//!
//! **The core testing challenge**: Caret syntax must work identically regardless
//! of how input is chunked. A tool invocation like:
//!
//! ```text
//! "Let me help.\n^^^read_files\nproject: test\n^^^"
//! ```
//!
//! **Must produce identical results** whether processed as:
//! - Single chunk (size = length)
//! - Large chunks (size = 10)
//! - Medium chunks (size = 5)
//! - Small chunks (size = 2) ⚠️ **Currently failing**
//! - Tiny chunks (size = 1) ✅ **Works**
//!
//! **Why size 1 works but size 2+ fails:**
//! - Size 1: Very conservative buffering, eventually processes correctly
//! - Size 2+: Buffering too aggressive, entire input held until finalization
//! - **Fix needed**: Smarter buffering decisions in `should_buffer_*` methods
//!
//! ## State-Aware Processing
//!
//! Tests validate that the processor correctly handles:
//! - **Outside tool blocks**: Most content → PlainText fragments
//! - **Inside tool blocks**: Parameter lines → ToolParameter fragments
//! - **Syntax validation**: `^^^not_tool` in line middle → PlainText (not tool)
//!
//! ## Fragment Verification Strategy
//!
//! Tests check both:
//! - **Raw fragments**: What processor emits during streaming
//! - **Merged fragments**: Final result after TestUI combines adjacent text
//!
//! This catches subtle issues where individual fragments are correct but
//! don't combine to expected final result.
//!
//! # Implementation Priority Guide
//!
//! ## 🚨 Critical (Blocking most tests)
//! 1. **Fix buffering strategy**: `should_buffer_incomplete_line()` too conservative
//! 2. **Parameter parsing**: Recognize `"key: value"` inside tool blocks
//!
//! ## 🔧 High Impact
//! 3. **Tool end processing**: Ensure `^^^` lines processed in all chunk scenarios
//! 4. **Finalization logic**: Handle incomplete tools at stream end
//!
//! ## 📋 Feature Complete
//! 5. **Array parameters**: `key: [elem1, elem2]` syntax
//! 6. **Multiline parameters**: `key ---\ncontent\n--- key` syntax
//!
//! ## 🎯 Polish
//! 7. **Edge case handling**: Complex chunking scenarios
//! 8. **Error recovery**: Malformed syntax handling
//!
//! **Key Insight**: The foundation is solid. Most failures are due to 1-2 core
//! issues in buffering and parameter recognition, not fundamental design problems.

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
            panic!("Unexpected error: {e}");
        }
    }

    // Note: finalize_buffer() was removed - the processor should handle incomplete content gracefully

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
    assert!(matches!(
        fragments.last(),
        Some(DisplayFragment::ToolEnd { .. })
    ));
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
    // Note: finalize_buffer() was removed - the processor should handle incomplete content gracefully

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
    assert!(matches!(
        fragments.last(),
        Some(DisplayFragment::ToolEnd { .. })
    ));
}

#[tokio::test]
async fn test_extract_fragments_from_complete_message() {
    let test_ui = TestUI::new();
    let ui = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui, 123);

    let message = Message {
        role: MessageRole::Assistant,
        content: MessageContent::Text(
            "I'll create the file for you.\n\n^^^list_projects\n^^^".to_string(),
        ),
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
    let tool_name_fragment = fragments
        .iter()
        .find(|f| matches!(f, DisplayFragment::ToolName { name, .. } if name == "list_projects"));
    assert!(tool_name_fragment.is_some());

    // Check for tool end
    assert!(fragments
        .iter()
        .any(|f| matches!(f, DisplayFragment::ToolEnd { .. })));
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
            id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "tool-42-1".to_string(),
        },
    ];

    // First test with no chunking to see if it works at all
    let test_ui_no_chunking = process_chunked_text(input, input.len());
    let fragments_no_chunking = test_ui_no_chunking.get_fragments();
    println!("No chunking - Actual fragments: {fragments_no_chunking:?}");
    assert_eq!(expected_fragments, fragments_no_chunking);

    // Then test with chunking
    let test_ui_chunked = process_chunked_text(input, 5);
    let fragments_chunked = test_ui_chunked.get_fragments();
    println!("With chunking - Actual fragments: {fragments_chunked:?}");
    assert_eq!(expected_fragments, fragments_chunked);
}

#[test]
fn test_simple_text_processing() {
    // Test that simple text without caret syntax works correctly
    let input = "Hello world\nThis is a test\n";

    let expected_fragments = vec![DisplayFragment::PlainText(
        "Hello world\nThis is a test\n".to_string(),
    )];

    let test_ui = process_chunked_text(input, 1);
    let raw_fragments = test_ui.get_raw_fragments();
    let merged_fragments = test_ui.get_fragments();

    println!("Simple text - Raw fragments: {raw_fragments:?}");
    println!("Simple text - Merged fragments: {merged_fragments:?}");

    // Check a few raw fragments to see what's being emitted
    if raw_fragments.len() > 5 {
        println!("First few raw fragments:");
        for (i, frag) in raw_fragments.iter().take(5).enumerate() {
            println!("  [{i}]: {frag:?}");
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

    // Test with different chunk sizes to catch edge cases
    for chunk_size in [1, 2, 3, 5, 7, 10] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();

        // All chunk sizes should produce the same basic structure
        assert!(fragments.len() >= 4, "Expected at least 4 fragments");

        // Check the essential fragments are present
        assert!(
            matches!(fragments[0], DisplayFragment::PlainText(ref text) if text == "Let me help you.\n")
        );
        assert!(
            matches!(fragments[1], DisplayFragment::ToolName { ref name, .. } if name == "read_files")
        );
        assert!(
            matches!(fragments[2], DisplayFragment::ToolParameter { ref name, ref value, .. }
                        if name == "project" && value == "test")
        );
        assert!(matches!(fragments[3], DisplayFragment::ToolEnd { .. }));

        // After implementing tool blocking: whitespace after complete tool blocks is silently ignored
        // This behavior is consistent regardless of chunk size
        assert_eq!(
            fragments.len(),
            4,
            "All chunk sizes should produce same result - whitespace after tool blocks is ignored"
        );
    }
}

#[test]
fn test_caret_chunked_across_tool_closing() {
    // Test that tool closing can be split across chunks
    let input = "^^^list_projects\n^^^";

    let expected_fragments = vec![
        DisplayFragment::ToolName {
            name: "list_projects".to_string(),
            id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "tool-42-1".to_string(),
        },
    ];

    // Test with very small chunks that will split the closing "^^^"
    for chunk_size in [1, 2, 3] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();
        assert_eq!(expected_fragments, fragments);
    }
}

#[test]
fn test_caret_array_syntax_proper_formatting() {
    // Test that arrays must have proper formatting with ] on its own line
    let input = "^^^read_files\nproject: test\npaths: [\nsrc/main.rs\nCargo.toml\n]\n^^^";

    let expected_fragments = vec![
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
            name: "paths".to_string(),
            value: "[\"src/main.rs\",\"Cargo.toml\"]".to_string(),
            tool_id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "tool-42-1".to_string(),
        },
    ];

    let test_ui = process_chunked_text(input, 3);
    let fragments = test_ui.get_fragments();

    assert_eq!(expected_fragments, fragments);
}

#[test]
fn test_caret_array_syntax_chunked() {
    // Test array syntax when chunked at various positions
    let input = "^^^read_files\nproject: test\npaths: [\nsrc/main.rs\nCargo.toml\n]\n^^^";

    let expected_fragments = vec![
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
            name: "paths".to_string(),
            value: "[\"src/main.rs\",\"Cargo.toml\"]".to_string(),
            tool_id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "tool-42-1".to_string(),
        },
    ];

    // Test with various chunk sizes to test chunking across array boundaries
    for chunk_size in [1, 2, 4, 8] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();
        assert_eq!(expected_fragments, fragments);
    }
}

#[test]
fn test_caret_multiline_parameter_chunked() {
    // Test multiline parameters when chunked across the --- markers
    let input = "^^^write_file\nproject: test\ncontent ---\nHello\nWorld\n--- content\n^^^";

    let expected_fragments = vec![
        DisplayFragment::ToolName {
            name: "write_file".to_string(),
            id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "project".to_string(),
            value: "test".to_string(),
            tool_id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolParameter {
            name: "content".to_string(),
            value: "Hello\nWorld".to_string(),
            tool_id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "tool-42-1".to_string(),
        },
    ];

    // Test with chunks that will split across the --- markers
    for chunk_size in [1, 3, 5, 7] {
        let test_ui = process_chunked_text(input, chunk_size);
        let fragments = test_ui.get_fragments();
        assert_eq!(expected_fragments, fragments);
    }
}

#[test]
fn test_caret_false_positive_prevention() {
    // Test that text containing ^^^ patterns that aren't valid tools is handled correctly
    let input = "Here's some code:\n```\n^^^not-a-tool\n```\nAnd here's a real tool:\n^^^list_projects\n^^^";

    let expected_fragments = vec![
        DisplayFragment::PlainText(
            "Here's some code:\n```\n^^^not-a-tool\n```\nAnd here's a real tool:\n".to_string(),
        ),
        DisplayFragment::ToolName {
            name: "list_projects".to_string(),
            id: "tool-42-1".to_string(),
        },
        DisplayFragment::ToolEnd {
            id: "tool-42-1".to_string(),
        },
    ];

    let test_ui = process_chunked_text(input, 4);
    let fragments = test_ui.get_fragments();

    assert_eq!(expected_fragments, fragments);
}

#[test]
fn test_caret_incomplete_tool_at_buffer_end() {
    // Test that incomplete tool syntax at the end of a buffer is not processed prematurely
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Send partial tool syntax
    processor
        .process(&StreamingChunk::Text("^^^list_proj".to_string()))
        .unwrap();

    // At this point, no tool should be emitted yet (it's incomplete)
    let fragments = test_ui.get_fragments();
    assert!(
        fragments.is_empty(),
        "No fragments should be emitted for incomplete tool"
    );

    // Complete the tool
    processor
        .process(&StreamingChunk::Text("ects\n^^^".to_string()))
        .unwrap();
    // Note: finalize_buffer() was removed - the processor should handle incomplete content gracefully

    // Now the tool should be complete
    let fragments = test_ui.get_fragments();
    assert!(fragments.len() >= 2, "Should have tool name and tool end");

    let tool_name = fragments
        .iter()
        .find(|f| matches!(f, DisplayFragment::ToolName { name, .. } if name == "list_projects"));
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
    let raw_text_count = raw_fragments
        .iter()
        .filter(|f| matches!(f, DisplayFragment::PlainText(_)))
        .count();
    let merged_text_count = merged_fragments
        .iter()
        .filter(|f| matches!(f, DisplayFragment::PlainText(_)))
        .count();

    // Due to chunking, we should have more raw text fragments than merged ones
    assert!(
        raw_text_count >= merged_text_count,
        "Raw fragments ({raw_text_count}) should have at least as many text fragments as merged ({merged_text_count})"
    );

    // The merged result should still be correct
    let expected_fragments = vec![
        DisplayFragment::PlainText("Let me\n help you\n\n".to_string()),
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
    ];

    assert_eq!(expected_fragments, merged_fragments);
}

/// Test that verifies the core streaming requirement:
/// Content should be emitted as it comes, not buffered in complete lines
#[test]
fn test_streaming_vs_buffering_behavior() {
    println!("\n=== Testing Streaming vs Buffering Behavior ===");

    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Send regular text that cannot be tool syntax
    // This should be emitted immediately, not buffered until complete line
    processor
        .process(&StreamingChunk::Text("Hello ".to_string()))
        .unwrap();

    let fragments_after_hello = test_ui.get_raw_fragments();
    println!("After 'Hello ': {fragments_after_hello:?}");

    // Key assertion: text that cannot be tool syntax should be emitted immediately
    assert!(
        !fragments_after_hello.is_empty(),
        "Regular text should be emitted immediately, not buffered until complete line"
    );
    assert!(
        matches!(fragments_after_hello[0], DisplayFragment::PlainText(ref text) if text == "Hello ")
    );

    // Send more text
    processor
        .process(&StreamingChunk::Text("world\n".to_string()))
        .unwrap();

    let fragments_after_world = test_ui.get_raw_fragments();
    println!("After 'world\\n': {fragments_after_world:?}");

    // Should have additional content
    assert!(fragments_after_world.len() >= 2);

    // Now test buffering behavior with potential tool syntax
    processor
        .process(&StreamingChunk::Text("^".to_string()))
        .unwrap();

    let fragments_after_caret = test_ui.get_raw_fragments();
    println!("After '^': {fragments_after_caret:?}");

    // The single caret should be buffered (not emitted) since it could be start of tool syntax
    // We should have the same number of fragments as before the caret
    let fragments_count_before_caret = fragments_after_world.len();
    assert_eq!(
        fragments_after_caret.len(),
        fragments_count_before_caret,
        "Single caret should be buffered, not emitted immediately"
    );

    // Send more carets to complete potential tool syntax
    processor
        .process(&StreamingChunk::Text("^^list".to_string()))
        .unwrap();

    let fragments_after_tool_start = test_ui.get_raw_fragments();
    println!("After '^^list': {fragments_after_tool_start:?}");

    // Still building tool name, should still be buffered
    assert_eq!(
        fragments_after_tool_start.len(),
        fragments_count_before_caret,
        "Incomplete tool name should still be buffered"
    );

    // Complete the tool line
    processor
        .process(&StreamingChunk::Text("_projects\n".to_string()))
        .unwrap();

    let fragments_after_complete_tool = test_ui.get_raw_fragments();
    println!("After complete tool line: {fragments_after_complete_tool:?}");

    // Now should have emitted the tool name
    assert!(
        fragments_after_complete_tool.len() > fragments_count_before_caret,
        "Complete tool syntax should be processed and emitted"
    );

    // Should have a ToolName fragment
    let has_tool_name = fragments_after_complete_tool
        .iter()
        .any(|f| matches!(f, DisplayFragment::ToolName { name, .. } if name == "list_projects"));
    assert!(has_tool_name, "Should have emitted ToolName fragment");

    println!("✅ Streaming vs buffering behavior test passed!");
}

/// Test that demonstrates newline buffering for boundary trimming
/// This verifies the elegant solution of buffering standalone newlines
#[test]
fn test_newline_boundary_trimming() {
    println!("\n=== Testing Newline Boundary Trimming ===");

    // Test case 1: Newline before tool block (should be trimmed in large chunks)
    let input1 = "Some text\n^^^list_projects\n^^^";

    // Large chunk - newline should be naturally trimmed
    let test_ui_large = process_chunked_text(input1, input1.len());
    let fragments_large = test_ui_large.get_fragments();
    println!("Large chunk trimming: {fragments_large:?}");

    // Should not have a separate newline fragment between text and tool
    let has_standalone_newline = fragments_large
        .iter()
        .any(|f| matches!(f, DisplayFragment::PlainText(text) if text == "\n"));
    assert!(
        !has_standalone_newline,
        "Large chunks should trim newlines at boundaries"
    );

    // Test case 2: Small chunks should show the newline because it arrives separately
    let test_ui_small = process_chunked_text(input1, 1);
    let fragments_small = test_ui_small.get_fragments();
    println!("Small chunk behavior: {fragments_small:?}");

    // With small chunks, we might see the newline processed separately, which is correct streaming behavior

    // Test case 3: Trailing newline after tool (should be trimmed when buffered)
    let input2 = "^^^list_projects\n^^^\n";

    let test_ui_trailing = process_chunked_text(input2, input2.len());
    let fragments_trailing = test_ui_trailing.get_fragments();
    println!("Trailing newline (large chunk): {fragments_trailing:?}");

    // Should not have trailing newline fragment
    let ends_with_newline = matches!(fragments_trailing.last(),
                                   Some(DisplayFragment::PlainText(text)) if text == "\n");
    assert!(
        !ends_with_newline,
        "Trailing newlines should be buffered and trimmed"
    );

    println!("✅ Newline boundary trimming test passed!");
}

#[test]
fn test_caret_tool_blocking_with_whitespace() {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Process a complete tool block followed by whitespace
    let input = "^^^read_files\nproject: test\n^^^\n\n  \t\n";

    let result = processor.process(&StreamingChunk::Text(input.to_string()));
    assert!(
        result.is_ok(),
        "Processing should succeed - whitespace is ignored silently"
    );

    let fragments = test_ui.get_fragments();

    // Should have exactly 3 fragments: ToolName, ToolParameter, ToolEnd
    // The whitespace after the tool block should be silently ignored
    assert_eq!(fragments.len(), 3);
    assert!(matches!(fragments[0], DisplayFragment::ToolName { .. }));
    assert!(matches!(
        fragments[1],
        DisplayFragment::ToolParameter { .. }
    ));
    assert!(matches!(fragments[2], DisplayFragment::ToolEnd { .. }));
}

#[test]
fn test_caret_tool_blocking_with_non_whitespace() {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Process a complete write tool block followed by non-whitespace text
    // Write tools should block further content according to SmartToolFilter
    let input = "^^^write_file\nproject: test\npath: test.txt\ncontent: hello\n^^^";
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
    assert!(
        error_msg.contains("no additional text after complete tool block allowed"),
        "Error should explain the blocking behavior"
    );

    let fragments = test_ui.get_fragments();

    // Should have fragments for the write_file tool: ToolName, 3 ToolParameters (project, path, content), ToolEnd
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

#[test]
fn test_smart_filter_allows_content_after_read_tools() {
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Process a complete read tool block followed by text
    // Read tools should allow content after them according to SmartToolFilter
    let input = "^^^read_files\nproject: test\n^^^";
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
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Process first read tool
    let input1 = "^^^read_files\nproject: test\n^^^";
    let result = processor.process(&StreamingChunk::Text(input1.to_string()));
    assert!(result.is_ok(), "First read tool should succeed");

    // Process text between tools
    let between_text = "Now let me list the files:";
    let result = processor.process(&StreamingChunk::Text(between_text.to_string()));
    assert!(result.is_ok(), "Text between read tools should be allowed");

    // Process second read tool
    let input2 = "^^^list_files\nproject: test\n^^^";
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
    let mut processor = CaretStreamProcessor::new(ui_arc, 42);

    // Process first read tool
    let input1 = "^^^read_files\nproject: test\n^^^";
    let result = processor.process(&StreamingChunk::Text(input1.to_string()));
    assert!(result.is_ok(), "First read tool should succeed");

    // Process text between tools
    let between_text = "Now let me write to a file:";
    let result = processor.process(&StreamingChunk::Text(between_text.to_string()));
    assert!(result.is_ok(), "Text between tools should be buffered");

    // Try to process a write tool - this should be blocked
    let write_tool = "^^^write_file\nproject: test\npath: output.txt\ncontent: test\n^^^";
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
