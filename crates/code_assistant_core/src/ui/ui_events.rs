use crate::persistence::{BranchInfo, ChatMetadata, DraftAttachment, NodeId};
use crate::session::instance::SessionActivityState;
use crate::tools::core::ImageData;
use crate::types::PlanState;
use crate::ui::{DisplayFragment, ToolStatus};
use sandbox::SandboxPolicy;
use std::path::PathBuf;

/// Role of a message in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    /// System-level messages (e.g. compaction dividers) that have no author header
    System,
}

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
    pub styled_output: Option<Vec<terminal_output::StyledLine>>,
    /// Duration of the tool execution in seconds, computed from persisted ContentBlock timestamps.
    pub duration_seconds: Option<f64>,
    /// Image data from tools that produce visual output (e.g. view_images).
    pub images: Vec<ImageData>,
}

impl UiEvent {
    /// Translate an agent-loop event into the application's event vocabulary.
    ///
    /// `session_id` scopes the events that address a specific session; events
    /// that need it are dropped (`None`) when it is absent, matching the
    /// loop's previous behavior of skipping those updates for anonymous
    /// agents (e.g. sub-agents).
    pub fn from_agent(
        event: agent_core::ui::AgentUiEvent,
        session_id: Option<&str>,
    ) -> Option<UiEvent> {
        use agent_core::ui::{AgentActivity, AgentUiEvent};

        Some(match event {
            AgentUiEvent::UserInputAppended { content, node_id } => UiEvent::DisplayUserInput {
                content,
                attachments: Vec::new(),
                node_id,
            },
            AgentUiEvent::StreamingStarted {
                request_id,
                node_id,
            } => UiEvent::StreamingStarted {
                request_id,
                node_id,
            },
            AgentUiEvent::StreamingStopped {
                request_id,
                cancelled,
                error,
            } => UiEvent::StreamingStopped {
                id: request_id,
                cancelled,
                error,
            },
            AgentUiEvent::RollbackStreaming { request_id } => {
                UiEvent::RollbackStreaming { id: request_id }
            }
            AgentUiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
                replace,
            } => UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
                replace,
            },
            AgentUiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                duration_seconds,
                images,
            } => UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                styled_output: None,
                duration_seconds,
                images,
            },
            AgentUiEvent::ShowTransientStatus { message } => {
                UiEvent::ShowTransientStatus { message }
            }
            AgentUiEvent::ActivityChanged { activity } => UiEvent::UpdateSessionActivityState {
                session_id: session_id?.to_string(),
                activity_state: match activity {
                    AgentActivity::Running => SessionActivityState::AgentRunning,
                    AgentActivity::WaitingForResponse => SessionActivityState::WaitingForResponse,
                },
            },
        })
    }
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
        styled_output: Option<Vec<terminal_output::StyledLine>>,
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
    /// A backend terminal was attached for a tool (`execute_command`),
    /// possibly before any output exists. Frontends with a terminal
    /// emulator create the tool card's display terminal on this signal so
    /// the card shows a running terminal (and its stop button) even for
    /// commands that stay silent.
    AttachToolTerminal { tool_id: String },
    /// Append raw terminal output (ANSI escapes included) for frontends
    /// that render it in a terminal emulator
    AppendToolTerminalOutput { tool_id: String, bytes: Vec<u8> },
    /// The process backing a tool's terminal exited; mark the display-only
    /// terminal finished so the card stops showing the running spinner.
    SetToolTerminalExited {
        tool_id: String,
        exit_code: Option<i32>,
    },
    /// Update the session plan display
    UpdatePlan { plan: PlanState },
    /// Set all messages at once (for session loading, clears existing)
    SetMessages {
        messages: Vec<MessageData>,
        session_id: Option<String>,
        tool_results: Vec<ToolResultData>,
    },
    /// Streaming started for a request.
    /// The `node_id` is pre-allocated so the UI container is tagged from the start
    /// with the same ID that will be used when the message is persisted.
    StreamingStarted { request_id: u64, node_id: NodeId },
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
    ClearMessages,
    /// Update metadata for a single session without refreshing the entire list
    UpdateSessionMetadata { metadata: ChatMetadata },
    /// Update activity state for a single session
    UpdateSessionActivityState {
        session_id: String,
        activity_state: SessionActivityState,
    },
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
    /// Update the models that may be selected for the active session.
    UpdateAllowedModels { models: Vec<String> },
    /// Update the current sandbox selection in the UI
    UpdateSandboxPolicy { policy: SandboxPolicy },
    /// Update the current permission tier selection in the UI
    UpdatePermissionTier {
        tier: tools_core::permissions::PermissionTier,
    },
    /// The agent asks the user for permission to run a tool. Answered via
    /// `SessionService::respond_permission`; a
    /// [`UiEvent::ToolPermissionRequestResolved`] follows once settled.
    RequestToolPermission {
        request: crate::session::permissions::ToolPermissionRequestData,
    },
    /// A permission request was settled (answered, or dropped by a stop
    /// request); open prompts for it should dismiss.
    ToolPermissionRequestResolved { request_id: String },

    /// Schedule a debounced save of the per-session UI state file.
    /// Sent after any mutation to the UI state (tool collapse toggle, plan
    /// toggle, etc.).  The handler cancels any pending save timer and starts
    /// a new one.
    PersistUiState,

    // === Session Branching Events ===
    /// The transcript was truncated for a message edit and the message
    /// content should be loaded into the input area. Emitted by the GPUI
    /// edit flow after `SessionService::start_message_edit`.
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
    /// Append new messages to the current session display (incremental update).
    /// Used by the file watcher when an external agent appends messages.
    AppendMessages {
        messages: Vec<MessageData>,
        tool_results: Vec<ToolResultData>,
    },

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

    // === Configuration Events ===
    /// Configuration files (providers.json / models.json) were changed on disk.
    /// The UI should reload model lists, settings sections, etc.
    ConfigChanged,
}
