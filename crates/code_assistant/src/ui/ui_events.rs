use crate::persistence::{BranchInfo, ChatMetadata, DraftAttachment, NodeId};
use crate::session::instance::SessionActivityState;
use crate::tools::core::ImageData;
use crate::types::PlanState;
use crate::ui::gpui::elements::MessageRole;
use crate::ui::{DisplayFragment, ToolStatus};
use sandbox::SandboxPolicy;
use std::path::PathBuf;

/// Data for a complete message with its display fragments
#[derive(Debug, Clone)]
pub struct MessageData {
    pub role: MessageRole,
    pub fragments: Vec<DisplayFragment>,
    /// Optional node ID for branching support
    pub node_id: Option<NodeId>,
    /// Optional branch info if this message is part of a branch
    pub branch_info: Option<BranchInfo>,
}

/// Tool execution result data for UI updates
#[derive(Debug, Clone)]
pub struct ToolResultData {
    pub tool_id: String,
    pub status: ToolStatus,
    pub message: Option<String>,
    pub output: Option<String>,
    /// Styled terminal output with ANSI color information preserved.
    pub styled_output: Option<Vec<terminal::StyledLine>>,
    /// Duration of the tool execution in seconds, computed from persisted ContentBlock timestamps.
    pub duration_seconds: Option<f64>,
    /// Image data from tools that produce visual output (e.g. view_images).
    pub images: Vec<ImageData>,
}

/// Events for UI updates from the agent thread
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Display user input message with optional attachments
    DisplayUserInput {
        content: String,
        attachments: Vec<DraftAttachment>,
        /// Node ID for this message (for edit button support)
        node_id: Option<NodeId>,
    },
    /// Display a system-generated compaction divider message
    DisplayCompactionSummary { summary: String },
    /// Append to the last text block
    AppendToTextBlock { content: String },
    /// Append to the last thinking block
    AppendToThinkingBlock { content: String },
    /// Start a tool invocation
    StartTool { name: String, id: String },
    /// Add or update a tool parameter.
    ///
    /// When `replace` is `false` (the default for streaming), the value is
    /// **appended** to any existing parameter with the same name.
    /// When `replace` is `true` (used by post-execution format-on-save updates),
    /// the value **replaces** the existing parameter value entirely.
    UpdateToolParameter {
        tool_id: String,
        name: String,
        value: String,
        /// If true, replace the parameter value instead of appending.
        replace: bool,
    },

    /// Update a tool status
    UpdateToolStatus {
        tool_id: String,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
        /// Styled terminal output with ANSI color information preserved.
        styled_output: Option<Vec<terminal::StyledLine>>,
        /// Execution duration in seconds, set from ContentBlock timestamps on completion.
        duration_seconds: Option<f64>,
        /// Image data from tools that produce visual output (e.g. view_images).
        #[allow(dead_code)]
        images: Vec<ImageData>,
    },

    /// End a tool invocation
    EndTool { id: String },
    /// A hidden tool completed - UI may need paragraph break before next text
    HiddenToolCompleted,
    /// Add an image to the message
    AddImage { media_type: String, data: String },
    /// Append streaming tool output
    AppendToolOutput { tool_id: String, chunk: String },
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
    /// Rollback all UI content produced by a failed streaming request.
    /// Sent before a retry so that UIs can discard the partial output.
    RollbackStreaming { id: u64 },
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
        /// If set, creates a new branch from this parent node instead of appending to active path
        branch_parent_id: Option<NodeId>,
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
    /// Show a brief, auto-dismissing status notification (e.g. "Stream interrupted — retrying")
    ShowTransientStatus { message: String },
    /// Clear the transient status notification (sent by the auto-dismiss timer)
    ClearTransientStatus,
    /// Start a new reasoning summary item
    StartReasoningSummaryItem,
    /// Append delta content to the current reasoning summary item
    AppendReasoningSummaryDelta { delta: String },
    /// Complete reasoning block
    CompleteReasoning,
    /// Update the current model selection in the UI
    UpdateCurrentModel { model_name: String },
    /// Update the current sandbox selection in the UI
    UpdateSandboxPolicy { policy: SandboxPolicy },

    /// Cancel a running sub-agent by its tool id
    CancelSubAgent { tool_id: String },

    /// Schedule a debounced save of the per-session UI state file.
    /// Sent after any mutation to the UI state (tool collapse toggle, plan
    /// toggle, etc.).  The handler cancels any pending save timer and starts
    /// a new one.
    PersistUiState,

    // === Session Branching Events ===
    /// Request to start editing a message (creates a branch point)
    /// UI should load the message content into the input area
    StartMessageEdit {
        session_id: String,
        /// The node ID of the message being edited
        node_id: NodeId,
    },

    /// Switch to a different branch at a branch point
    SwitchBranch {
        session_id: String,
        /// The node ID to switch to (a sibling of the current node at a branch point)
        new_node_id: NodeId,
    },

    /// Response: Message content loaded for editing
    /// Sent in response to StartMessageEdit
    MessageEditReady {
        /// The text content of the message
        content: String,
        /// Any attachments from the original message
        attachments: Vec<DraftAttachment>,
        /// The parent node ID where the new branch will be created
        branch_parent_id: Option<NodeId>,
        /// Messages up to (but not including) the message being edited
        messages: Vec<MessageData>,
        /// Tool results for the truncated path
        tool_results: Vec<ToolResultData>,
    },

    /// Response: Branch switch completed, new messages to display
    BranchSwitched {
        session_id: String,
        /// Full message list for the new active path
        messages: Vec<MessageData>,
        /// Tool results for the new path
        tool_results: Vec<ToolResultData>,
        /// Updated plan for the new path
        plan: PlanState,
    },

    /// Update the branch info for a specific message node
    /// Used when a new branch is created to update the UI for the parent message
    UpdateBranchInfo {
        /// The node ID whose branch info should be updated
        node_id: NodeId,
        /// The updated branch info (siblings at this branch point)
        branch_info: BranchInfo,
    },

    // === Cross-instance awareness ===
    /// Another process modified the session file on disk for the currently
    /// viewed session.  The UI should reload messages from persistence.
    RefreshCurrentSession { session_id: String },

    // === Resource Events (for tool operations) ===
    /// A file was loaded/read by a tool
    ResourceLoaded { project: String, path: PathBuf },
    /// A file was written/modified by a tool
    ResourceWritten { project: String, path: PathBuf },
    /// A directory was listed by a tool
    DirectoryListed { project: String, path: PathBuf },
    /// A file was deleted by a tool
    ResourceDeleted { project: String, path: PathBuf },

    // === Git Worktree Events ===
    /// Updated worktree/branch listing from the backend
    UpdateWorktreeData {
        worktrees: Vec<git::Worktree>,
        current_worktree_path: Option<PathBuf>,
        is_git_repo: bool,
    },
}
