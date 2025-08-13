//! Tests for the terminal UI formatting and output

use super::streaming::DisplayFragment;
use super::terminal::TerminalUI;
use super::{UIError, UiEvent, UserInterface};
use std::io::Write;
use std::sync::{Arc, Mutex};

// Mock stdout to capture output
struct TestWriter {
    buffer: Vec<u8>,
}

impl TestWriter {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn get_output(&self) -> String {
        String::from_utf8_lossy(&self.buffer).to_string()
    }
}

impl Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Helper function to create a terminal UI with a test writer
fn create_test_terminal_ui() -> (TerminalUI, Arc<Mutex<TestWriter>>) {
    let writer = Arc::new(Mutex::new(TestWriter::new()));

    // Create a wrapper to satisfy the trait bounds
    struct WriterWrapper(Arc<Mutex<TestWriter>>);

    impl Write for WriterWrapper {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    let wrapper = Box::new(WriterWrapper(writer.clone()));
    let ui = TerminalUI::with_test_writer(wrapper);

    (ui, writer)
}

#[test]
fn test_terminal_formatting() {
    // Create terminal UI with test writer
    let (ui, writer) = create_test_terminal_ui();

    // Test various display fragments
    ui.display_fragment(&DisplayFragment::PlainText("Hello world".to_string()))
        .unwrap();
    ui.display_fragment(&DisplayFragment::ThinkingText("Thinking...".to_string()))
        .unwrap();

    // The newline is needed because the tool name formatting starts with a newline
    ui.display_fragment(&DisplayFragment::PlainText("\n".to_string()))
        .unwrap();

    ui.display_fragment(&DisplayFragment::ToolName {
        name: "search_files".to_string(),
        id: "tool-123".to_string(),
    })
    .unwrap();
    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "search term".to_string(),
        tool_id: "tool-123".to_string(),
    })
    .unwrap();

    // Check the output
    let output = writer.lock().unwrap().get_output();
    println!("Output:\n{output}");

    // Verify various formatting aspects
    assert!(
        output.contains("Hello world"),
        "Plain text should be displayed as-is"
    );

    // Thinking text should be styled (usually italic and grey, but we can't easily check styling in tests)
    assert!(
        output.contains("Thinking..."),
        "Thinking text should be visible"
    );

    // We can check if the bullet point appears
    assert!(output.contains("•"), "Bullet point should be visible");

    // Verify tool name appears
    assert!(
        output.contains("search_files"),
        "Tool name should be visible"
    );

    // Parameter should be formatted with indentation and name
    // In der Ausgabe mit ANSI-Farbcodes ist es schwer, genau nach "query:" zu suchen
    // Wir prüfen stattdessen, ob der Wert vorhanden ist
    assert!(
        output.contains("search term"),
        "Parameter value should be visible"
    );
}

#[test]
fn test_streaming_parameter_rendering() {
    // Create terminal UI with test writer
    let (ui, writer) = create_test_terminal_ui();

    // Test streaming parameter rendering - parameter name should only appear once
    ui.display_fragment(&DisplayFragment::ToolName {
        name: "search_files".to_string(),
        id: "tool-456".to_string(),
    })
    .unwrap();

    // Simulate streaming parameter chunks - name should only appear once
    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "search ".to_string(),
        tool_id: "tool-456".to_string(),
    })
    .unwrap();

    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "for ".to_string(),
        tool_id: "tool-456".to_string(),
    })
    .unwrap();

    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "rust ".to_string(),
        tool_id: "tool-456".to_string(),
    })
    .unwrap();

    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "files".to_string(),
        tool_id: "tool-456".to_string(),
    })
    .unwrap();

    // Add a second parameter to test multiple parameters
    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "paths".to_string(),
        value: "src/".to_string(),
        tool_id: "tool-456".to_string(),
    })
    .unwrap();

    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "paths".to_string(),
        value: "tests/".to_string(),
        tool_id: "tool-456".to_string(),
    })
    .unwrap();

    // Check the output
    let output = writer.lock().unwrap().get_output();
    println!("Streaming parameter output:\n{output}");

    // Verify the parameter name "query" appears only once (ignoring color codes)
    let query_count = output.matches("query").count();
    assert_eq!(
        query_count, 1,
        "Parameter name 'query' should appear exactly once, found {query_count} times"
    );

    // Verify the parameter name "paths" appears only once (ignoring color codes)
    let paths_count = output.matches("paths").count();
    assert_eq!(
        paths_count, 1,
        "Parameter name 'paths' should appear exactly once, found {paths_count} times"
    );

    // Verify the complete parameter value is assembled correctly
    assert!(
        output.contains("search for rust files"),
        "Complete parameter value should be assembled: 'search for rust files'"
    );

    // Verify both parameter values are present
    assert!(
        output.contains("src/tests/"),
        "Both path values should be present: 'src/tests/'"
    );
}

#[test]
fn test_multiple_tools_parameter_isolation() {
    // Create terminal UI with test writer
    let (ui, writer) = create_test_terminal_ui();

    // First tool
    ui.display_fragment(&DisplayFragment::ToolName {
        name: "search_files".to_string(),
        id: "tool-1".to_string(),
    })
    .unwrap();

    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "first tool value".to_string(),
        tool_id: "tool-1".to_string(),
    })
    .unwrap();

    // Second tool with same parameter name
    ui.display_fragment(&DisplayFragment::ToolName {
        name: "read_files".to_string(),
        id: "tool-2".to_string(),
    })
    .unwrap();

    ui.display_fragment(&DisplayFragment::ToolParameter {
        name: "query".to_string(),
        value: "second tool value".to_string(),
        tool_id: "tool-2".to_string(),
    })
    .unwrap();

    // Check the output
    let output = writer.lock().unwrap().get_output();
    println!("Multiple tools output:\n{output}");

    // Verify both parameter names appear (once for each tool)
    let query_count = output.matches("query").count();
    assert_eq!(
        query_count, 2,
        "Parameter name 'query' should appear twice (once per tool), found {query_count} times"
    );

    // Verify both parameter values are present
    assert!(
        output.contains("first tool value"),
        "First tool parameter value should be present"
    );
    assert!(
        output.contains("second tool value"),
        "Second tool parameter value should be present"
    );
}

#[test]
fn test_streaming_events_cleanup() {
    // Create terminal UI with test writer
    let (ui, writer) = create_test_terminal_ui();

    // Test that streaming started event is silent
    ui.send_event_sync(UiEvent::StreamingStarted(42)).unwrap();

    // Test that streaming stopped event (not cancelled) is mostly silent
    ui.send_event_sync(UiEvent::StreamingStopped {
        id: 42,
        cancelled: false,
    })
    .unwrap();

    // Check the output - should be minimal (only prompt)
    let output = writer.lock().unwrap().get_output();
    println!("Streaming events output:\n{output}");

    // Should NOT contain "Starting" or "Completed" messages
    assert!(
        !output.contains("Starting"),
        "Should not contain 'Starting' message"
    );
    assert!(
        !output.contains("Completed"),
        "Should not contain 'Completed' message"
    );

    // Should contain the prompt
    assert!(
        output.contains(">"),
        "Should contain prompt after completion"
    );
}

#[test]
fn test_spinner_functionality() {
    // Create terminal UI with test writer
    let (ui, writer) = create_test_terminal_ui();

    // Test streaming started (should start spinner - but not visible in test mode)
    ui.send_event_sync(UiEvent::StreamingStarted(42)).unwrap();

    // Simulate some content arriving (should stop spinner)
    ui.display_fragment(&DisplayFragment::PlainText("Hello".to_string()))
        .unwrap();

    // Check the output
    let output = writer.lock().unwrap().get_output();
    println!("Spinner test output:\n{output}");

    // Should contain the content
    assert!(
        output.contains("Hello"),
        "Should contain the displayed content"
    );

    // Should not contain spinner control sequences in test mode
    assert!(
        !output.contains("[K"),
        "Should not contain terminal control sequences in test mode"
    );
}

// Helper to send events synchronously for testing
impl TerminalUI {
    fn send_event_sync(&self, event: UiEvent) -> Result<(), UIError> {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.send_event(event))
    }
}
