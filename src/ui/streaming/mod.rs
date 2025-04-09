//! Streaming processor for handling chunks from LLM providers

use crate::agent::ToolMode;
use crate::llm::StreamingChunk;
use crate::ui::UIError;
use crate::ui::UserInterface;
use std::sync::Arc;

mod json_processor;
mod xml_processor;

#[cfg(test)]
mod json_processor_tests;
#[cfg(test)]
mod xml_processor_tests;

// Re-export the display fragments and other types needed by the UI
pub use xml_processor::DisplayFragment;

/// Common trait for stream processors
pub trait StreamProcessorTrait: Send + Sync {
    /// Create a new stream processor with the given UI
    fn new(ui: Arc<Box<dyn UserInterface>>) -> Self
    where
        Self: Sized;

    /// Process a streaming chunk and send display fragments to the UI
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError>;
}

// Export the concrete implementations
pub use json_processor::JsonStreamProcessor;
pub use xml_processor::XmlStreamProcessor;

/// Factory function to create the appropriate processor based on tool mode
pub fn create_stream_processor(
    tool_mode: ToolMode,
    ui: Arc<Box<dyn UserInterface>>,
) -> Box<dyn StreamProcessorTrait> {
    match tool_mode {
        ToolMode::Xml => Box::new(XmlStreamProcessor::new(ui)),
        ToolMode::Native => Box::new(JsonStreamProcessor::new(ui)),
    }
}
