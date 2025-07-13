//! Streaming processor for handling chunks from LLM providers

use crate::agent::ToolSyntax;
use crate::ui::UIError;
use crate::ui::UserInterface;
use llm::{Message, StreamingChunk};
use std::sync::Arc;

mod json_processor;
mod xml_processor;

#[cfg(test)]
mod json_processor_tests;
#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod xml_processor_tests;

/// Fragments for display in UI components
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayFragment {
    /// Regular plain text
    PlainText(String),
    /// Thinking text (shown differently)
    ThinkingText(String),
    /// Tool invocation start
    ToolName { name: String, id: String },
    /// Parameter for a tool
    ToolParameter {
        name: String,
        value: String,
        tool_id: String,
    },
    /// End of a tool invocation
    ToolEnd { id: String },
}

/// Common trait for stream processors
pub trait StreamProcessorTrait: Send + Sync {
    /// Create a new stream processor with the given UI and request context
    fn new(ui: Arc<Box<dyn UserInterface>>, request_id: u64) -> Self
    where
        Self: Sized;

    /// Process a streaming chunk and send display fragments to the UI
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError>;

    /// Extract display fragments from a stored message without sending to UI
    /// Used for session loading to reconstruct fragment sequences
    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError>;
}

// Export the concrete implementations
pub use json_processor::JsonStreamProcessor;
pub use xml_processor::XmlStreamProcessor;

/// Factory function to create the appropriate processor based on tool syntax
pub fn create_stream_processor(
    tool_syntax: ToolSyntax,
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
) -> Box<dyn StreamProcessorTrait> {
    match tool_syntax {
        ToolSyntax::Xml => Box::new(XmlStreamProcessor::new(ui, request_id)),
        ToolSyntax::Native => Box::new(JsonStreamProcessor::new(ui, request_id)),
    }
}
