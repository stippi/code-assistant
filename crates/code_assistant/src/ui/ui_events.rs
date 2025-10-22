use crate::persistence::{ChatMetadata, DraftAttachment};
use crate::session::instance::SessionActivityState;
use crate::types::{PlanState, WorkingMemory};
use crate::ui::gpui::elements::MessageRole;
use crate::ui::{DisplayFragment, ToolStatus};

/// Data for a complete message with its display fragments
#[derive(Debug, Clone)]
pub struct MessageData {
    pub role: MessageRole,
    pub fragments: Vec<DisplayFragment>,
}

/// Tool execution result data for UI updates
#[derive(Debug, Clone)]
pub struct ToolResultData {
    pub tool_id: String,
    pub status: ToolStatus,
    pub message: Option<String>,
    pub output: Option<String>,
}

/// Events for UI updates from the agent thread
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Display user input message with optional attachments
    DisplayUserInput {
        content: String,
        attachments: Vec<DraftAttachment>,
    },
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
    /// Add an image to the message
    AddImage { media_type: String, data: String },
    /// Append streaming tool output
    AppendToolOutput { tool_id: String, chunk: String },
    /// Update the working memory view
    UpdateMemory { memory: WorkingMemory },
    /// Update the session plan display
    UpdatePlan { plan: PlanState },
    /// Set all messages at once (for session loading, clears existing)
    SetMessages {
        messages: Vec<MessageData>,
        session_id: Option<String>,
        tool_results: Vec<ToolResultData>,
    },
    /// Streaming started for a request
    StreamingStarted(u64),
    /// Streaming stopped for a request
    StreamingStopped {
        id: u64,
        cancelled: bool,
        error: Option<String>,
    },
    /// Refresh the chat list from session manager
    RefreshChatList,
    /// Update the chat list display
    UpdateChatList { sessions: Vec<ChatMetadata> },
    /// Clear all messages
    #[allow(dead_code)]
    ClearMessages,
    /// Send user message with optional attachments to active session (triggers agent)
    SendUserMessage {
        message: String,
        session_id: String,
        attachments: Vec<DraftAttachment>,
    },
    /// Update metadata for a single session without refreshing the entire list
    UpdateSessionMetadata { metadata: ChatMetadata },
    /// Update activity state for a single session
    UpdateSessionActivityState {
        session_id: String,
        activity_state: SessionActivityState,
    },
    /// Queue a user message with optional attachments while agent is running
    QueueUserMessage {
        message: String,
        session_id: String,
        attachments: Vec<DraftAttachment>,
    },
    /// Request to edit pending message (move back to input)
    #[allow(dead_code)]
    RequestPendingMessageEdit { session_id: String },
    /// Update pending message display
    UpdatePendingMessage { message: Option<String> },
    /// Display an error message to the user
    DisplayError { message: String },
    /// Clear the current error display
    ClearError,
    /// Start a new reasoning summary item
    StartReasoningSummaryItem,
    /// Append delta content to the current reasoning summary item
    AppendReasoningSummaryDelta { delta: String },
    /// Complete reasoning block
    CompleteReasoning,
    /// Update the current model selection in the UI
    UpdateCurrentModel { model_name: String },
}
