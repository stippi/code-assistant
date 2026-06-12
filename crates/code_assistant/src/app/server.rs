use crate::logging::setup_logging;
use anyhow::Result;
use mcp_server::MCPServer;

pub async fn run(verbose: bool) -> Result<()> {
    // Setup logging based on verbose flag
    setup_logging(if verbose { 1 } else { 0 }, false);

    // Initialize server
    let mut server = MCPServer::new()?;
    server.run().await
}
