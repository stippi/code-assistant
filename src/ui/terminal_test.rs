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
    let input = "<thinking>The user has not provided a task.</thinking>\nI'll use the ask_user tool.\n<tool:ask_user>\n<param:question>What would you like to know?</param:question>\n</tool:ask_user>";

    // Create shared test writer to capture output
    let writer = Arc::new(Mutex::new(TestWriter::new()));

    // Run the code that uses the test writer
    stream_text(writer.clone(), chunk_str(input, 3));

    // Check the output
    let output = writer.lock().unwrap().get_output();
    println!("{}", output);

    // Parameter content should be visible
    assert!(
        output.contains("What would you like to know?"),
        "Parameter content should be visible"
    );

    // Parameter end tags should not be visible
    assert!(
        !output.contains("</param:question>"),
        "Parameter end tag should not be visible"
    );
}

fn chunk_str(s: &str, chunk_size: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut chunks = Vec::new();

    for chunk in chars.chunks(chunk_size) {
        chunks.push(chunk.iter().collect::<String>());
    }

    chunks
}

// Helper function to run a chunk of text through our UI
fn stream_text(writer: Arc<Mutex<TestWriter>>, chunks: Vec<String>) {
    // Erstelle eine minimale Struktur, die Write implementiert und an unseren Arc<Mutex<TestWriter>> weiterleitet
    struct WriterWrapper(Arc<Mutex<TestWriter>>);

    impl Write for WriterWrapper {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    // Erstelle unseren Wrapper
    let wrapper = Box::new(WriterWrapper(writer));

    // Erstelle UI mit Test-Writer
    let ui = TerminalUI::with_test_writer(wrapper);

    // Streame Text-Chunks
    for chunk in chunks {
        ui.display_streaming(&chunk).unwrap();
    }
}
