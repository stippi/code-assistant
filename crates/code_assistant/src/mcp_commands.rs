//! CLI commands for authenticating HTTP MCP servers that require OAuth
//! (MCP's authorization spec — e.g. servers that answer `initialize` with
//! `401 Auth required`).
//!
//! `mcp-login <server>` runs the browser OAuth flow and stores the resulting
//! tokens under `<config_dir>/mcp-oauth/<server>.json`; subsequent agent runs
//! reuse them silently. This mirrors what Cline's "Authenticate" button does.
//! The flow itself lives in `code_assistant_core::tools::mcp_auth` so the
//! settings UI can drive the same login.

use anyhow::Result;
use code_assistant_core::tools::{mcp, mcp_auth};

/// Run the OAuth browser login for the configured HTTP MCP server `server`.
pub async fn run_mcp_login(server: &str) -> Result<()> {
    println!("Starting OAuth login for MCP server '{server}'...");
    mcp_auth::login_mcp_server(server).await?;
    println!();
    println!("Login successful. Tokens stored for MCP server '{server}'.");
    println!("The next agent run will use them automatically.");
    Ok(())
}

/// Remove any stored OAuth tokens for the MCP server `server`.
pub fn run_mcp_logout(server: &str) -> Result<()> {
    if mcp::forget_mcp_oauth_tokens(server)? {
        println!("Logged out. Removed stored OAuth tokens for MCP server '{server}'.");
    } else {
        println!("No stored OAuth tokens for MCP server '{server}'.");
    }
    Ok(())
}
