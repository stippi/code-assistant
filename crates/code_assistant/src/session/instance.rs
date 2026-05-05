use anyhow::Result;
use llm::{ContentBlock, Message};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// Agent instances are created on-demand, no need to import
use crate::agent::SubAgentCancellationRegistry;
use crate::persistence::{ChatMetadata, ChatSession, NodeId};
use crate::ui::gpui::elements::MessageRole;
use crate::ui::streaming::create_stream_processor;
use crate::ui::ui_events::{MessageData, UiEvent};
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

    /// Buffer for DisplayFragments from the current streaming message
    /// This allows UI to connect mid-streaming and see buffered content
    pub fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,

    /// Buffer for `UpdateToolStatus` events received while the session is
    /// disconnected.  Shared with the session's [`ProxyUI`] which writes into
    /// it; read (and drained) by [`generate_session_connect_events`] on
    /// reconnect.  Only the latest status per tool-id is kept.
    pub tool_status_buffer: Arc<Mutex<ToolStatusBuffer>>,

    /// Whether this session is currently connected to the UI
    pub is_ui_connected: Arc<Mutex<bool>>,

    /// Current activity state of this session
    pub activity_state: Arc<Mutex<SessionActivityState>>,

    /// Pending user message (structured content blocks) that will be processed by the next agent iteration
    pub pending_message: Arc<Mutex<Option<Vec<ContentBlock>>>>,

    /// Tracks sandbox-approved roots for this session
    pub sandbox_context: Arc<SandboxContext>,

    /// Cancellation registry for sub-agents running in agent tasks
    pub sub_agent_cancellation_registry: Arc<SubAgentCancellationRegistry>,

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
}

impl SessionInstance {
    /// Create a new session instance
    pub fn new(session: ChatSession) -> Self {
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
            is_ui_connected: Arc::new(Mutex::new(false)),
            activity_state: Arc::new(Mutex::new(SessionActivityState::Idle)),
            pending_message: Arc::new(Mutex::new(None)),
            sandbox_context,
            sub_agent_cancellation_registry: Arc::new(SubAgentCancellationRegistry::default()),
            agent_lock: None,
            last_ui_synced_path: initial_path,
            last_ui_synced_tool_count: initial_tool_count,
        }
    }

    /// Cancel a running sub-agent by its tool ID
    /// Returns true if a sub-agent was found and cancelled, false otherwise
    pub fn cancel_sub_agent(&self, tool_id: &str) -> bool {
        self.sub_agent_cancellation_registry.cancel(tool_id)
    }

    /// Get the current activity state
    pub fn get_activity_state(&self) -> SessionActivityState {
        self.activity_state.lock().unwrap().clone()
    }

    /// Set the activity state
    pub fn set_activity_state(&self, state: SessionActivityState) {
        *self.activity_state.lock().unwrap() = state;
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

    /// Set UI active state for this session
    pub fn set_ui_connected(&mut self, connected: bool) {
        if let Ok(mut ui_connected) = self.is_ui_connected.lock() {
            *ui_connected = connected;
        }
    }

    /// Create a ProxyUI for this session that handles fragment buffering
    pub fn create_proxy_ui(&self, real_ui: Arc<dyn UserInterface>) -> Arc<dyn UserInterface> {
        Arc::new(ProxyUI::new(
            real_ui,
            self.fragment_buffer.clone(),
            self.tool_status_buffer.clone(),
            self.is_ui_connected.clone(),
            self.activity_state.clone(),
            self.session.id.clone(),
        ))
    }

    /// Generate UI events for connecting to this session.
    /// Returns SetMessages event with all session messages including incomplete streaming message.
    pub fn generate_session_connect_events(&self) -> Result<Vec<UiEvent>, anyhow::Error> {
        let mut events = Vec::new();

        // Convert session messages to UI data
        let mut messages_data =
            self.convert_messages_to_ui_data(self.session.config.tool_syntax)?;
        let mut tool_results = self.convert_tool_executions_to_ui_data()?;

        // Drain any UpdateToolStatus events that arrived while we were
        // disconnected (e.g. from running sub-agents).  Only inject entries
        // that don't already have a persisted result — the persisted result
        // is authoritative once it exists.
        if let Ok(mut buf) = self.tool_status_buffer.lock() {
            for (_tool_id, result_data) in buf.drain() {
                if !tool_results
                    .iter()
                    .any(|r| r.tool_id == result_data.tool_id)
                {
                    tool_results.push(result_data);
                }
            }
        }

        // If currently streaming, add incomplete message as additional MessageData
        let buffered_fragments = self.get_buffered_fragments(false); // Don't clear buffer
        if !buffered_fragments.is_empty() {
            // Create incomplete assistant message from buffered fragments
            let incomplete_message = MessageData {
                role: crate::ui::gpui::elements::MessageRole::Assistant,
                fragments: buffered_fragments,
                node_id: None,     // Streaming message doesn't have a node yet
                branch_info: None, // No branch info for incomplete message
            };
            messages_data.push(incomplete_message);
        }

        events.push(UiEvent::SetMessages {
            messages: messages_data,
            session_id: Some(self.session.id.clone()),
            tool_results,
        });

        events.push(UiEvent::UpdatePlan {
            plan: self.session.plan.clone(),
        });

        events.push(UiEvent::UpdateSessionActivityState {
            session_id: self.session.id.clone(),
            activity_state: self.get_activity_state(),
        });

        // If the session is in an errored state, emit a DisplayError so the
        // error banner is shown when the user switches to this session.
        if let SessionActivityState::Errored { message } = self.get_activity_state() {
            events.push(UiEvent::DisplayError { message });
        }

        // Add session metadata to ensure UI has the session info including initial_project

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
        };

        events.push(UiEvent::UpdateSessionMetadata { metadata });

        if let Ok(pending) = self.pending_message.lock() {
            events.push(UiEvent::UpdatePendingMessage {
                message: pending
                    .as_ref()
                    .map(|blocks| crate::utils::content::text_summary_from_blocks(blocks)),
            });
        }

        Ok(events)
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
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let dummy_ui: std::sync::Arc<dyn crate::ui::UserInterface> = std::sync::Arc::new(DummyUI);
        let mut processor = create_stream_processor(tool_syntax, dummy_ui, 0);

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
                    role: MessageRole::User,
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
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let dummy_ui: std::sync::Arc<dyn crate::ui::UserInterface> = std::sync::Arc::new(DummyUI);
        let mut processor = create_stream_processor(tool_syntax, dummy_ui, 0);

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
                    role: MessageRole::User,
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
            // Deserialize the tool execution
            let execution = serialized_execution.deserialize()?;

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

/// ProxyUI that buffers fragments and conditionally forwards to real UI
struct ProxyUI {
    real_ui: Arc<dyn UserInterface>,
    fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,
    /// Buffers `UpdateToolStatus` events received while disconnected so they
    /// can be replayed on the next session reconnect.
    tool_status_buffer: Arc<Mutex<ToolStatusBuffer>>,
    is_session_connected: Arc<Mutex<bool>>,
    session_activity_state: Arc<Mutex<SessionActivityState>>,
    session_id: String,
}

impl ProxyUI {
    pub fn new(
        real_ui: Arc<dyn UserInterface>,
        fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,
        tool_status_buffer: Arc<Mutex<ToolStatusBuffer>>,
        is_session_connected: Arc<Mutex<bool>>,
        session_activity_state: Arc<Mutex<SessionActivityState>>,
        session_id: String,
    ) -> Self {
        Self {
            real_ui,
            fragment_buffer,
            tool_status_buffer,
            is_session_connected,
            session_activity_state,
            session_id,
        }
    }

    /// Check if this session is currently connected to the real UI
    fn is_connected(&self) -> bool {
        self.is_session_connected
            .lock()
            .map(|active| *active)
            .unwrap_or(false)
    }

    /// Update activity state and broadcast the change to the UI
    fn update_activity_state(&self, new_state: SessionActivityState) {
        // Update our internal state
        if let Ok(mut state) = self.session_activity_state.lock() {
            // Don't allow transitions from terminal states (Idle, Errored) back to other states.
            // Terminal states persist until a new agent is explicitly started.
            if state.is_terminal() && !new_state.is_terminal() {
                debug!(
                    "Ignoring state transition from {:?} to {:?} for session {}",
                    *state, new_state, self.session_id
                );
                return;
            }

            if *state != new_state {
                *state = new_state.clone();

                // Always broadcast activity state changes to UI (regardless of connection status)
                // This ensures the chat sidebar shows current activity for all sessions
                // Send synchronously to avoid race conditions with async task spawning
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let ui = self.real_ui.clone();
                    let session_id = self.session_id.clone();
                    let activity_state = new_state;
                    handle.spawn(async move {
                        let _ = ui
                            .send_event(UiEvent::UpdateSessionActivityState {
                                session_id,
                                activity_state,
                            })
                            .await;
                    });
                }
            }
        }
    }
}

#[async_trait]
impl UserInterface for ProxyUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        // Handle special events that need buffer management and activity state updates
        match &event {
            UiEvent::StreamingStarted { .. } => {
                // Clear fragment buffer at start of new LLM request
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                // Update activity state to waiting for response
                self.update_activity_state(SessionActivityState::WaitingForResponse);
            }
            UiEvent::StreamingStopped {
                cancelled, error, ..
            } => {
                // Clear fragment buffer when LLM request ends - fragments are now part of message history
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                // Only update activity state back to agent running if streaming was not cancelled
                // and there was no error, and the agent hasn't already completed (i.e., state is not already Idle)
                if !cancelled && error.is_none() {
                    let current_state = self
                        .session_activity_state
                        .lock()
                        .map(|s| s.clone())
                        .unwrap_or(SessionActivityState::Idle);
                    if matches!(
                        current_state,
                        SessionActivityState::WaitingForResponse
                            | SessionActivityState::RateLimited { .. }
                    ) {
                        self.update_activity_state(SessionActivityState::AgentRunning);
                    }
                } else if let Some(error_msg) = error {
                    // If there was an error, the agent will terminate, so don't transition to AgentRunning
                    debug!(
                        "StreamingStopped with error for session {}: {}",
                        self.session_id, error_msg
                    );
                    // The agent task will set the state to Idle when it terminates
                }
            }
            UiEvent::RollbackStreaming { .. } => {
                // Clear fragment buffer — the partial content is being discarded before a retry
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
            }
            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                if session_id == &self.session_id {
                    self.update_activity_state(activity_state.clone());
                    return Ok(());
                }
            }
            _ => {}
        }

        if self.is_connected() {
            self.real_ui.send_event(event).await
        } else {
            // Session is disconnected — buffer UpdateToolStatus events so that
            // the latest state per tool can be replayed on reconnect.

            if let UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                styled_output,
                duration_seconds,
                images,
            } = event
            {
                if let Ok(mut buf) = self.tool_status_buffer.lock() {
                    buf.insert(
                        tool_id.clone(),
                        crate::ui::ui_events::ToolResultData {
                            tool_id,
                            status,
                            message,
                            output,
                            styled_output,
                            duration_seconds,
                            images,
                        },
                    );
                }
            }
            Ok(())
        }
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Always buffer fragments
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.push_back(fragment.clone());

            // Keep buffer size reasonable
            while buffer.len() > 1000 {
                buffer.pop_front();
            }
        }

        // First fragment indicates streaming has started - transition from WaitingForResponse
        // But only if the agent is still running (not Idle)
        let current_state = self
            .session_activity_state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(SessionActivityState::Idle);
        if matches!(current_state, SessionActivityState::WaitingForResponse) {
            self.update_activity_state(SessionActivityState::AgentRunning);
        }

        // Only forward to real UI if session is connected
        if self.is_connected() {
            self.real_ui.display_fragment(fragment)
        } else {
            Ok(())
        }
    }

    fn should_streaming_continue(&self) -> bool {
        if self.is_connected() {
            self.real_ui.should_streaming_continue()
        } else {
            true // Don't interrupt streaming if session is not connected
        }
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        // Update session activity state and broadcast
        self.update_activity_state(SessionActivityState::RateLimited { seconds_remaining });

        if self.is_connected() {
            self.real_ui.notify_rate_limit(seconds_remaining);
        }
        // No-op if session not connected
    }

    fn clear_rate_limit(&self) {
        // Update session activity state back to waiting for response
        self.update_activity_state(SessionActivityState::WaitingForResponse);

        if self.is_connected() {
            self.real_ui.clear_rate_limit();
        }
        // No-op if session not connected
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::MockUI;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn test_streaming_stopped_with_error_prevents_agent_running_state() {
        let mock_ui = Arc::new(MockUI::default());
        let fragment_buffer = Arc::new(Mutex::new(VecDeque::new()));
        let is_session_connected = Arc::new(Mutex::new(true));
        let session_activity_state = Arc::new(Mutex::new(SessionActivityState::WaitingForResponse));
        let session_id = "test-session".to_string();

        let proxy_ui = ProxyUI::new(
            mock_ui.clone(),
            fragment_buffer,
            Arc::new(Mutex::new(HashMap::new())),
            is_session_connected,
            session_activity_state.clone(),
            session_id,
        );

        // Simulate StreamingStopped with error
        let _ = proxy_ui
            .send_event(UiEvent::StreamingStopped {
                id: 1,
                cancelled: false,
                error: Some("LLM request failed".to_string()),
            })
            .await;

        // Verify that the activity state is NOT changed to AgentRunning when there's an error
        let final_state = session_activity_state.lock().unwrap().clone();
        assert_eq!(final_state, SessionActivityState::WaitingForResponse);

        // Now test without error - should transition to AgentRunning
        let session_activity_state2 =
            Arc::new(Mutex::new(SessionActivityState::WaitingForResponse));

        let proxy_ui2 = ProxyUI::new(
            mock_ui.clone(),
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(true)),
            session_activity_state2.clone(),
            "test-session-2".to_string(),
        );

        let _ = proxy_ui2
            .send_event(UiEvent::StreamingStopped {
                id: 2,
                cancelled: false,
                error: None,
            })
            .await;

        // Verify that the activity state IS changed to AgentRunning when there's no error
        let final_state2 = session_activity_state2.lock().unwrap().clone();
        assert_eq!(final_state2, SessionActivityState::AgentRunning);
    }
}
