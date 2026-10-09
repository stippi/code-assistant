//! Keyless search and fetch through the same MCP tool registry used by the app.
//! Run from the repository root: cargo run -p mcp_client --example parallel_search

use anyhow::{Context, Result, ensure};
use command_executor::DefaultCommandExecutor;
use mcp_client::{McpServersConfig, register_mcp_tools};
use serde_json::{Value, json};
use std::time::Duration;
use tools_core::{registry::ToolRegistry, tool::ToolContext};

async fn call(registry: &ToolRegistry, name: &str, mut arguments: Value) -> Result<()> {
    let tool = registry
        .get(name)
        .with_context(|| format!("missing {name}"))?;
    let executor = DefaultCommandExecutor;
    let mut context = ToolContext {
        command_executor: &executor,
        tool_id: None,
        session_id: None,
        permission_handler: None,
        extensions: None,
    };
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tool.invoke(&mut context, &mut arguments),
    )
    .await
    .context("MCP tool call timed out")??;
    let value = output.to_json()?;
    ensure!(output.is_success(), "{name}: {value}");
    println!("{name}: {}", value["text"].as_str().unwrap_or_default());
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let config: McpServersConfig = serde_json::from_str(include_str!("parallel-search.json"))?;
    let mut registry = ToolRegistry::new();
    // This registration path passes no OAuth credential store. The example
    // config has no Authorization header or environment variable references.
    let statuses = tokio::time::timeout(
        Duration::from_secs(60),
        register_mcp_tools(&mut registry, &config, &["scope:agent"]),
    )
    .await
    .context("MCP registration timed out")?;
    for status in statuses {
        status
            .result
            .map_err(|error| anyhow::anyhow!("{}: {error}", status.server))?;
    }

    call(
        &registry,
        "mcp__parallel__web_search",
        json!({
            "objective": "Find the Rust book's explanation of ownership",
            "search_queries": ["Rust book ownership rules"]
        }),
    )
    .await?;
    // Fetch a known documentation URL to demonstrate reading a specific page.
    call(
        &registry,
        "mcp__parallel__web_fetch",
        json!({
            "urls": ["https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html"],
            "objective": "Explain the three ownership rules in Rust"
        }),
    )
    .await
}
