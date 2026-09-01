//! MCP client mode: connect to configured MCP servers (stdio or HTTP
//! streamable transport) and register each offered MCP tool as a regular
//! [`tools_core::ToolRegistry`] tool. MCP stays a registry *source*, never an
//! architecture — everything downstream of the registry (dialects, scoping,
//! the agent loop, permission checks) keeps working unchanged.
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
pub use config::{
    McpServerConfig, McpServersConfig, McpTransport, parse_local_mcp_json, substitute_variables,
};
pub use naming::is_mcp_tool_name;
pub use output::deserialize_mcp_output;
pub use registry::{
    ConnectionProvider, DiscoveredTool, MCP_CAPABILITY, McpServerStatus, discover_tools,
    register_connection_tools, register_mcp_tools, register_mcp_tools_pooled,
    server_scope_capability,
};
pub use tool::McpTool;
