use anyhow::Result;
use serde_json::Value;
use tracing::{debug, error};

use super::types::*;

pub struct MessageHandler {
    // TODO: Add fields for tools, explorer etc.
}

impl MessageHandler {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn handle_message(&self, message: &str) -> Result<Option<String>> {
        // Parse JSON-RPC request
        let request: JSONRPCRequest = match serde_json::from_str(message) {
            Ok(req) => req,
            Err(e) => {
                error!("Invalid JSON-RPC request: {}", e);
                return Ok(Some(serde_json::to_string(&JSONRPCError {
                    jsonrpc: "2.0".to_string(),
                    id: RequestId::Number(0),
                    error: ErrorObject {
                        code: -32700,
                        message: "Parse error".to_string(),
                        data: None,
                    },
                })?));
            }
        };

        debug!("Received request: {:?}", request);

        // Handle different methods
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(&request.params).await?,
            "tools/list" => self.handle_list_tools(&request.params).await?,
            _ => {
                return Ok(Some(serde_json::to_string(&JSONRPCError {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    error: ErrorObject {
                        code: -32601,
                        message: "Method not found".to_string(),
                        data: None,
                    },
                })?))
            }
        };

        // Create successful response
        Ok(Some(serde_json::to_string(&JSONRPCResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result,
        })?))
    }

    async fn handle_initialize(&self, params: &Value) -> Result<Value> {
        let _params: InitializeParams = serde_json::from_value(params.clone())?;

        // For now, return minimal server capabilities
        Ok(serde_json::json!({
            "serverInfo": {
                "name": "code-assistant",
                "version": "0.1.0"
            },
            "capabilities": {
                "tools": {
                    "listChanged": false
                }
            },
            "protocolVersion": "0.1.0"
        }))
    }

    async fn handle_list_tools(&self, _params: &Value) -> Result<Value> {
        // Return list of available tools
        Ok(serde_json::json!({
            "tools": [
                {
                    "name": "read-file",
                    "description": "Read content of a file",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the file from project root"
                            }
                        },
                        "required": ["path"]
                    }
                },
                {
                    "name": "list-files",
                    "description": "List contents of a directory",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Directory path"
                            },
                            "max_depth": {
                                "type": "integer",
                                "description": "Maximum directory depth"
                            }
                        },
                        "required": ["path"]
                    }
                }
                // TODO: Add more tools
            ]
        }))
    }
}
