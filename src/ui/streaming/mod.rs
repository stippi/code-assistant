//! Streaming processor for handling chunks from LLM providers

use std::sync::Arc;
use crate::ui::UserInterface;
use crate::ui::UIError;
use crate::llm::StreamingChunk;
use crate::agent::ToolMode;

mod xml_processor;

// Re-export the display fragments and other types needed by the UI
pub use xml_processor::DisplayFragment;

/// Common trait for stream processors
pub trait StreamProcessorTrait: Send + Sync {
    /// Create a new stream processor with the given UI
    fn new(ui: Arc<Box<dyn UserInterface>>) -> Self where Self: Sized;
    
    /// Process a streaming chunk and send display fragments to the UI
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError>;
}

// Export the concrete implementation
pub use xml_processor::XmlStreamProcessor;

/// Factory function to create the appropriate processor based on tool mode
pub fn create_stream_processor(
    tool_mode: ToolMode, 
    ui: Arc<Box<dyn UserInterface>>
) -> Box<dyn StreamProcessorTrait> {
    match tool_mode {
        ToolMode::Xml => Box::new(XmlStreamProcessor::new(ui)),
        ToolMode::Native => Box::new(XmlStreamProcessor::new(ui)), // Temporarily use XML for both
    }
}

