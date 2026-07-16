//! Code-assistant-specific behavior, plugged into the agent loop through the
//! hook traits in `agent_core::hooks`.

mod app_state;
mod compaction;
mod name_session;
mod plan;
mod recovery;
mod skill_snapshot;
mod sub_agent;
mod system_prompt;
mod tool_services;

pub use app_state::AgentAppState;
pub use compaction::TokenRatioCompaction;
pub use name_session::{NameSessionInterceptor, NameSessionReminderHook};
pub use plan::PlanSnapshotHook;

pub use recovery::DefaultRecovery;
pub use skill_snapshot::SkillSnapshotHook;
pub use sub_agent::SpawnAgentParallelPolicy;
pub use system_prompt::CodeAssistantSystemPrompt;
pub use tool_services::CodeAssistantToolServices;

use agent_core::hooks::HookRegistry;

/// The hook set code-assistant runs its agents with.
pub fn default_hooks() -> HookRegistry {
    HookRegistry {
        interceptors: vec![
            Box::new(NameSessionInterceptor),
            Box::new(PlanSnapshotHook),
            Box::new(SkillSnapshotHook),
        ],
        iteration_hooks: vec![Box::new(NameSessionReminderHook)],
        observers: vec![],
        dispatch: Box::new(SpawnAgentParallelPolicy),
        compaction: Box::new(TokenRatioCompaction::new(0.8)),
        recovery: Box::new(DefaultRecovery),
        system_prompt: Box::new(CodeAssistantSystemPrompt::new()),
    }
}
