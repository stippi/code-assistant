//! Tests for the terminal UI formatting and output

use super::streaming::DisplayFragment;
use super::terminal::TerminalUI;
use super::UserInterface;
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
    ui.display_fragment(&DisplayFragment::PlainText("Hello world".to_string())).unwrap();
    ui.display_fragment(&DisplayFragment::ThinkingText("Thinking...".to_string())).unwrap();
    
    // The newline is needed because the tool name formatting starts with a newline
    ui.display_fragment(&DisplayFragment::PlainText("\n".to_string())).unwrap();
    
    ui.display_fragment(&DisplayFragment::ToolName { 
        name: "search_files".to_string(), 
        id: "tool-123".to_string() 
    }).unwrap();
    ui.display_fragment(&DisplayFragment::ToolParameter { 
        name: "query".to_string(), 
        value: "search term".to_string(),
        tool_id: "tool-123".to_string()
    }).unwrap();
    
    // Check the output
    let output = writer.lock().unwrap().get_output();
    println!("Output:\n{}", output);
    
    // Verify various formatting aspects
    assert!(output.contains("Hello world"), "Plain text should be displayed as-is");
    
    // Thinking text should be styled (usually italic and grey, but we can't easily check styling in tests)
    assert!(output.contains("Thinking..."), "Thinking text should be visible");
    
    // We can check if the bullet point appears
    assert!(output.contains("•"), "Bullet point should be visible");
    
    // Verify tool name appears
    assert!(output.contains("search_files"), "Tool name should be visible");
    
    // Parameter should be formatted with indentation and name
    // In der Ausgabe mit ANSI-Farbcodes ist es schwer, genau nach "query:" zu suchen
    // Wir prüfen stattdessen, ob der Wert vorhanden ist
    assert!(output.contains("search term"), "Parameter value should be visible");
}
