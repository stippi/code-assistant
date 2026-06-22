//! ACP (Agent Client Protocol) frontend of code-assistant.
//!
//! Built against the Role/Component based `agent-client-protocol` SDK, pinned to
//! `0.14.0` to match the version Zed ships.
//!
//! Key design points:
//! - The agent is wired with `Agent.builder().on_receive_request(...)` handlers
//!   that run on the connection's dispatch loop (there is no `Agent` trait to
//!   implement in this SDK).
//! - Outgoing client calls (filesystem, terminal, permission) go through
//!   `ConnectionTo<Client>::send_request(...)` directly; the connection is
//!   `Send`, so the forwarder, watcher and per-prompt agent tasks are ordinary
//!   `tokio` tasks (no `LocalSet`/`spawn_local`).
//! - Model selection is exposed via the generic session **config options**
//!   mechanism (`SessionConfigOption` with `category = Model`).
//! - Context-window usage is reported via `SessionUpdate::UsageUpdate` and
//!   session titles via `SessionUpdate::SessionInfoUpdate`.

mod agent;
mod app;
pub mod error_handling;
mod explorer;
pub mod permissions;
mod terminal_executor;
mod types;
mod ui;

pub use agent::AgentState;
pub use app::run;
pub use explorer::AcpProjectManager;
pub use permissions::AcpPermissionMediator;
pub use terminal_executor::ACPTerminalCommandExecutor;
pub use ui::ACPUserUI;

/// Convenience alias for the agent-side connection used everywhere in this
/// crate: a connection whose counterpart is the ACP client.
pub type ClientConn = agent_client_protocol::ConnectionTo<agent_client_protocol::Client>;
