//! ACP (Agent Client Protocol) frontend of code-assistant.
//!
//! Exposes the agent to ACP clients such as Zed: `ACPAgentImpl` implements
//! the protocol's `Agent` trait, `ACPUserUI` implements the domain
//! `UserInterface` by translating `UiEvent`s into session notifications, and
//! the explorer/terminal workers proxy filesystem access and command
//! execution through the connected client. `run` wires it all to stdio.

mod agent;
mod app;
pub mod error_handling;
mod explorer;
pub mod permissions;
mod terminal_executor;
mod types;
mod ui;

pub use agent::{set_acp_client_connection, ACPAgentImpl};
pub use app::run;
pub use explorer::{register_fs_worker, AcpProjectManager};
pub use permissions::AcpPermissionMediator;
pub use terminal_executor::{register_terminal_worker, ACPTerminalCommandExecutor};
pub use ui::ACPUserUI;
