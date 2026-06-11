//! The agent loop's UI vocabulary (§3.8 of the extraction plan).
//!
//! The loop reports its progress in these events only; the application
//! translates them into its richer event vocabulary (see
//! [`crate::ui::UiEvent::from_agent`]). Destined for the `agent_core` crate.

use crate::tools::core::ImageData;
use crate::ui::ToolStatus;

/// What the agent is currently doing, as far as the UI is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentActivity {
    /// The loop is active (running tools, processing).
    Running,
    /// A request is in flight and the response hasn't started streaming yet.
    WaitingForResponse,
}

/// Events the agent loop sends to the UI.
#[derive(Debug, Clone)]
pub enum AgentUiEvent {
    /// A user message entered the conversation history.
    UserInputAppended {
        content: String,
        /// Persistence node ID of the message, when already known.
        node_id: Option<u64>,
    },
    /// Streaming started for a request. The `node_id` is pre-allocated so the
    /// UI container is tagged from the start with the same ID that will be
    /// used when the message is persisted.
    StreamingStarted { request_id: u64, node_id: u64 },
    /// Streaming stopped for a request.
    StreamingStopped {
        request_id: u64,
        cancelled: bool,
        error: Option<String>,
    },
    /// Discard all UI content produced by a failed streaming request.
    /// Sent before a retry so that UIs can drop the partial output.
    RollbackStreaming { request_id: u64 },
    /// Add or update a tool parameter. When `replace` is true (post-execution
    /// format-on-save updates), the value replaces the existing parameter
    /// value instead of being appended.
    UpdateToolParameter {
        tool_id: String,
        name: String,
        value: String,
        replace: bool,
    },
    /// Update a tool status.
    UpdateToolStatus {
        tool_id: String,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
        duration_seconds: Option<f64>,
        images: Vec<ImageData>,
    },
    /// Show a brief, auto-dismissing status notification
    /// (e.g. "Stream interrupted — retrying").
    ShowTransientStatus { message: String },
    /// The loop's activity changed.
    ActivityChanged { activity: AgentActivity },
}
