//! The agent loop's UI boundary: a minimal, agent-centric event vocabulary
//! plus the [`AgentUi`] trait the loop talks to. Applications adapt these
//! events into their own richer UI event types.

use async_trait::async_trait;
use llm::{Message, StreamingChunk};
use std::sync::Arc;
use thiserror::Error;
use tools_core::ImageData;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Pending, // Default status when a tool appears in the stream
    Running, // Tool is currently being executed
    Success, // Execution was successful
    Error,   // Error during execution
}

#[derive(Error, Debug)]
pub enum UIError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
}

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
    /// Streaming tool output chunk (plain text, safe for text frontends)
    ToolOutput { tool_id: String, chunk: String },
    /// Streaming raw terminal output chunk (ANSI escape sequences
    /// included). Emitted alongside the plain `ToolOutput` for frontends
    /// with a terminal emulator (GPUI feeds these into a display-only
    /// terminal for live colored output); text frontends ignore it.
    ToolTerminalOutput { tool_id: String, bytes: Vec<u8> },
    /// The process backing a tool's terminal exited.
    ToolTerminalExited {
        tool_id: String,
        exit_code: Option<i32>,
    },
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

/// What the agent loop needs from a user interface. Applications implement
/// this once, usually as an adapter that translates [`AgentUiEvent`]s into
/// their own event vocabulary.
#[async_trait]
pub trait AgentUi: Send + Sync {
    /// Send a loop event to the UI
    async fn send_event(&self, event: AgentUiEvent) -> Result<(), UIError>;

    /// Display a streaming fragment with specific type information
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError>;

    /// Check if streaming should continue
    fn should_streaming_continue(&self) -> bool;

    /// Notify the UI about rate limiting and countdown
    fn notify_rate_limit(&self, seconds_remaining: u64);

    /// Clear rate limit notification
    fn clear_rate_limit(&self);
}

/// Predicate deciding whether a tool's invocation is hidden from the UI.
pub type HiddenTools = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Common trait for stream processors. Implementations are constructed by
/// their dialect (see `ToolDialect::stream_processor`) with whatever context
/// they need.
pub trait StreamProcessorTrait: Send + Sync {
    /// Process a streaming chunk and send display fragments to the UI
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError>;

    /// Extract display fragments from a stored message without sending to UI
    /// Used for session loading to reconstruct fragment sequences
    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError>;
}
