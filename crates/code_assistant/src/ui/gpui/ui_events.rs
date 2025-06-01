use crate::types::WorkingMemory;
use crate::ui::gpui::elements::MessageRole;
use crate::ui::ToolStatus;

/// Events for UI updates from the agent thread
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Display a new message or append to an existing one
    DisplayMessage { content: String, role: MessageRole },
    /// Append to the last text block
    AppendToTextBlock { content: String },
    /// Append to the last thinking block
    AppendToThinkingBlock { content: String },
    /// Start a tool invocation
    StartTool { name: String, id: String },
    /// Add or update a tool parameter
    UpdateToolParameter {
        tool_id: String,
        name: String,
        value: String,
    },
    /// Update a tool status
    UpdateToolStatus {
        tool_id: String,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
    },
    /// End a tool invocation
    EndTool { id: String },
    /// Update the working memory view
    UpdateMemory { memory: WorkingMemory },
    /// Streaming started for a request
    StreamingStarted(u64),
    /// Streaming stopped for a request
    StreamingStopped { id: u64, cancelled: bool },
}
