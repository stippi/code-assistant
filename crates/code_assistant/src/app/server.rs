use crate::logging::setup_logging;
use crate::mcp::MCPServer;
use anyhow::Result;

pub async fn run(verbose: bool) -> Result<()> {
    // Setup logging based on verbose flag
    setup_logging(if verbose { 1 } else { 0 }, false);

    // Initialize server
    let mut server = MCPServer::new()?;
    server.run().await
}
