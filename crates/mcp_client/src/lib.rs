//! MCP client mode: connect to configured MCP servers (stdio transport) and
//! register each offered MCP tool as a regular [`tools_core::ToolRegistry`]
//! tool. MCP stays a registry *source*, never an architecture — everything
//! downstream of the registry (dialects, scoping, the agent loop, permission
//! checks) keeps working unchanged.
//!
//! Built on the official Rust MCP SDK (`rmcp`).

pub mod config;
pub mod naming;

pub use config::{McpServerConfig, McpServersConfig};
