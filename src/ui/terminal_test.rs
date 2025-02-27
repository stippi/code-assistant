//! Tests for the terminal UI streaming functionality

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

// Test param tag handling
#[test]
fn test_param_tag_hiding() {
    let input = "<tool:ask_user>\n<param:question>What would you like to know?</param:question>\n</tool:ask_user>";
    
    // Create shared test writer to capture output
    let writer = Arc::new(Mutex::new(TestWriter::new()));
    
    // Run the code that uses the test writer
    stream_text(writer.clone(), &[input]);
    
    // Check the output
    let output = writer.lock().unwrap().get_output();
    
    // Parameter content should be visible
    assert!(output.contains("What would you like to know?"), "Parameter content should be visible");
    
    // Parameter end tags should not be visible
    assert!(!output.contains("</param:question>"), "Parameter end tag should not be visible");
}

// Helper function to run a chunk of text through our UI
fn stream_text(writer: Arc<Mutex<TestWriter>>, chunks: &[&str]) {
    // Create a minimal struct that implements Write and forwards to our Arc<Mutex<TestWriter>>
    struct WriterWrapper(Arc<Mutex<TestWriter>>);
    
    impl Write for WriterWrapper {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }
    
    // Create our wrapper
    let wrapper = Box::new(WriterWrapper(writer));
    
    // Create UI with test writer
    let ui = TerminalUI::with_test_writer(wrapper);
    
    // Stream text chunks
    for chunk in chunks {
        ui.display_streaming(chunk).unwrap();
    }
}
