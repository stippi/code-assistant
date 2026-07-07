use anyhow::Result;
use llm::{ContentBlock, Message};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// Agent instances are created on-demand, no need to import
use crate::agent::SubAgentCancellationRegistry;
use crate::persistence::{ChatMetadata, ChatSession, NodeId};
use crate::ui::streaming::create_stream_processor;
use crate::ui::ui_events::{MessageData, MessageRole, UiEvent};
use crate::ui::{DisplayFragment, UIError, UserInterface};
use crate::utils::file_utils::AgentLockGuard;
use async_trait::async_trait;
use sandbox::SandboxContext;
use tracing::{debug, error};

/// Represents the current activity state of a session
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SessionActivityState {
    /// No agent running, waiting for user input
    #[default]
    Idle,
    /// Agent loop is active (running tools, processing)
    AgentRunning,
    /// Agent sent LLM request, waiting for first streaming chunk
    WaitingForResponse,
    /// Agent is rate limited with countdown
    RateLimited { seconds_remaining: u64 },
    /// Agent terminated with an error
    Errored { message: String },
    /// Agent is running in another code-assistant process.
    /// The session is view-only in this instance: the user can browse
    /// messages but cannot send or queue new ones.
    RunningExternally,
}

impl SessionActivityState {
    /// Whether this state is terminal (agent is no longer running).
    /// Terminal states block transitions to non-terminal states until a new
    /// agent is explicitly started.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Idle | Self::Errored { .. })
    }

    /// Whether the session is locked by another code-assistant instance.
    pub fn is_running_externally(&self) -> bool {
        matches!(self, Self::RunningExternally)
    }
}

/// Shared handle to a session's activity state that owns the transition
/// rules. The [`SessionEventPublisher`] reports streaming lifecycle moments
/// through the `on_*` methods and publishes whatever state change they
/// return — what a moment *means* for the state is decided here.
#[derive(Clone, Default)]
pub struct SessionActivity {
    state: Arc<Mutex<SessionActivityState>>,
}

impl SessionActivity {
    pub fn get(&self) -> SessionActivityState {
        self.state.lock().unwrap().clone()
    }

    /// Set the state unconditionally. Reserved for the agent lifecycle
    /// itself (start, completion, error) — the only places allowed to leave
    /// a terminal state.
    pub fn set(&self, state: SessionActivityState) {
        *self.state.lock().unwrap() = state;
    }

    /// Apply a transition respecting the terminal-state rule: terminal
    /// states (Idle, Errored) persist until a new agent is explicitly
    /// started via [`SessionActivity::set`]. Returns the new state if it
    /// changed, so the caller knows whether to broadcast.
    pub fn try_transition(&self, new_state: SessionActivityState) -> Option<SessionActivityState> {
        let mut state = self.state.lock().unwrap();
        if state.is_terminal() && !new_state.is_terminal() {
            debug!(
                "Ignoring activity transition from {:?} to {:?}",
                *state, new_state
            );
            return None;
        }
        if *state == new_state {
            return None;
        }
        *state = new_state.clone();
        Some(new_state)
    }

    /// An LLM request was sent and the response hasn't started streaming.
    pub fn on_streaming_started(&self) -> Option<SessionActivityState> {
        self.try_transition(SessionActivityState::WaitingForResponse)
    }

    /// Streaming ended. Moves back to AgentRunning on success; a cancelled
    /// or failed request leaves the state untouched (the agent task decides
    /// the final state), as does an agent that already completed.
    pub fn on_streaming_stopped(
        &self,
        cancelled: bool,
        errored: bool,
    ) -> Option<SessionActivityState> {
        if cancelled || errored {
            return None;
        }
        match self.get() {
            SessionActivityState::WaitingForResponse | SessionActivityState::RateLimited { .. } => {
                self.try_transition(SessionActivityState::AgentRunning)
            }
            _ => None,
        }
    }

    /// The stream produced its first visible output.
    pub fn on_visible_output(&self) -> Option<SessionActivityState> {
        match self.get() {
            SessionActivityState::WaitingForResponse => {
                self.try_transition(SessionActivityState::AgentRunning)
            }
            _ => None,
        }
    }

    pub fn on_rate_limited(&self, seconds_remaining: u64) -> Option<SessionActivityState> {
        self.try_transition(SessionActivityState::RateLimited { seconds_remaining })
    }

    pub fn on_rate_limit_cleared(&self) -> Option<SessionActivityState> {
        self.try_transition(SessionActivityState::WaitingForResponse)
    }
}

/// Buffered tool-status update received while the session was disconnected.
/// Keyed by `tool_id` so only the most recent status per tool is retained.
type ToolStatusBuffer = HashMap<String, crate::ui::ui_events::ToolResultData>;

/// Represents a single session instance with its own agent and state
pub struct SessionInstance {
    /// The session data (messages, metadata, etc.)
    pub session: ChatSession,

    // Agent instances are created on-demand and moved into tokio tasks
    // We only track the task handle, not the agent itself
    /// Task handle for the running agent (None if not running)
    pub task_handle: Option<JoinHandle<Result<()>>>,

    /// In-flight DisplayFragments of the currently streaming response.
    /// Written by the [`SessionEventPublisher`]; included in snapshots so a
    /// frontend connecting mid-stream sees the partial message.
    pub fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,

    /// The pre-allocated node id of the currently streaming response (from
    /// `StreamingStarted`). Snapshots tag the partial message with it so
    /// frontends can deduplicate against the persisted message later.
    pub in_flight_node_id: Arc<Mutex<Option<NodeId>>>,

    /// Latest live `UpdateToolStatus` per tool of the current agent run.
    /// Written by the [`SessionEventPublisher`]; merged into snapshots
    /// (persisted results take precedence). Cleared on agent start.
    pub tool_status_buffer: Arc<Mutex<ToolStatusBuffer>>,

    /// Current activity state of this session (shared with the publisher)
    pub activity: SessionActivity,

    /// Set when a user requests the running agent to stop; checked by the
    /// agent at streaming checkpoints. Cleared when a new agent starts.
    pub stop_requested: Arc<std::sync::atomic::AtomicBool>,

    /// Pending user message (structured content blocks) that will be processed by the next agent iteration
    pub pending_message: Arc<Mutex<Option<Vec<ContentBlock>>>>,

    /// Tracks sandbox-approved roots for this session
    pub sandbox_context: Arc<SandboxContext>,

    /// Tools the user granted "for this session" via the permission tier
    /// gate. Shared with running agents; survives across agent runs.
    pub permissions: tools_core::ToolPermissions,

    /// Permission requests currently awaiting a user decision.
    pub pending_permission_requests: Arc<crate::session::permissions::PendingPermissionRequests>,

    /// Cancellation registry for sub-agents running in agent tasks
    pub sub_agent_cancellation_registry: Arc<SubAgentCancellationRegistry>,

    /// Live PTY sessions started by this session's agents (execute_command
    /// session mode). Survives across agent runs; dropping the instance
    /// terminates all remaining sessions.
    pub pty_sessions: Arc<pty_session::PtySessionManager>,

    /// Exclusive cross-process lock held while an agent is running.
    ///
    /// Acquired before spawning the agent task, released on task completion
    /// or abort.  Prevents two code-assistant processes from running an
    /// agent for the same session simultaneously.
    pub agent_lock: Option<AgentLockGuard>,

    /// The active_path that the UI has been told about (either via full load
    /// or via `AppendMessages`).  Used by the file-watcher refresh logic to
    /// determine which nodes are truly "new" and avoid duplicate appends.
    ///
    /// Updated when:
    /// - A full session load sends all messages to the UI
    /// - An incremental append tells the UI about new nodes
    /// - A reload happens while the local agent is running (streaming covers UI)
    pub last_ui_synced_path: crate::persistence::ConversationPath,

    /// Number of tool executions the UI has been told about.
    pub last_ui_synced_tool_count: usize,

    /// The tool registry this session's agent runs with (for deserializing
    /// persisted tool executions and stream-processor metadata lookups).
    pub tool_registry: Arc<crate::tools::core::ToolRegistry>,
}

impl SessionInstance {
    /// Create a new session instance
    pub fn new(session: ChatSession, tool_registry: Arc<crate::tools::core::ToolRegistry>) -> Self {
        let sandbox_context = Arc::new(SandboxContext::default());
        if let Some(path) = session.config.effective_project_path() {
            let _ = sandbox_context.register_root(path);
        }

        let initial_path = session.active_path.clone();
        let initial_tool_count = session.tool_executions.len();

        Self {
            session,
            task_handle: None,
            fragment_buffer: Arc::new(Mutex::new(VecDeque::new())),
            tool_status_buffer: Arc::new(Mutex::new(HashMap::new())),
            in_flight_node_id: Arc::new(Mutex::new(None)),
            activity: SessionActivity::default(),
            stop_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pending_message: Arc::new(Mutex::new(None)),
            sandbox_context,
            permissions: tools_core::ToolPermissions::default(),
            pending_permission_requests: Arc::new(
                crate::session::permissions::PendingPermissionRequests::default(),
            ),
            sub_agent_cancellation_registry: Arc::new(SubAgentCancellationRegistry::default()),
            pty_sessions: Arc::new(pty_session::PtySessionManager::default()),
            agent_lock: None,
            last_ui_synced_path: initial_path,
            last_ui_synced_tool_count: initial_tool_count,
            tool_registry,
        }
    }

    /// Cancel a running sub-agent by its tool ID
    /// Returns true if a sub-agent was found and cancelled, false otherwise
    pub fn cancel_sub_agent(&self, tool_id: &str) -> bool {
        self.sub_agent_cancellation_registry.cancel(tool_id)
    }

    /// Ask the running agent to stop at its next streaming checkpoint.
    /// Pending permission requests resolve as denied so the agent does not
    /// stay blocked waiting for an answer.
    pub fn request_stop(&self) {
        self.stop_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.pending_permission_requests.deny_all();
    }

    /// Reset per-run state when a new agent starts: clears a previous stop
    /// request, the live tool-status map of the prior run, and any stale
    /// permission requests.
    pub fn begin_agent_run(&self) {
        self.stop_requested
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut buf) = self.tool_status_buffer.lock() {
            buf.clear();
        }
        self.pending_permission_requests.deny_all();
    }

    /// Get the current activity state
    pub fn get_activity_state(&self) -> SessionActivityState {
        self.activity.get()
    }

    /// Set the activity state
    pub fn set_activity_state(&self, state: SessionActivityState) {
        self.activity.set(state);
    }

    /// Get all buffered fragments and optionally clear the buffer
    pub fn get_buffered_fragments(&self, clear: bool) -> Vec<DisplayFragment> {
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            let fragments: Vec<_> = buffer.iter().cloned().collect();
            if clear {
                buffer.clear();
            }
            fragments
        } else {
            Vec::new()
        }
    }

    /// Clear the fragment buffer
    pub fn clear_fragment_buffer(&self) {
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.clear();
        }
    }

    /// Terminate the running agent and release the cross-process agent lock.
    pub fn terminate_agent(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
            self.clear_fragment_buffer();
        }
        // Release the cross-process agent lock
        self.agent_lock = None;
    }

    /// Add a message with optional branching support.
    /// If `branch_parent_id` is Some, creates a new branch from that parent.
    /// If `branch_parent_id` is None, appends to the end of the active path.
    pub fn add_message_with_branch(
        &mut self,
        message: Message,
        branch_parent_id: Option<NodeId>,
    ) -> Result<NodeId> {
        let node_id = if let Some(parent_id) = branch_parent_id {
            // Branching: create new message as child of specified parent
            debug!(
                "Creating new branch from parent {} in session {}",
                parent_id, self.session.id
            );
            self.session
                .add_message_with_parent(message, Some(parent_id))
        } else {
            // Normal append: add to end of active path
            self.session.add_message(message)
        };

        Ok(node_id)
    }

    /// Get the current context size (input tokens + cache reads from most recent assistant message)
    /// This represents the total tokens being processed in the current LLM request
    #[allow(dead_code)]
    pub fn get_current_context_size(&self) -> u32 {
        // Find the most recent assistant message with usage data
        for message in self.session.get_active_messages().iter().rev() {
            if matches!(message.role, llm::MessageRole::Assistant) {
                if let Some(usage) = &message.usage {
                    return usage.input_tokens + usage.cache_read_input_tokens;
                }
            }
        }
        0
    }

    /// Calculate total usage across the entire session
    #[allow(dead_code)]
    pub fn calculate_total_usage(&self) -> llm::Usage {
        let mut total = llm::Usage::zero();

        for message in self.session.get_active_messages() {
            if let Some(usage) = &message.usage {
                total.input_tokens += usage.input_tokens;
                total.output_tokens += usage.output_tokens;
                total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
                total.cache_read_input_tokens += usage.cache_read_input_tokens;
            }
        }

        total
    }

    /// Get usage from the most recent assistant message
    fn get_last_usage(&self) -> llm::Usage {
        for message in self.session.get_active_messages().iter().rev() {
            if matches!(message.role, llm::MessageRole::Assistant) {
                if let Some(usage) = &message.usage {
                    return usage.clone();
                }
            }
        }
        llm::Usage::zero()
    }

    /// Reload session data from persistence
    /// This ensures SessionInstance has the latest state even if agents have made changes
    pub fn reload_from_persistence(
        &mut self,
        persistence: &crate::persistence::FileSessionPersistence,
    ) -> anyhow::Result<()> {
        if let Some(session) = persistence.load_chat_session(&self.session.id)? {
            debug!("Reloading session {} from persistence", self.session.id);
            self.session = session;
            if let Some(path) = self.session.config.effective_project_path() {
                let _ = self.sandbox_context.register_root(path);
            }
        }
        Ok(())
    }

    /// Create the publisher this session's agent talks to: it records
    /// in-flight state for snapshots and publishes everything session-tagged
    /// to the core→UI broadcast stream.
    pub fn create_publisher(
        &self,
        events: crate::session::event_stream::EventStream,
    ) -> Arc<dyn UserInterface> {
        Arc::new(SessionEventPublisher {
            events,
            fragment_buffer: self.fragment_buffer.clone(),
            in_flight_node_id: self.in_flight_node_id.clone(),
            tool_status_buffer: self.tool_status_buffer.clone(),
            activity: self.activity.clone(),
            stop_requested: self.stop_requested.clone(),
            session_id: self.session.id.clone(),
        })
    }

    /// Generate UI events for connecting to this session.
    /// Returns SetMessages event with all session messages including incomplete streaming message.
    ///
    /// If `until_node_id` is `Some(_)`, the transcript is truncated to messages
    /// up to and including that node. This is used to restore the "edit mode"
    /// view (truncated to the branch parent) directly when connecting to a
    /// session whose draft is in edit mode, avoiding a full-then-truncate flash.
    pub fn build_snapshot(
        &self,
        until_node_id: Option<crate::persistence::NodeId>,
    ) -> Result<crate::session::SessionSnapshot, anyhow::Error> {
        // Convert session messages to UI data (optionally truncated for edit mode)
        let mut messages =
            self.convert_messages_to_ui_data_until(self.session.config.tool_syntax, until_node_id)?;
        let mut tool_results = self.convert_tool_executions_to_ui_data()?;

        // Merge in the latest live tool statuses (e.g. from running
        // sub-agents). Only inject entries that don't already have a
        // persisted result — the persisted result is authoritative once it
        // exists.
        if let Ok(buf) = self.tool_status_buffer.lock() {
            for result_data in buf.values() {
                if !tool_results
                    .iter()
                    .any(|r| r.tool_id == result_data.tool_id)
                {
                    tool_results.push(result_data.clone());
                }
            }
        }

        // If currently streaming, add the incomplete message as additional
        // MessageData, tagged with the pre-allocated node id so frontends can
        // deduplicate it against the persisted message later.
        let buffered_fragments = self.get_buffered_fragments(false); // Don't clear buffer
        if !buffered_fragments.is_empty() {
            messages.push(MessageData {
                role: MessageRole::Assistant,
                fragments: buffered_fragments,
                node_id: self.in_flight_node_id.lock().ok().and_then(|id| *id),
                branch_info: None, // No branch info for incomplete message
            });
        }

        let metadata = ChatMetadata {
            id: self.session.id.clone(),
            name: self.session.name.clone(),
            created_at: self.session.created_at,
            updated_at: self.session.updated_at,

            message_count: self.session.get_active_messages().len(),
            total_usage: self.calculate_total_usage(),
            last_usage: self.get_last_usage(),

            tokens_limit: None, // Will be updated by persistence layer if available
            tool_syntax: self.session.config.tool_syntax,
            initial_project: self.session.config.initial_project.clone(),
            plan_collapsed: self.session.plan_collapsed,
            is_resumable: self.session.is_resumable(),
        };

        let pending_message = self.pending_message.lock().ok().and_then(|pending| {
            pending
                .as_ref()
                .map(|blocks| crate::utils::content::text_summary_from_blocks(blocks))
        });

        Ok(crate::session::SessionSnapshot {
            session_id: self.session.id.clone(),
            messages,
            tool_results,
            plan: self.session.plan.clone(),
            activity_state: self.get_activity_state(),
            metadata,
            pending_message,
            // Filled in by the SessionManager, which owns model resolution.
            current_model: String::new(),
            allowed_models: Vec::new(),
            sandbox_policy: self.session.config.sandbox_policy.clone(),
            permission_tier: self.session.config.permission_tier,
            pending_permission_requests: self.pending_permission_requests.snapshot(),
        })
    }

    /// Convert session messages to UI MessageData format
    pub fn convert_messages_to_ui_data(
        &self,
        tool_syntax: crate::types::ToolSyntax,
    ) -> Result<Vec<MessageData>, anyhow::Error> {
        self.convert_messages_to_ui_data_until(tool_syntax, None)
    }

    /// Convert session messages to UI MessageData format, stopping at a specific node
    /// If `until_node_id` is Some, includes all messages up to and including that node.
    /// If `until_node_id` is None, includes all messages (same as convert_messages_to_ui_data).
    pub fn convert_messages_to_ui_data_until(
        &self,
        tool_syntax: crate::types::ToolSyntax,
        until_node_id: Option<crate::persistence::NodeId>,
    ) -> Result<Vec<MessageData>, anyhow::Error> {
        // Create dummy UI for stream processor
        struct DummyUI;
        #[async_trait::async_trait]
        impl crate::ui::UserInterface for DummyUI {
            async fn send_event(
                &self,
                _event: crate::ui::UiEvent,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }

            fn display_fragment(
                &self,
                _fragment: &crate::ui::DisplayFragment,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            fn should_streaming_continue(&self) -> bool {
                true
            }
            fn notify_rate_limit(&self, _seconds_remaining: u64) {}
            fn clear_rate_limit(&self) {}
        }

        let dummy_ui: std::sync::Arc<dyn crate::ui::UserInterface> = std::sync::Arc::new(DummyUI);
        let hidden_tools = self
            .tool_registry
            .hidden_tools(crate::tools::core::ToolScope::Agent.tag());
        let mut processor = create_stream_processor(
            tool_syntax,
            dummy_ui,
            0,
            hidden_tools,
            self.tool_registry.clone(),
        );

        let mut messages_data = Vec::new();

        // Build message iterator from tree or legacy messages
        let message_iter: Vec<(Option<crate::persistence::NodeId>, &llm::Message)> =
            if !self.session.message_nodes.is_empty() {
                // Use active path from tree, but stop at until_node_id
                let mut iter = Vec::new();
                for &node_id in &self.session.active_path {
                    if let Some(node) = self.session.message_nodes.get(&node_id) {
                        iter.push((Some(node_id), &node.message));
                        // Stop after adding the until_node_id
                        if until_node_id == Some(node_id) {
                            break;
                        }
                    }
                }
                iter
            } else {
                // Fall back to legacy linear messages (no until_node_id support)
                self.session
                    .messages
                    .iter()
                    .map(|msg| (None, msg))
                    .collect()
            };

        for (node_id, message) in message_iter {
            if message.is_compaction_summary {
                let summary = match &message.content {
                    llm::MessageContent::Text(text) => text.trim().to_string(),
                    llm::MessageContent::Structured(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                            llm::ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_string(),
                };

                messages_data.push(MessageData {
                    role: MessageRole::System,
                    fragments: vec![crate::ui::DisplayFragment::CompactionDivider { summary }],
                    node_id,
                    branch_info: node_id.and_then(|id| self.session.get_branch_info(id)),
                });
                continue;
            }

            // Filter out tool-result user messages
            if message.role == llm::MessageRole::User {
                match &message.content {
                    llm::MessageContent::Text(text) if text.trim().is_empty() => continue,
                    llm::MessageContent::Structured(blocks) => {
                        let has_tool_results = blocks
                            .iter()
                            .any(|block| matches!(block, llm::ContentBlock::ToolResult { .. }));
                        if has_tool_results {
                            continue;
                        }
                    }
                    _ => {}
                }
            }

            match processor.extract_fragments_from_message(message) {
                Ok(fragments) => {
                    let role = match message.role {
                        llm::MessageRole::User => MessageRole::User,
                        llm::MessageRole::Assistant => MessageRole::Assistant,
                    };
                    messages_data.push(MessageData {
                        role,
                        fragments,
                        node_id,
                        branch_info: node_id.and_then(|id| self.session.get_branch_info(id)),
                    });
                }
                Err(e) => {
                    error!("Failed to extract fragments from message: {}", e);
                }
            }
        }

        Ok(messages_data)
    }

    /// Convert a specific subset of nodes (by their IDs) to UI MessageData.
    /// Used for incremental updates when new nodes are appended to the active path.
    pub fn convert_messages_from_nodes(
        &self,
        node_ids: &[crate::persistence::NodeId],
        tool_syntax: crate::types::ToolSyntax,
    ) -> Result<Vec<MessageData>, anyhow::Error> {
        struct DummyUI;
        #[async_trait::async_trait]
        impl crate::ui::UserInterface for DummyUI {
            async fn send_event(
                &self,
                _event: crate::ui::UiEvent,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            fn display_fragment(
                &self,
                _fragment: &crate::ui::DisplayFragment,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            fn should_streaming_continue(&self) -> bool {
                true
            }
            fn notify_rate_limit(&self, _seconds_remaining: u64) {}
            fn clear_rate_limit(&self) {}
        }

        let dummy_ui: std::sync::Arc<dyn crate::ui::UserInterface> = std::sync::Arc::new(DummyUI);
        let hidden_tools = self
            .tool_registry
            .hidden_tools(crate::tools::core::ToolScope::Agent.tag());
        let mut processor = create_stream_processor(
            tool_syntax,
            dummy_ui,
            0,
            hidden_tools,
            self.tool_registry.clone(),
        );

        let mut messages_data = Vec::new();

        for &node_id in node_ids {
            let Some(node) = self.session.message_nodes.get(&node_id) else {
                continue;
            };
            let message = &node.message;

            if message.is_compaction_summary {
                let summary = match &message.content {
                    llm::MessageContent::Text(text) => text.trim().to_string(),
                    llm::MessageContent::Structured(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                            llm::ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_string(),
                };
                messages_data.push(MessageData {
                    role: MessageRole::System,
                    fragments: vec![crate::ui::DisplayFragment::CompactionDivider { summary }],
                    node_id: Some(node_id),
                    branch_info: self.session.get_branch_info(node_id),
                });
                continue;
            }

            // Skip tool-result user messages
            if message.role == llm::MessageRole::User {
                match &message.content {
                    llm::MessageContent::Text(text) if text.trim().is_empty() => continue,
                    llm::MessageContent::Structured(blocks) => {
                        let has_tool_results = blocks
                            .iter()
                            .any(|block| matches!(block, llm::ContentBlock::ToolResult { .. }));
                        if has_tool_results {
                            continue;
                        }
                    }
                    _ => {}
                }
            }

            match processor.extract_fragments_from_message(message) {
                Ok(fragments) => {
                    let role = match message.role {
                        llm::MessageRole::User => MessageRole::User,
                        llm::MessageRole::Assistant => MessageRole::Assistant,
                    };
                    messages_data.push(MessageData {
                        role,
                        fragments,
                        node_id: Some(node_id),
                        branch_info: self.session.get_branch_info(node_id),
                    });
                }
                Err(e) => {
                    error!("Failed to extract fragments from message: {}", e);
                }
            }
        }

        Ok(messages_data)
    }

    /// Convert tool executions to UI tool result data
    pub fn convert_tool_executions_to_ui_data(
        &self,
    ) -> Result<Vec<crate::ui::ui_events::ToolResultData>, anyhow::Error> {
        use crate::tools::core::ResourcesTracker;

        // Build a lookup map: tool_use_id → duration (seconds) from ToolResult ContentBlocks
        // in the persisted message tree. This gives us stable execution durations for restored sessions.
        let tool_result_durations = self.build_tool_result_duration_map();

        let mut tool_results = Vec::new();
        let mut resources_tracker = ResourcesTracker::new();

        for serialized_execution in &self.session.tool_executions {
            // A tool that has since disappeared (e.g. a reconfigured MCP
            // server) must not break rendering the session: skip its records.
            if !serialized_execution.tool_available(self.tool_registry.as_ref()) {
                tracing::warn!(
                    "Skipping recorded execution of unavailable tool '{}'",
                    serialized_execution.tool_name
                );
                continue;
            }

            // Deserialize the tool execution
            let execution = serialized_execution.deserialize(self.tool_registry.as_ref())?;

            // Generate status and output from result
            let success = execution.result.is_success();
            let status = if success {
                crate::ui::ToolStatus::Success
            } else {
                crate::ui::ToolStatus::Error
            };

            let short_output = execution.result.as_render().status();
            // Use render_for_ui() to get the UI-specific output (e.g., JSON for spawn_agent)
            let output = execution
                .result
                .as_render()
                .render_for_ui(&mut resources_tracker);

            let duration_seconds = tool_result_durations
                .get(&execution.tool_request.id)
                .copied();

            // Collect image data from tools that produce visual output
            let images = execution.result.render_images();

            tool_results.push(crate::ui::ui_events::ToolResultData {
                tool_id: execution.tool_request.id,
                status,
                message: Some(short_output),
                output: Some(output),
                styled_output: None, // Not available for restored sessions
                duration_seconds,
                images,
            });
        }

        Ok(tool_results)
    }

    /// Build a map from tool_use_id to execution duration (seconds) by scanning
    /// ToolResult ContentBlocks in the persisted message tree.
    fn build_tool_result_duration_map(&self) -> std::collections::HashMap<String, f64> {
        let mut map = std::collections::HashMap::new();

        // Iterate over all messages in the active path (tree) or legacy messages
        let messages: Vec<&llm::Message> = if !self.session.message_nodes.is_empty() {
            self.session
                .active_path
                .iter()
                .filter_map(|node_id| self.session.message_nodes.get(node_id))
                .map(|node| &node.message)
                .collect()
        } else {
            self.session.messages.iter().collect()
        };

        for message in messages {
            if let llm::MessageContent::Structured(blocks) = &message.content {
                for block in blocks {
                    if let llm::ContentBlock::ToolResult { tool_use_id, .. } = block {
                        if let Some(duration) = block.duration() {
                            map.insert(tool_use_id.clone(), duration.as_secs_f64());
                        }
                    }
                }
            }
        }

        map
    }
}

/// The session's publisher onto the core→UI broadcast stream, implementing
/// [`UserInterface`] for the agent seam.
///
/// It owns no state logic: activity transitions are decided by the shared
/// [`SessionActivity`] handle; in-flight fragments and live tool statuses
/// are recorded as session state so snapshots can include them. Which
/// frontend (if any) renders the published events is not its concern.
struct SessionEventPublisher {
    events: crate::session::event_stream::EventStream,
    /// In-flight fragments of the currently streaming response, kept for
    /// snapshots (the content is not persisted until the message completes).
    fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,
    /// Pre-allocated node id of the in-flight response (see
    /// [`SessionInstance::in_flight_node_id`]).
    in_flight_node_id: Arc<Mutex<Option<NodeId>>>,
    /// Latest live status per tool of the current agent run, kept for
    /// snapshots (persisted results take precedence when merging).
    tool_status_buffer: Arc<Mutex<ToolStatusBuffer>>,
    activity: SessionActivity,
    stop_requested: Arc<std::sync::atomic::AtomicBool>,
    session_id: String,
}

impl SessionEventPublisher {
    /// Publish an activity-state change produced by [`SessionActivity`].
    fn publish_activity_change(&self, change: Option<SessionActivityState>) {
        let Some(activity_state) = change else {
            return;
        };
        self.events.publish_ui(
            &self.session_id,
            UiEvent::UpdateSessionActivityState {
                session_id: self.session_id.clone(),
                activity_state,
            },
        );
    }
}

#[async_trait]
impl UserInterface for SessionEventPublisher {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        // Handle special events that need buffer management and activity state updates
        match &event {
            UiEvent::StreamingStarted { node_id, .. } => {
                // Reset the in-flight state for the new LLM request
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                if let Ok(mut in_flight) = self.in_flight_node_id.lock() {
                    *in_flight = Some(*node_id);
                }
                self.publish_activity_change(self.activity.on_streaming_started());
            }
            UiEvent::StreamingStopped {
                cancelled, error, ..
            } => {
                // Clear the in-flight state when the LLM request ends —
                // fragments are now part of message history
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                if let Ok(mut in_flight) = self.in_flight_node_id.lock() {
                    *in_flight = None;
                }
                if let Some(error_msg) = error {
                    // The agent task will set the final state when it terminates
                    debug!(
                        "StreamingStopped with error for session {}: {}",
                        self.session_id, error_msg
                    );
                }
                self.publish_activity_change(
                    self.activity
                        .on_streaming_stopped(*cancelled, error.is_some()),
                );
            }
            UiEvent::RollbackStreaming { .. } => {
                // Discard the in-flight state — the partial content is being
                // discarded before a retry
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                if let Ok(mut in_flight) = self.in_flight_node_id.lock() {
                    *in_flight = None;
                }
            }
            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } if session_id == &self.session_id => {
                self.publish_activity_change(self.activity.try_transition(activity_state.clone()));
                return Ok(());
            }
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                styled_output,
                duration_seconds,
                images,
            } => {
                // Record the latest status per tool so snapshots can include
                // live tool state that isn't persisted yet.
                if let Ok(mut buf) = self.tool_status_buffer.lock() {
                    buf.insert(
                        tool_id.clone(),
                        crate::ui::ui_events::ToolResultData {
                            tool_id: tool_id.clone(),
                            status: *status,
                            message: message.clone(),
                            output: output.clone(),
                            styled_output: styled_output.clone(),
                            duration_seconds: *duration_seconds,
                            images: images.clone(),
                        },
                    );
                }
            }
            _ => {}
        }

        self.events.publish_ui(&self.session_id, event);
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Record the in-flight fragment for snapshots. Cleared on streaming
        // start/stop/rollback, so the buffer is bounded by one response.
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.push_back(fragment.clone());
        }

        self.events.publish(
            Some(self.session_id.clone()),
            crate::session::event_stream::EventPayload::Fragment(fragment.clone()),
        );

        // Transition from WaitingForResponse to AgentRunning only when the
        // fragment actually produces something visible in the UI. Some
        // providers emit empty deltas (e.g. an empty PlainText at the start
        // of a content block) or purely structural events which would
        // otherwise hide the activity spinner before any content appears in
        // the MessagesView.
        let has_visible_content = match fragment {
            DisplayFragment::PlainText(s) => !s.is_empty(),
            DisplayFragment::ThinkingText { text, .. } => !text.is_empty(),
            DisplayFragment::ReasoningSummaryDelta(s) => !s.is_empty(),
            DisplayFragment::ToolParameter { value, .. } => !value.is_empty(),
            DisplayFragment::ToolOutput { chunk, .. } => !chunk.is_empty(),
            DisplayFragment::ToolTerminalOutput { bytes, .. } => !bytes.is_empty(),
            DisplayFragment::Image { .. }
            | DisplayFragment::ToolName { .. }
            | DisplayFragment::ReasoningSummaryStart
            | DisplayFragment::CompactionDivider { .. } => true,
            DisplayFragment::ToolEnd { .. }
            | DisplayFragment::ToolTerminal { .. }
            | DisplayFragment::ReasoningComplete
            | DisplayFragment::HiddenToolCompleted => false,
        };

        if has_visible_content {
            self.publish_activity_change(self.activity.on_visible_output());
        }

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        !self
            .stop_requested
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        self.publish_activity_change(self.activity.on_rate_limited(seconds_remaining));
    }

    fn clear_rate_limit(&self) {
        self.publish_activity_change(self.activity.on_rate_limit_cleared());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn activity_with(state: SessionActivityState) -> SessionActivity {
        let activity = SessionActivity::default();
        activity.set(state);
        activity
    }

    #[test]
    fn terminal_states_block_transitions_until_explicit_set() {
        for terminal in [
            SessionActivityState::Idle,
            SessionActivityState::Errored {
                message: "boom".to_string(),
            },
        ] {
            let activity = activity_with(terminal.clone());
            assert_eq!(
                activity.try_transition(SessionActivityState::AgentRunning),
                None
            );
            assert_eq!(activity.get(), terminal);

            // An explicit set (agent start) leaves the terminal state.
            activity.set(SessionActivityState::AgentRunning);
            assert_eq!(activity.get(), SessionActivityState::AgentRunning);
        }
    }

    #[test]
    fn try_transition_reports_only_changes() {
        let activity = activity_with(SessionActivityState::AgentRunning);
        // Same state → no change to broadcast.
        assert_eq!(
            activity.try_transition(SessionActivityState::AgentRunning),
            None
        );
        assert_eq!(
            activity.try_transition(SessionActivityState::WaitingForResponse),
            Some(SessionActivityState::WaitingForResponse)
        );
    }

    #[test]
    fn streaming_stopped_only_resumes_running_state_on_success() {
        // Error → state untouched (the agent task decides the final state).
        let activity = activity_with(SessionActivityState::WaitingForResponse);
        assert_eq!(activity.on_streaming_stopped(false, true), None);
        assert_eq!(activity.get(), SessionActivityState::WaitingForResponse);

        // Cancelled → state untouched.
        assert_eq!(activity.on_streaming_stopped(true, false), None);

        // Success from WaitingForResponse → AgentRunning.
        assert_eq!(
            activity.on_streaming_stopped(false, false),
            Some(SessionActivityState::AgentRunning)
        );

        // Success while already AgentRunning → no change.
        assert_eq!(activity.on_streaming_stopped(false, false), None);

        // Success from RateLimited → AgentRunning.
        let activity = activity_with(SessionActivityState::RateLimited {
            seconds_remaining: 5,
        });
        assert_eq!(
            activity.on_streaming_stopped(false, false),
            Some(SessionActivityState::AgentRunning)
        );

        // Success after the agent already completed (Idle) → stays Idle.
        let activity = activity_with(SessionActivityState::Idle);
        assert_eq!(activity.on_streaming_stopped(false, false), None);
        assert_eq!(activity.get(), SessionActivityState::Idle);
    }

    #[test]
    fn visible_output_moves_waiting_to_running() {
        let activity = activity_with(SessionActivityState::WaitingForResponse);
        assert_eq!(
            activity.on_visible_output(),
            Some(SessionActivityState::AgentRunning)
        );
        // Only the first visible output transitions.
        assert_eq!(activity.on_visible_output(), None);

        // No transition when the agent already finished.
        let activity = activity_with(SessionActivityState::Idle);
        assert_eq!(activity.on_visible_output(), None);
    }

    #[test]
    fn rate_limit_round_trip() {
        let activity = activity_with(SessionActivityState::WaitingForResponse);
        assert_eq!(
            activity.on_rate_limited(30),
            Some(SessionActivityState::RateLimited {
                seconds_remaining: 30
            })
        );
        assert_eq!(
            activity.on_rate_limit_cleared(),
            Some(SessionActivityState::WaitingForResponse)
        );

        // Rate limit notifications after completion don't revive the session.
        let activity = activity_with(SessionActivityState::Idle);
        assert_eq!(activity.on_rate_limited(30), None);
        assert_eq!(activity.on_rate_limit_cleared(), None);
    }

    fn test_publisher(activity: SessionActivity, session_id: &str) -> SessionEventPublisher {
        SessionEventPublisher {
            events: crate::session::event_stream::EventStream::new(),
            fragment_buffer: Arc::new(Mutex::new(VecDeque::new())),
            in_flight_node_id: Arc::new(Mutex::new(None)),
            tool_status_buffer: Arc::new(Mutex::new(HashMap::new())),
            activity,
            stop_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            session_id: session_id.to_string(),
        }
    }

    #[tokio::test]
    async fn snapshot_tags_in_flight_message_with_preallocated_node_id() {
        let session = crate::persistence::ChatSession::new_empty(
            "s1".to_string(),
            String::new(),
            crate::session::SessionConfig::default(),
            None,
        );
        let instance = SessionInstance::new(session, crate::tools::test_registry());
        let publisher = instance.create_publisher(crate::session::event_stream::EventStream::new());

        // The agent announces the request with the node id the message will
        // be persisted under, then streams fragments.
        let _ = publisher
            .send_event(UiEvent::StreamingStarted {
                request_id: 1,
                node_id: 42,
            })
            .await;
        let _ = publisher.display_fragment(&DisplayFragment::PlainText("hel".to_string()));
        let _ = publisher.display_fragment(&DisplayFragment::PlainText("lo".to_string()));

        // A snapshot taken mid-stream carries the partial message tagged
        // with the pre-allocated node id, so a frontend rendering it stays
        // deduplicatable against the persisted message later.
        let snapshot = instance.build_snapshot(None).unwrap();
        let partial = snapshot.messages.last().expect("partial message present");
        assert_eq!(partial.node_id, Some(42));
        assert_eq!(partial.fragments.len(), 2);

        // Once streaming ends the in-flight state is gone.
        let _ = publisher
            .send_event(UiEvent::StreamingStopped {
                id: 1,
                cancelled: false,
                error: None,
            })
            .await;
        let snapshot = instance.build_snapshot(None).unwrap();
        assert!(snapshot.messages.is_empty());
    }

    #[tokio::test]
    async fn test_streaming_stopped_with_error_prevents_agent_running_state() {
        let activity = activity_with(SessionActivityState::WaitingForResponse);
        let publisher = test_publisher(activity.clone(), "test-session");

        // Simulate StreamingStopped with error
        let _ = publisher
            .send_event(UiEvent::StreamingStopped {
                id: 1,
                cancelled: false,
                error: Some("LLM request failed".to_string()),
            })
            .await;

        // Verify that the activity state is NOT changed to AgentRunning when there's an error
        assert_eq!(activity.get(), SessionActivityState::WaitingForResponse);

        // Now test without error - should transition to AgentRunning
        let activity2 = activity_with(SessionActivityState::WaitingForResponse);
        let publisher2 = test_publisher(activity2.clone(), "test-session-2");

        let _ = publisher2
            .send_event(UiEvent::StreamingStopped {
                id: 2,
                cancelled: false,
                error: None,
            })
            .await;

        // Verify that the activity state IS changed to AgentRunning when there's no error
        assert_eq!(activity2.get(), SessionActivityState::AgentRunning);
    }
}
