//! Extension points for the agent loop.
//!
//! The loop itself stays application-neutral; behavior specific to
//! code-assistant (session naming, plan snapshots, spawn-agent parallelism,
//! compaction thresholds, error recovery) is provided through these traits,
//! collected in a [`HookRegistry`]. The concrete implementations live in
//! `crate::plugins`.

use crate::agent::dialect::ToolDialect;
use crate::agent::types::ToolExecution;
use crate::persistence::{ConversationPath, MessageNode, NodeId};
use crate::tools::core::ToolScope;
use crate::tools::ToolRequest;
use crate::types::PlanState;
use anyhow::Result;
use llm::Message;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

/// View of the agent state that hooks may read and act on.
pub struct LoopCtx<'a> {
    pub session_name: &'a mut String,
    pub naming_reminders_enabled: bool,
    pub tool_scope: ToolScope,
    pub tool_executions: &'a mut Vec<ToolExecution>,
    pub plan: &'a PlanState,
    pub message_nodes: &'a mut BTreeMap<NodeId, MessageNode>,
    pub active_path: &'a ConversationPath,
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

/// Conversation measurements a [`CompactionPolicy`] bases its decision on.
pub struct ContextSnapshot {
    /// Tokens used by the last assistant turn relative to the model's context
    /// window, when known.
    pub usage_ratio: Option<f32>,
}

/// Decides when the conversation is compacted and with which prompt.
pub trait CompactionPolicy: Send + Sync {
    fn should_compact(&self, snapshot: &ContextSnapshot) -> bool;
    fn compaction_prompt(&self) -> &str;
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
    pub tool_scope: ToolScope,
    pub model_hint: Option<&'a str>,
    pub initial_project: &'a str,
    /// Effective project root (worktree or init path), when configured.
    pub project_root: Option<&'a Path>,
    pub file_trees: &'a HashMap<String, String>,
    pub available_projects: &'a [String],
}

/// Builds the system prompt.
pub trait SystemPromptProvider: Send + Sync {
    fn build(&self, ctx: &PromptCtx) -> String;
}

/// All hooks an agent instance runs with, set at construction time.
pub struct HookRegistry {
    pub interceptors: Vec<Box<dyn ToolInterceptor>>,
    pub iteration_hooks: Vec<Box<dyn IterationHook>>,
    pub dispatch: Box<dyn ToolDispatchPolicy>,
    pub compaction: Box<dyn CompactionPolicy>,
    pub recovery: Box<dyn RecoveryPolicy>,
    pub system_prompt: Box<dyn SystemPromptProvider>,
}
