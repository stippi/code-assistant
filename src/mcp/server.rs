use crate::mcp::handler::MessageHandler;
use crate::mcp::resources::ResourceManager;
use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{stdin, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error};

pub struct MCPServer {
    handler: MessageHandler,
    resources: ResourceManager,
}

impl MCPServer {
    pub fn new(root_path: PathBuf) -> Result<Self> {
        let resources = ResourceManager::new();
        let handler = MessageHandler::new(root_path, resources.clone())?;

        Ok(Self { handler, resources })
    }

    pub async fn run(&self) -> Result<()> {
        debug!("Starting MCP server using stdio transport");

        let stdin = stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);

        // Set up the initial file tree resource
        if let Ok(tree) = self.handler.create_initial_tree().await {
            self.resources.update_file_tree(tree).await;
        }

        let mut line = String::new();
        while let Ok(n) = reader.read_line(&mut line).await {
            if n == 0 {
                break; // EOF
            }

            let trimmed = line.trim();
            debug!("Received message: {}", trimmed);

            // Process the message
            match self.handler.handle_message(trimmed).await {
                Ok(Some(response)) => {
                    debug!("Sending response: {}", response);
                    // Make sure to write the complete response followed by a newline
                    stdout.write_all(response.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                }
                Ok(None) => {
                    debug!("No response required (notification)");
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
