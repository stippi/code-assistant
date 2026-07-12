//! Domain layer of code-assistant.
//!
//! Sits on top of the generic `agent_core`/`tools_core` crates and owns the
//! application concepts: sessions, persistence, the concrete tools, the
//! XML/Caret tool dialects, the agent-loop plugins, and the domain
//! `UiEvent` vocabulary. The frontends (GPUI, terminal) and the wiring
//! binary consume this crate; nothing in here depends on a frontend.

// Re-exported so external implementors of `config::ProjectManager` — whose
// `get_explorer_for_project` returns a `fs_explorer::CodeExplorer` — can name
// the explorer types without wiring up a separate dependency on the crate.
pub use fs_explorer;

pub mod agent;
pub mod config;
pub mod config_dir;
pub mod persistence;
pub mod plugins;
pub mod session;
pub mod skills;
pub mod tool_dialects;
pub mod tools;
pub mod types;
pub mod ui;
pub mod utils;

// Mock building blocks (LLM provider, UI, project manager, tool fixtures).
// Compiled for our own tests and, behind the `test-utils` feature, for
// dependent crates' tests.
#[cfg(any(test, feature = "test-utils"))]
pub mod mocks;

#[cfg(test)]
mod tests;
