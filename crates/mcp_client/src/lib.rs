//! MCP client mode: connect to configured MCP servers (stdio transport) and
//! register each offered MCP tool as a regular [`tools_core::ToolRegistry`]
//! tool. MCP stays a registry *source*, never an architecture — everything
//! downstream of the registry (dialects, scoping, the agent loop, permission
//! checks) keeps working unchanged.
//!
//! Built on the official Rust MCP SDK (`rmcp`).

pub mod client;
pub mod config;
pub mod naming;
pub mod output;
pub mod registry;
pub mod tool;

#[cfg(test)]
mod tests;

pub use client::McpServerConnection;
pub use config::{McpServerConfig, McpServersConfig};
pub use registry::{
    discover_tools, register_mcp_tools, DiscoveredTool, McpServerStatus, MCP_CAPABILITY,
};
pub use tool::McpTool;
