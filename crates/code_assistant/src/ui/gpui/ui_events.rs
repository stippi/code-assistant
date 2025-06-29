use crate::persistence::ChatMetadata;
use crate::types::WorkingMemory;
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
    /// Set all messages at once (for session loading, clears existing)
    SetMessages {
        messages: Vec<MessageData>,
        session_id: Option<String>,
        tool_results: Vec<ToolResultData>,
    },
    /// Streaming started for a request
    StreamingStarted(u64),
    /// Streaming stopped for a request
    StreamingStopped { id: u64, cancelled: bool },
    /// Chat session management events
    LoadChatSession { session_id: String },
    /// Create a new chat session
    CreateNewChatSession { name: Option<String> },
    /// Delete a chat session
    DeleteChatSession { session_id: String },
    /// Refresh the chat list from session manager
    RefreshChatList,
    /// Update the chat list display
    UpdateChatList { sessions: Vec<ChatMetadata> },
    /// Clear all messages
    #[allow(dead_code)]
    ClearMessages,
    /// Send user message to active session (triggers agent)
    SendUserMessage { message: String, session_id: String },
    /// Notify about rate limiting with countdown
    RateLimitNotification { seconds_remaining: u64 },
    /// Clear rate limit notification
    ClearRateLimit,
    /// Update metadata for a single session without refreshing the entire list
    UpdateSessionMetadata { metadata: ChatMetadata },
}
