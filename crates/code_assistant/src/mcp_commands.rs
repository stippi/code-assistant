//! CLI commands for authenticating HTTP MCP servers that require OAuth
//! (MCP's authorization spec — e.g. servers that answer `initialize` with
//! `401 Auth required`).
//!
//! `mcp-login <server>` runs the browser OAuth flow and stores the resulting
//! tokens under `<config_dir>/mcp-oauth/`, per server name and URL;
//! subsequent agent runs reuse them silently. The server may also come from
//! the trusted `.mcp.json` of the project given by `--path`. This mirrors
//! what Cline's "Authenticate" button does.
//! The flow itself lives in `code_assistant_core::tools::mcp_auth` so the
//! settings UI can drive the same login.

use anyhow::Result;
use code_assistant_core::tools::{mcp, mcp_auth};
use std::path::Path;

/// Run the OAuth browser login for the configured HTTP MCP server `server`,
/// from `mcp-servers.json` or the trusted `.mcp.json` in `project_dir`.
pub async fn run_mcp_login(server: &str, project_dir: &Path) -> Result<()> {
    let config = mcp::resolve_project_mcp_server(server, project_dir)?;
    println!("Starting OAuth login for MCP server '{server}'...");
    mcp_auth::login_configured_server(server, &config).await?;
    println!();
    println!("Login successful. Tokens stored for MCP server '{server}'.");
    println!("The next agent run will use them automatically.");
    Ok(())
}

/// Remove every stored OAuth login of the MCP server `server`, whatever URL
/// it was made for.
pub fn run_mcp_logout(server: &str) -> Result<()> {
    match mcp::forget_mcp_oauth_tokens(server)? {
        0 => println!("No stored OAuth tokens for MCP server '{server}'."),
        1 => println!("Logged out. Removed the stored OAuth tokens for MCP server '{server}'."),
        n => println!("Logged out. Removed {n} stored OAuth logins for MCP server '{server}'."),
    }
    Ok(())
}
