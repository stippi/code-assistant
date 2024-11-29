use crate::mcp::handler::MessageHandler;
use anyhow::Result;
use tokio::io::stdin;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct MCPServer {
    handler: MessageHandler,
}

impl MCPServer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            handler: MessageHandler::new(),
        })
    }

    pub async fn run(&self) -> Result<()> {
        eprintln!("Starting MCP server using stdio transport");

        let stdin = stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);

        // Read lines from stdin
        let mut line = String::new();
        while let Ok(n) = reader.read_line(&mut line).await {
            if n == 0 {
                break; // EOF
            }

            eprintln!("Received message: {}", line);

            // Process the message
            match self.handler.handle_message(&line).await {
                Ok(Some(response)) => {
                    // Write response to stdout
                    stdout.write_all(response.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                }
                Ok(None) => (),
                Err(e) => eprintln!("Error handling message: {}", e),
            }

            line.clear();
        }

        eprintln!("MCP server shutting down");
        Ok(())
    }
}
