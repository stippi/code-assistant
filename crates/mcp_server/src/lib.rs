//! MCP (Model Context Protocol) server mode of code-assistant.
//!
//! Unlike the frontends, this does not drive the agent: it exposes the
//! domain tool registry and project resources directly to external MCP
//! clients over stdio.

mod handler;
mod resources;
mod server;
mod types;

#[cfg(test)]
mod tests;

pub use server::MCPServer;
