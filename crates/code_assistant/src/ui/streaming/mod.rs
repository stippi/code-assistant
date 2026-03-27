//! Streaming processor for handling chunks from LLM providers

use crate::agent::ToolSyntax;
use crate::ui::UIError;
use crate::ui::UserInterface;
use llm::{Message, StreamingChunk};
use std::sync::Arc;

mod caret_processor;
mod json_processor;
mod xml_processor;

#[cfg(test)]
mod caret_processor_tests;
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
    /// `duration_seconds` is set during session restore from persisted ContentBlock timestamps.
    ThinkingText {
        text: String,
        duration_seconds: Option<f64>,
    },
    /// Image content
    Image { media_type: String, data: String },
    /// Tool invocation start.
    /// `duration_seconds` is set during session restore from the corresponding ToolUse ContentBlock.
    ToolName {
        name: String,
        id: String,
        duration_seconds: Option<f64>,
    },
    /// Parameter for a tool
    ToolParameter {
        name: String,
        value: String,
        tool_id: String,
    },
    /// End of a tool invocation
    ToolEnd { id: String },
    /// Streaming tool output chunk
    ToolOutput { tool_id: String, chunk: String },
    /// Tool attached a terminal on the client side
    ToolTerminal {
        tool_id: String,
        terminal_id: String,
    },
    /// OpenAI reasoning summary started a new item
    ReasoningSummaryStart,
    /// OpenAI reasoning summary delta for the current item
    ReasoningSummaryDelta(String),
    /// Mark reasoning as completed
    ReasoningComplete,
    /// Divider indicating the conversation was compacted, with expandable summary text
    CompactionDivider { summary: String },
    /// A hidden tool completed - UI may need to insert paragraph break if next fragment is same type
    HiddenToolCompleted,
}

impl DisplayFragment {
    /// Create a ThinkingText fragment without duration (used during live streaming)
    pub fn thinking_text(text: impl Into<String>) -> Self {
        DisplayFragment::ThinkingText {
            text: text.into(),
            duration_seconds: None,
        }
    }

    /// Create a ToolName fragment without duration (used during live streaming)
    pub fn tool_name(name: impl Into<String>, id: impl Into<String>) -> Self {
        DisplayFragment::ToolName {
            name: name.into(),
            id: id.into(),
            duration_seconds: None,
        }
    }
}

/// Common trait for stream processors
pub trait StreamProcessorTrait: Send + Sync {
    /// Create a new stream processor with the given UI and request context
    fn new(ui: Arc<dyn UserInterface>, request_id: u64) -> Self
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
pub use caret_processor::CaretStreamProcessor;
pub use json_processor::JsonStreamProcessor;
pub use xml_processor::XmlStreamProcessor;

/// Factory function to create the appropriate processor based on tool syntax
pub fn create_stream_processor(
    tool_syntax: ToolSyntax,
    ui: Arc<dyn UserInterface>,
    request_id: u64,
) -> Box<dyn StreamProcessorTrait> {
    match tool_syntax {
        ToolSyntax::Xml => Box::new(XmlStreamProcessor::new(ui, request_id)),
        ToolSyntax::Native => Box::new(JsonStreamProcessor::new(ui, request_id)),
        ToolSyntax::Caret => Box::new(CaretStreamProcessor::new(ui, request_id)),
    }
}
