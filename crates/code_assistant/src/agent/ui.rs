//! Compatibility re-export: the agent loop's UI vocabulary lives in
//! `agent_core::ui`. The translation into the application's `UiEvent` is
//! `crate::ui::UiEvent::from_agent`, applied by `crate::ui::AgentUiAdapter`.

pub use agent_core::ui::{AgentActivity, AgentUiEvent};
