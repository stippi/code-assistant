use crate::mcp::handler::MessageHandler;
use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{stdin, AsyncBufReadExt, BufReader};
use tracing::{debug, error, trace};

pub struct MCPServer {
    handler: MessageHandler,
}

impl MCPServer {
    pub fn new(root_path: PathBuf) -> Result<Self> {
        Ok(Self {
            handler: MessageHandler::new(root_path, tokio::io::stdout())?,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        debug!("Starting MCP server using stdio transport");

        let stdin = stdin();
        let mut reader = BufReader::new(stdin);

        let mut line = String::new();
        while let Ok(n) = reader.read_line(&mut line).await {
            if n == 0 {
                break; // EOF
            }

            let trimmed = line.trim();
            trace!("Received message: {}", trimmed);

            // Process the message
            match self.handler.handle_message(trimmed).await {
                Ok(()) => {
                    trace!("Message processed successfully");
                }
                Err(e) => {
                    error!("Error handling message: {}", e);
                }
            }

            line.clear();
        }

        debug!("MCP server shutting down");
        Ok(())
    }
}
