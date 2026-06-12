//! Streaming processors for handling chunks from LLM providers.
//!
//! The display-fragment vocabulary, the processor trait, and the native
//! (JSON) processor live in the agent core; the XML and Caret processors are
//! application code, each living with its dialect in `tool_dialects`. This
//! module ties them together with a factory keyed on the tool syntax.

use crate::agent::ToolSyntax;
use crate::tools::core::ToolRegistry;
use crate::ui::UserInterface;
use std::sync::Arc;

#[cfg(test)]
mod json_processor_tests;
#[cfg(test)]
pub(crate) mod test_utils;

pub use agent_core::ui::{DisplayFragment, HiddenTools, StreamProcessorTrait};

// Export the concrete implementations
pub use crate::tool_dialects::caret::CaretStreamProcessor;
pub use crate::tool_dialects::xml::XmlStreamProcessor;
pub use agent_core::native::JsonStreamProcessor;

/// Factory function to create the appropriate processor based on tool syntax.
/// The given [`UserInterface`] is adapted to the core's UI boundary.
pub fn create_stream_processor(
    tool_syntax: ToolSyntax,
    ui: Arc<dyn UserInterface>,
    request_id: u64,
    hidden_tools: HiddenTools,
    registry: Arc<ToolRegistry>,
) -> Box<dyn StreamProcessorTrait> {
    let ui: Arc<dyn agent_core::AgentUi> = Arc::new(crate::ui::AgentUiAdapter::new(ui));
    match tool_syntax {
        ToolSyntax::Xml => Box::new(XmlStreamProcessor::new(
            ui,
            request_id,
            hidden_tools,
            registry,
        )),
        ToolSyntax::Native => Box::new(JsonStreamProcessor::new(ui, request_id, hidden_tools)),
        ToolSyntax::Caret => Box::new(CaretStreamProcessor::new(
            ui,
            request_id,
            hidden_tools,
            registry,
        )),
    }
}
