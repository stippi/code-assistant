use anyhow::Result;
use llm::ContentBlock;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// Agent instances are created on-demand, no need to import
use crate::agent::SubAgentCancellationRegistry;
use crate::persistence::{
    ChatMetadata, ChatSession, FileSessionPersistence, JournalVersion, NodeId,
};
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

/// How UI data carries tool results stored outside the session record
/// (large ones, see `persistence::blobs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredOutputs {
    /// Read them and include their outputs.
    Include,
    /// Leave the outputs of successful runs out
    /// ([`crate::ui::ui_events::ToolResultData::output_deferred`]).
    /// Failures are included.
    Defer,
}

/// What the conversation records about a tool result.
struct RecordedToolResult {
    is_error: bool,
    duration_seconds: Option<f64>,
}

/// Reads the complete UI data of one tool result, without the session
/// instance: the caller can let go of the session manager meanwhile.
pub struct ToolOutputLoader {
    persistence: FileSessionPersistence,
    session_id: String,
    execution: agent_core::SerializedToolExecution,
    tool_registry: Arc<crate::tools::core::ToolRegistry>,
    duration_seconds: Option<f64>,
}

impl ToolOutputLoader {
    pub fn tool_id(&self) -> &str {
        &self.execution.tool_request.id
    }

    pub fn load(mut self) -> Result<crate::ui::ui_events::ToolResultData> {
        self.persistence
            .resolve_tool_results(&self.session_id, std::slice::from_mut(&mut self.execution))?;
        tool_result_data(
            &self.execution,
            self.tool_registry.as_ref(),
            self.duration_seconds,
        )
    }
}

/// The UI data of a tool execution whose result is resolved.
fn tool_result_data(
    serialized_execution: &agent_core::SerializedToolExecution,
    tool_registry: &crate::tools::core::ToolRegistry,
    duration_seconds: Option<f64>,
) -> Result<crate::ui::ui_events::ToolResultData> {
    let execution =
        crate::tools::mcp::deserialize_tool_execution(serialized_execution, tool_registry)?;
    let status = if execution.result.is_success() {
        crate::ui::ToolStatus::Success
    } else {
        crate::ui::ToolStatus::Error
    };
    // Rendering for the UI doesn't deduplicate resources across executions:
    // each result renders on its own, so it can be loaded on its own.
    let output = execution
        .result
        .as_render()
        .render_for_ui(&mut crate::tools::core::ResourcesTracker::new());
    Ok(crate::ui::ui_events::ToolResultData {
        message: Some(execution.result.as_render().status()),
        output: Some(output),
        styled_output: None, // Not available for restored sessions
        duration_seconds,
        // Image data from tools that produce visual output
        images: execution.result.render_images(),
        tool_id: execution.tool_request.id,
        status,
        output_deferred: false,
    })
}

/// Represents a single session instance with its own agent and state
pub struct SessionInstance {
    /// The session data (messages, metadata, etc.), with its tool results
    /// unresolved: results stored outside the record are read when needed
    /// (see [`FileSessionPersistence::load_chat_session_unresolved`]).
    pub session: ChatSession,
    /// The journal version `session` was read at; `None` when `session` may
    /// differ from the journal.
    loaded_version: Option<JournalVersion>,

    // Agent instances are created on-demand and moved into tokio tasks
    // We only track the task handle, not the agent itself
    /// Task handle for the running agent (None if not running)
    pub task_handle: Option<JoinHandle<Result<()>>>,
    /// Owned preparation (LLM construction, MCP trust and registry). Never detached.
    pub(crate) setup_task: Option<JoinHandle<()>>,
    pub(crate) sleep_guard: Option<super::sleep_inhibitor::AgentSleepGuard>,
    pub(crate) cancellation: tools_core::RunCancellation,

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

    /// The open `/new` / `/handoff` target question, if any.
    pub pending_new_context_target: Arc<crate::session::new_context::PendingTargetRequest>,

    /// `ask_question` requests currently awaiting the user's answers.
    pub pending_questions: Arc<crate::session::questions::PendingQuestions>,

    /// The last message a handoff was prepared for while idle, so each
    /// state of the session is prepared at most once.
    pub handoff_prepared_for: Option<crate::persistence::NodeId>,

    /// Cancellation registry for sub-agents running in agent tasks
    pub sub_agent_cancellation_registry: Arc<SubAgentCancellationRegistry>,

    /// Live PTY sessions started by this session's agents (execute_command
    /// session mode). Survives across agent runs; dropping the instance
    /// terminates all remaining sessions.
    pub pty_sessions: Arc<pty_session::PtySessionManager>,

    /// Live browser sessions started by this session's agents (`browser_*`
    /// tools), sub-agents' included. Survives across agent runs so an
    /// authenticated browser can be reused; dropping the instance kills any
    /// remaining browser processes.
    pub browsers: Arc<crate::session::browsers::SessionBrowsers>,

    /// Cancel flags for in-flight blocking (foreground) `execute_command`
    /// invocations, so the UI's terminal-card stop button can interrupt a
    /// foreground command by tool_id (background ones go through
    /// `pty_sessions`).
    pub terminal_interrupts: Arc<crate::tools::TerminalInterrupts>,

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

impl Drop for SessionInstance {
    fn drop(&mut self) {
        self.terminate_agent();
    }
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
            loaded_version: None,
            task_handle: None,
            setup_task: None,
            sleep_guard: None,
            cancellation: tools_core::RunCancellation::default(),
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
            pending_new_context_target: Arc::default(),
            pending_questions: Arc::default(),
            handoff_prepared_for: None,
            sub_agent_cancellation_registry: Arc::new(SubAgentCancellationRegistry::default()),
            pty_sessions: Arc::new(pty_session::PtySessionManager::default()),
            browsers: Arc::default(),
            terminal_interrupts: Arc::new(crate::tools::TerminalInterrupts::default()),
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
    /// Pending permission requests resolve as denied and an open target
    /// question is dropped, so the agent does not stay blocked waiting for
    /// an answer.
    pub fn request_stop(&self) {
        self.cancellation.cancel();
        self.stop_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.pending_permission_requests.deny_all();
        self.pending_new_context_target.cancel();
        self.pending_questions.cancel_all();
    }

    /// Reset per-run state when a new agent starts: clears a previous stop
    /// request, the live tool-status map of the prior run, and any stale
    /// permission requests or target question.
    pub fn begin_agent_run(&mut self) {
        self.cancellation = tools_core::RunCancellation::default();
        self.activity = SessionActivity::default();
        self.stop_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Ok(mut buf) = self.tool_status_buffer.lock() {
            buf.clear();
        }
        self.pending_permission_requests.deny_all();
        self.pending_new_context_target.cancel();
        self.pending_questions.cancel_all();
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
        // Release before stopping: the stopped run resolves its outcome on
        // another thread, and whoever observes that may rely on the locks
        // being gone.
        self.agent_lock = None;
        self.sleep_guard = None;
        self.request_stop();
        if let Some(handle) = self.setup_task.take() {
            handle.abort();
        }
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
            self.clear_fragment_buffer();
        }
        self.set_activity_state(SessionActivityState::Idle);
    }

    /// Get the current context size occupied by the most recent assistant
    /// message. All token categories count: input and cache-write tokens are
    /// both part of the prompt, cache-read tokens were replayed from the cache
    /// into the prompt, and the output tokens become part of the prompt on the
    /// next request.
    #[allow(dead_code)]
    pub fn get_current_context_size(&self) -> u32 {
        // Find the most recent assistant message with usage data
        for message in self.session.get_active_messages().iter().rev() {
            if matches!(message.role, llm::MessageRole::Assistant)
                && let Some(usage) = &message.usage
            {
                return usage
                    .input_tokens
                    .saturating_add(usage.cache_creation_input_tokens)
                    .saturating_add(usage.cache_read_input_tokens)
                    .saturating_add(usage.output_tokens);
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
            if matches!(message.role, llm::MessageRole::Assistant)
                && let Some(usage) = &message.usage
            {
                return usage.clone();
            }
        }
        llm::Usage::zero()
    }

    /// The session-list entry describing the current state of this session.
    pub fn metadata(&self) -> ChatMetadata {
        ChatMetadata {
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
            branch: self.session.config.branch.clone(),
            plan_collapsed: self.session.plan_collapsed,
            is_resumable: self.session.is_resumable(),
        }
    }

    /// Load a stored session, `None` when there is none.
    pub fn load(
        persistence: &FileSessionPersistence,
        session_id: &str,
        tool_registry: Arc<crate::tools::core::ToolRegistry>,
    ) -> Result<Option<Self>> {
        // Taken before reading: a write in between makes the next reload read.
        let version = persistence.journal_version(session_id)?;
        let Some(session) = persistence.load_chat_session_unresolved(session_id)? else {
            return Ok(None);
        };
        let mut instance = Self::new(session, tool_registry);
        instance.loaded_version = version;
        Ok(Some(instance))
    }

    /// Bring the session up to date with persistence, where agents and
    /// other processes store their changes. Reads only when the journal
    /// changed since the last read.
    pub fn reload_from_persistence(
        &mut self,
        persistence: &FileSessionPersistence,
    ) -> anyhow::Result<()> {
        let version = persistence.journal_version(&self.session.id)?;
        if version.is_some() && version == self.loaded_version {
            return Ok(());
        }
        if let Some(session) = persistence.load_chat_session_unresolved(&self.session.id)? {
            debug!("Reloading session {} from persistence", self.session.id);
            self.session = session;
            if let Some(path) = self.session.config.effective_project_path() {
                let _ = self.sandbox_context.register_root(path);
            }
        }
        self.loaded_version = version;
        Ok(())
    }

    /// Create the publisher this session's agent talks to: it records
    /// in-flight state for snapshots and publishes everything session-tagged
    /// to the core→UI broadcast stream.
    ///
    /// `turn_recorder` is the synchronous tee for a controller-started turn
    /// (see [`crate::session::turn`]): it sees the run's events *before* the
    /// lossy broadcast, so a turn outcome never depends on a subscriber
    /// keeping up.
    pub fn create_publisher(
        &self,
        events: crate::session::event_stream::EventStream,
        turn_recorder: Option<Arc<crate::session::turn::TurnRecorder>>,
    ) -> Arc<dyn UserInterface> {
        Arc::new(SessionEventPublisher {
            events,
            fragment_buffer: self.fragment_buffer.clone(),
            in_flight_node_id: self.in_flight_node_id.clone(),
            tool_status_buffer: self.tool_status_buffer.clone(),
            activity: self.activity.clone(),
            stop_requested: self.stop_requested.clone(),
            session_id: self.session.id.clone(),
            turn_recorder,
            pending_questions: self.pending_questions.clone(),
        })
    }

    /// Generate UI events for connecting to this session.
    /// Returns SetMessages event with all session messages including incomplete streaming message.
    ///
    /// If `until_node_id` is `Some(_)`, the transcript is truncated to messages
    /// up to and including that node. This is used to restore the "edit mode"
    /// view (truncated to the branch parent) directly when connecting to a
    /// session whose draft is in edit mode, avoiding a full-then-truncate flash.
    ///
    /// Tool outputs stored outside the session record are deferred (see
    /// [`Self::convert_tool_executions_to_ui_data`]).
    pub fn build_snapshot(
        &self,
        persistence: &FileSessionPersistence,
        until_node_id: Option<crate::persistence::NodeId>,
    ) -> Result<crate::session::SessionSnapshot, anyhow::Error> {
        // Convert session messages to UI data (optionally truncated for edit mode)
        let mut messages =
            self.convert_messages_to_ui_data_until(self.session.config.tool_syntax, until_node_id)?;
        let mut tool_results = self.convert_tool_executions_to_ui_data(persistence)?;

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

        let metadata = self.metadata();

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
            mcp_servers: crate::tools::mcp::session_mcp_servers(
                self.session
                    .config
                    .effective_project_path()
                    .map(|p| p.as_path()),
                &self.session.config.disabled_mcp_servers,
            ),
            pending_permission_requests: self.pending_permission_requests.snapshot(),
            pending_new_context_target: self.pending_new_context_target.snapshot(),
            pending_questions: self.pending_questions.snapshot(),
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
                messages_data.push(MessageData {
                    role: MessageRole::System,
                    fragments: vec![crate::ui::context_divider(message)],
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

            match processor
                .extract_fragments_from_message(&crate::injection::without_injections(message))
            {
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
                messages_data.push(MessageData {
                    role: MessageRole::System,
                    fragments: vec![crate::ui::context_divider(message)],
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

            match processor
                .extract_fragments_from_message(&crate::injection::without_injections(message))
            {
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

    /// Convert the tool executions to UI tool result data for showing the
    /// session. The outputs of successful runs stored outside the session
    /// record are left out ([`crate::ui::ui_events::ToolResultData::output_deferred`]),
    /// so showing a session reads none of them; see [`Self::tool_output_loader`].
    pub fn convert_tool_executions_to_ui_data(
        &self,
        persistence: &FileSessionPersistence,
    ) -> Result<Vec<crate::ui::ui_events::ToolResultData>, anyhow::Error> {
        self.tool_results_ui_data(
            persistence,
            &self.session.tool_executions,
            StoredOutputs::Defer,
        )
    }

    /// Convert the tool executions from index `first` on, outputs included:
    /// what a frontend following the session gets appended.
    pub fn convert_tool_executions_since_to_ui_data(
        &self,
        persistence: &FileSessionPersistence,
        first: usize,
    ) -> Result<Vec<crate::ui::ui_events::ToolResultData>, anyhow::Error> {
        self.tool_results_ui_data(
            persistence,
            self.session
                .tool_executions
                .get(first..)
                .unwrap_or_default(),
            StoredOutputs::Include,
        )
    }

    fn tool_results_ui_data(
        &self,
        persistence: &FileSessionPersistence,
        executions: &[agent_core::SerializedToolExecution],
        stored: StoredOutputs,
    ) -> Result<Vec<crate::ui::ui_events::ToolResultData>, anyhow::Error> {
        let recorded = self.recorded_tool_results();
        let mut tool_results = Vec::new();

        for serialized_execution in executions {
            // A tool that has since disappeared (e.g. a reconfigured MCP
            // server) must not break rendering the session: skip its records.
            if !crate::tools::mcp::execution_renderable(
                serialized_execution,
                self.tool_registry.as_ref(),
            ) {
                tracing::warn!(
                    "Skipping recorded execution of unavailable tool '{}'",
                    serialized_execution.tool_name
                );
                continue;
            }

            let tool_id = &serialized_execution.tool_request.id;
            let recorded = recorded.get(tool_id.as_str());
            let duration_seconds = recorded.and_then(|result| result.duration_seconds);
            let is_stored =
                crate::persistence::is_blob_reference(&serialized_execution.result_json);
            // The conversation records whether the run failed; failures are
            // read, their output explains them.
            if is_stored
                && stored == StoredOutputs::Defer
                && recorded.is_some_and(|result| !result.is_error)
            {
                tool_results.push(crate::ui::ui_events::ToolResultData {
                    tool_id: tool_id.clone(),
                    status: crate::ui::ToolStatus::Success,
                    message: None,
                    output: None,
                    styled_output: None,
                    duration_seconds,
                    images: Vec::new(),
                    output_deferred: true,
                });
                continue;
            }

            let mut execution = std::borrow::Cow::Borrowed(serialized_execution);
            if is_stored {
                persistence.resolve_tool_results(
                    &self.session.id,
                    std::slice::from_mut(execution.to_mut()),
                )?;
            }
            tool_results.push(tool_result_data(
                &execution,
                self.tool_registry.as_ref(),
                duration_seconds,
            )?);
        }

        Ok(tool_results)
    }

    /// Prepare reading the complete UI data of a tool result, which
    /// [`StoredOutputs::Defer`] may have left out. `None` when the session
    /// has no execution `tool_id`.
    pub fn tool_output_loader(
        &self,
        persistence: &FileSessionPersistence,
        tool_id: &str,
    ) -> Option<ToolOutputLoader> {
        let execution = self
            .session
            .tool_executions
            .iter()
            .find(|execution| execution.tool_request.id == tool_id)?;
        Some(ToolOutputLoader {
            persistence: persistence.clone(),
            session_id: self.session.id.clone(),
            execution: execution.clone(),
            tool_registry: self.tool_registry.clone(),
            duration_seconds: self
                .recorded_tool_results()
                .get(tool_id)
                .and_then(|result| result.duration_seconds),
        })
    }

    /// What the conversation records about each tool result, by tool use ID:
    /// whether it failed and how long the run took (from the `ToolResult`
    /// block timestamps, stable across restores).
    fn recorded_tool_results(&self) -> HashMap<&str, RecordedToolResult> {
        // Every branch, so executions outside the active path are covered too
        let messages: Vec<&llm::Message> = if !self.session.message_nodes.is_empty() {
            self.session
                .message_nodes
                .values()
                .map(|node| &node.message)
                .collect()
        } else {
            self.session.messages.iter().collect()
        };

        let mut recorded = HashMap::new();
        for message in messages {
            if let llm::MessageContent::Structured(blocks) = &message.content {
                for block in blocks {
                    if let llm::ContentBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        ..
                    } = block
                    {
                        recorded.insert(
                            tool_use_id.as_str(),
                            RecordedToolResult {
                                is_error: is_error.unwrap_or(false),
                                duration_seconds: block
                                    .duration()
                                    .map(|duration| duration.as_secs_f64()),
                            },
                        );
                    }
                }
            }
        }
        recorded
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
    /// Synchronous tee for a controller-started turn (see
    /// [`crate::session::turn`]); `None` for ordinary user turns.
    turn_recorder: Option<Arc<crate::session::turn::TurnRecorder>>,
    /// Open `ask_question` requests (see [`SessionInstance::pending_questions`]).
    pending_questions: Arc<crate::session::questions::PendingQuestions>,
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
        if let Some(recorder) = &self.turn_recorder {
            recorder.observe(&event);
        }
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
                            output_deferred: false,
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
        if let Some(recorder) = &self.turn_recorder {
            recorder.observe_fragment(fragment);
        }
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
            | DisplayFragment::ContextDivider { .. } => true,
            DisplayFragment::ToolEnd { .. }
            | DisplayFragment::ToolTerminal { .. }
            | DisplayFragment::ToolTerminalExited { .. }
            | DisplayFragment::ReasoningComplete
            | DisplayFragment::HiddenToolCompleted => false,
        };

        if has_visible_content {
            self.publish_activity_change(self.activity.on_visible_output());
        }

        Ok(())
    }

    fn stream_terminal_output(&self, tool_id: &str, bytes: &[u8]) {
        // Publish straight to the broadcast stream, bypassing the in-flight
        // fragment buffer: a background process streams for its whole life,
        // and buffering that (unbounded, replayed in every snapshot) would
        // be wrong. Frontends rebuild the card from the persisted result on
        // reconnect; live colored output is best-effort on the stream.
        self.events.publish(
            Some(self.session_id.clone()),
            crate::session::event_stream::EventPayload::Fragment(
                DisplayFragment::ToolTerminalOutput {
                    tool_id: tool_id.to_string(),
                    bytes: bytes.to_vec(),
                },
            ),
        );
    }

    fn stream_terminal_exit(&self, tool_id: &str, exit_code: Option<i32>) {
        // Direct publish, bypassing the in-flight fragment buffer — same
        // rationale as stream_terminal_output above.
        self.events.publish(
            Some(self.session_id.clone()),
            crate::session::event_stream::EventPayload::Fragment(
                DisplayFragment::ToolTerminalExited {
                    tool_id: tool_id.to_string(),
                    exit_code,
                },
            ),
        );
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

    async fn ask_questions(
        &self,
        request: crate::session::questions::UserQuestionRequest,
    ) -> Result<crate::session::questions::QuestionOutcome, UIError> {
        use crate::session::questions::QuestionOutcome;

        let request_id = request.request_id.clone();
        let rx = self.pending_questions.insert(request.clone());
        self.events
            .publish_ui(&self.session_id, UiEvent::RequestUserQuestions { request });
        // Settles the request on every exit, including a caller that drops
        // this future because its run was cancelled.
        let _settled = QuestionGuard {
            publisher: self,
            request_id,
        };
        // A dropped responder (stop request, new agent run) counts as cancelled.
        Ok(rx.await.unwrap_or(QuestionOutcome::Cancelled))
    }
}

/// Removes the pending question entry (so a late answer is a no-op) and
/// tells every view the request is settled, whichever way the wait ended.
struct QuestionGuard<'a> {
    publisher: &'a SessionEventPublisher,
    request_id: String,
}

impl Drop for QuestionGuard<'_> {
    fn drop(&mut self) {
        self.publisher.pending_questions.resolve(
            &self.request_id,
            crate::session::questions::QuestionOutcome::Cancelled,
        );
        self.publisher.events.publish_ui(
            &self.publisher.session_id,
            UiEvent::UserQuestionsResolved {
                request_id: self.request_id.clone(),
            },
        );
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
            turn_recorder: None,
            pending_questions: Arc::default(),
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
        let dir = tempfile::tempdir().unwrap();
        let persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let publisher =
            instance.create_publisher(crate::session::event_stream::EventStream::new(), None);

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
        let snapshot = instance.build_snapshot(&persistence, None).unwrap();
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
        let snapshot = instance.build_snapshot(&persistence, None).unwrap();
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
