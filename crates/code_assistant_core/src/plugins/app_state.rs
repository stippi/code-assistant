//! The application state code-assistant rides on the agent loop.
//!
//! The loop context exposes this bundle type-erased (`LoopCtx::extensions`,
//! `PromptCtx::extensions`) — the same dyn-Any approach that `ToolServices`
//! uses on `ToolContext`. The plugins downcast via
//! [`AgentAppState::of`] / [`AgentAppState::of_ref`].

use crate::persistence::SessionModelConfig;
use crate::session::SessionConfig;
use crate::tools::core::ToolScope;
use crate::types::PlanState;
use std::any::Any;
use std::collections::HashMap;

pub struct AgentAppState {
    /// The actual session name (empty if not named yet).
    pub session_name: String,
    /// Whether to inject naming reminders (disabled for tests).
    pub naming_reminders_enabled: bool,
    /// State of the `update_plan` tool.
    pub plan: PlanState,
    /// Names of skills activated in this session, in activation order. Used to
    /// render the "Active skills" section and persisted with the session.
    pub active_skills: Vec<String>,
    /// Which tool selection the agent runs with.
    pub tool_scope: ToolScope,
    /// Static configuration stored with the session.
    pub session_config: SessionConfig,
    /// Model configuration associated with the session.
    pub model_config: Option<SessionModelConfig>,
    /// Override for the model's context window (primarily used in tests).
    pub context_limit_override: Option<u32>,
    /// File trees per project (used in the system prompt).
    pub file_trees: HashMap<String, String>,
    /// Available project names (used in the system prompt).
    pub available_projects: Vec<String>,
}

impl AgentAppState {
    pub fn new(session_config: SessionConfig) -> Self {
        let tool_scope = if session_config.use_diff_blocks {
            ToolScope::AgentWithDiffBlocks
        } else {
            ToolScope::Agent
        };
        Self {
            session_name: String::new(),

            naming_reminders_enabled: true,
            plan: PlanState::default(),
            active_skills: Vec::new(),
            tool_scope,
            session_config,
            model_config: None,
            context_limit_override: None,
            file_trees: HashMap::new(),
            available_projects: Vec::new(),
        }
    }

    /// The bundle behind a loop extension slot. Panics when absent — all
    /// code-assistant agents install it.
    pub fn of(ext: &mut (dyn Any + Send)) -> &mut Self {
        ext.downcast_mut()
            .expect("agent loop is missing code-assistant's AgentAppState")
    }

    /// Shared-access variant of [`Self::of`].
    pub fn of_ref(ext: &(dyn Any + Send)) -> &Self {
        ext.downcast_ref()
            .expect("agent loop is missing code-assistant's AgentAppState")
    }
}
