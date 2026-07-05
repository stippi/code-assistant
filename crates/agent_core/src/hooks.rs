//! Extension points for the agent loop.
//!
//! The loop itself stays application-neutral; application-specific behavior
//! (session naming, plan snapshots, parallel dispatch, compaction
//! thresholds, error recovery, system prompts, per-invocation tool services)
//! is provided through these traits, collected in a [`HookRegistry`].
//! Application state travels type-erased through `extensions` slots — the
//! same dyn-Any approach `ToolContext` uses.

use crate::dialect::ToolDialect;
use crate::tree::{ConversationPath, MessageNode, NodeId};
use crate::types::{ToolExecution, ToolRequest};
use anyhow::Result;
use llm::Message;
use std::any::Any;
use std::collections::BTreeMap;
use std::time::Duration;
use tools_core::ToolRegistry;

/// View of the agent state that hooks may read and act on.
pub struct LoopCtx<'a> {
    pub tool_executions: &'a mut Vec<ToolExecution>,
    pub message_nodes: &'a mut BTreeMap<NodeId, MessageNode>,
    pub active_path: &'a ConversationPath,
    /// The session this agent runs, `None` while no session is assigned yet.
    /// Lets shared hook state (built once per process) be keyed per session —
    /// same role `PromptCtx::session_id` plays for system-prompt providers.
    pub session_id: Option<&'a str>,
    /// The agent's tool registry (e.g. for capability checks).
    pub registry: &'a ToolRegistry,
    /// Application-specific loop state. Hooks downcast this to the concrete
    /// type their application installed.
    pub extensions: &'a mut (dyn Any + Send),
}

/// Intercepts tool requests that the application handles itself instead of
/// dispatching them to the registry, and observes successful executions.
pub trait ToolInterceptor: Send + Sync {
    /// Returns `Some(result)` when the request was handled here. Intercepted
    /// tools do not appear in the UI.
    fn try_intercept(&self, _request: &ToolRequest, _ctx: &mut LoopCtx) -> Option<Result<bool>> {
        None
    }

    /// Invoked after any tool executed successfully (standard path included).
    fn after_tool_success(&self, _request: &ToolRequest, _ctx: &mut LoopCtx) {}
}

/// Observes every message as it is appended to the conversation history —
/// user input, assistant responses and tool results alike. For side channels
/// such as transcript mirroring, external memory sync or metrics: observers
/// cannot veto or modify the message, must not block, and any internal
/// failure must stay internal (log, don't panic) — the loop does not inspect
/// an outcome.
///
/// Contract: each message is announced exactly once, when it first enters a
/// conversation. Restoring a persisted conversation does not re-notify.
/// Embedders that insert messages outside the loop (e.g. a session manager
/// accepting user input while no agent runs) must notify observers at that
/// insertion point themselves.
pub trait MessageObserver: Send + Sync {
    /// `session_id` is `None` while the agent has no session assigned yet.
    fn on_message(&self, session_id: Option<&str>, message: &Message);
}

/// Participates in each loop iteration.
pub trait IterationHook: Send + Sync {
    /// Adjust the rendered messages right before they are sent to the LLM
    /// (e.g. to inject system reminders).
    fn shape_request(&self, _messages: &mut Vec<Message>, _ctx: &LoopCtx) -> Result<()> {
        Ok(())
    }
}

/// Decides which tool requests of a turn may execute concurrently.
pub trait ToolDispatchPolicy: Send + Sync {
    /// Indices of the requests that may execute concurrently with each other.
    fn parallel_indices(&self, requests: &[ToolRequest]) -> Vec<usize>;
}

/// Builds the application extension carried by `ToolContext` for each tool
/// invocation, and takes it back afterwards (state such as the plan may move
/// in for the duration of an invocation).
pub trait ToolServicesProvider: Send + Sync {
    /// Per-invocation services for the sequential path. `loop_ext` is the
    /// agent's extension state (see [`LoopCtx::extensions`]).
    fn begin(&self, loop_ext: &mut (dyn Any + Send), tool_id: &str) -> Box<dyn Any + Send>;

    /// Returns the services after the invocation completed.
    fn end(&self, loop_ext: &mut (dyn Any + Send), services: Box<dyn Any + Send>);

    /// Services for a detached invocation (parallel tool execution), which
    /// runs without access to the loop state.
    fn detached(&self, tool_id: &str) -> Box<dyn Any + Send>;
}

/// Conversation measurements a [`CompactionPolicy`] bases its decision on.
pub struct ContextSnapshot {
    /// Tokens used by the last assistant turn relative to the model's context
    /// window, when known.
    pub usage_ratio: Option<f32>,
}

/// Decides when the conversation is compacted and with which prompt.
pub trait CompactionPolicy: Send + Sync {
    /// The model's context window size, when known. The loop divides the
    /// last turn's token usage by this to build the [`ContextSnapshot`].
    fn context_limit(&self, extensions: &(dyn Any + Send)) -> Result<Option<u32>>;
    fn should_compact(&self, snapshot: &ContextSnapshot) -> bool;
    fn compaction_prompt(&self) -> &str;

    /// Optional text appended to the freshly generated compaction summary
    /// message. Lets the application carry application-specific state across
    /// the compaction boundary — e.g. a reminder of which skills were loaded,
    /// whose tool results the compaction just dropped. `extensions` is the
    /// agent's loop state (see [`LoopCtx::extensions`]).
    fn post_compaction_summary_addendum(&self, _extensions: &(dyn Any + Send)) -> Option<String> {
        None
    }
}

/// How the agent recovers from a failed LLM request.
pub enum RecoveryAction {
    /// Shrink the conversation (oversized prompt) and retry.
    ReduceContext,
    /// Transient streaming failure; retry the request after a delay.
    RetryStream {
        delay: Duration,
        attempt: u32,
        max_attempts: u32,
    },
    /// Not recoverable; propagate the error.
    Fail,
}

/// Classifies failed LLM requests into recovery actions.
pub trait RecoveryPolicy: Send + Sync {
    /// `completed_retries` counts the retries already performed for the
    /// current request.
    fn classify(&self, error: &anyhow::Error, completed_retries: u32) -> RecoveryAction;
}

/// Inputs for building the system prompt. The result is cached per model
/// hint by the agent.
pub struct PromptCtx<'a> {
    /// The dialect the agent speaks; providers ask it for the format and
    /// tool documentation sections.
    pub dialect: &'a dyn ToolDialect,
    pub model_hint: Option<&'a str>,
    /// The session the agent runs for, when known — lets providers key
    /// prompt content to a session (e.g. serve a snapshot that stays stable
    /// for the session's lifetime), mirroring what observers get on
    /// [`MessageObserver::on_message`].
    pub session_id: Option<&'a str>,
    /// The agent's tool registry, for rendering tool documentation.
    pub registry: &'a ToolRegistry,
    /// Application-specific state (projects, scope, …); see
    /// [`LoopCtx::extensions`].
    pub extensions: &'a (dyn Any + Send),
}

/// Builds the system prompt.
pub trait SystemPromptProvider: Send + Sync {
    fn build(&self, ctx: &PromptCtx) -> String;
}

/// All hooks an agent instance runs with, set at construction time.
pub struct HookRegistry {
    pub interceptors: Vec<Box<dyn ToolInterceptor>>,
    pub iteration_hooks: Vec<Box<dyn IterationHook>>,
    pub observers: Vec<Box<dyn MessageObserver>>,
    pub dispatch: Box<dyn ToolDispatchPolicy>,
    pub compaction: Box<dyn CompactionPolicy>,
    pub recovery: Box<dyn RecoveryPolicy>,
    pub system_prompt: Box<dyn SystemPromptProvider>,
}

/// Builds a fresh [`HookRegistry`] per agent instance. Embedders install one
/// where agents are created repeatedly (e.g. a session manager) to customize
/// the hook set without the wiring layer knowing about agent construction.
pub type HookRegistryFactory = std::sync::Arc<dyn Fn() -> HookRegistry + Send + Sync>;
