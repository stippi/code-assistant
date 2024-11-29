use super::types::*;
use crate::explorer::Explorer;
use crate::types::{CodeExplorer, FileSystemEntryType, Tool as AgentTool};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, error}; // Rename to avoid naming conflict

#[derive(Debug, Serialize, Deserialize)]
pub struct JSONRPCRequest {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct JSONRPCResponse<T> {
    pub jsonrpc: String,
    pub id: RequestId,
    pub result: T,
}

#[derive(Debug, Serialize)]
pub struct JSONRPCError {
    pub jsonrpc: String,
    pub id: RequestId,
    pub error: ErrorObject,
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

pub struct MessageHandler {
    explorer: Box<dyn CodeExplorer>,
}

impl MessageHandler {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            explorer: Box::new(Explorer::new(root_path.clone())),
        }
    }

    async fn handle_tool_call(&self, id: RequestId, params: ToolCallParams) -> Result<String> {
        let result = match params.name.as_str() {
            "read-file" => {
                let path = match params.arguments {
                    Some(args) => {
                        let path_str = args["path"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'path' argument"))?;
                        PathBuf::from(path_str)
                    }
                    None => return Err(anyhow::anyhow!("No arguments provided")),
                };

                // Nutze den vorhandenen Explorer
                match self.explorer.read_file(&path) {
                    Ok(content) => ToolCallResult {
                        content: vec![ToolResultContent::Text { text: content }],
                        is_error: None,
                    },
                    Err(e) => ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!("Error reading file: {}", e),
                        }],
                        is_error: Some(true),
                    },
                }
            }

            "list-files" => {
                let args = params
                    .arguments
                    .ok_or_else(|| anyhow::anyhow!("No arguments provided"))?;
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'path' argument"))?;
                let max_depth = args
                    .get("max_depth")
                    .and_then(|v| v.as_u64())
                    .map(|d| d as usize);

                let path = PathBuf::from(path_str);

                // Nutze die vorhandene list_files Implementierung
                match self.explorer.list_files(&path, max_depth) {
                    Ok(tree_entry) => {
                        // Konvertiere den FileTreeEntry in einen String
                        let result = tree_entry.to_string();
                        ToolCallResult {
                            content: vec![ToolResultContent::Text { text: result }],
                            is_error: None,
                        }
                    }
                    Err(e) => ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!("Error listing files: {}", e),
                        }],
                        is_error: Some(true),
                    },
                }
            }

            _ => return Err(anyhow::anyhow!("Unknown tool: {}", params.name)),
        };

        let response = JSONRPCResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result,
        };

        Ok(serde_json::to_string(&response)?)
    }

    async fn handle_tools_list(&self, id: RequestId) -> Result<String> {
        let response = JSONRPCResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: ListToolsResult {
                tools: vec![
                    Tool {
                        name: "read-file".to_string(),
                        description: Some("Read content of a file from the workspace".to_string()),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Relative path to the file from project root"
                                }
                            },
                            "required": ["path"]
                        }),
                    },
                    Tool {
                        name: "list-files".to_string(),
                        description: Some("List files in a directory".to_string()),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Directory path relative to project root"
                                },
                                "max_depth": {
                                    "type": "integer",
                                    "description": "Maximum directory depth"
                                }
                            },
                            "required": ["path"]
                        }),
                    },
                ],
                next_cursor: None,
            },
        };

        Ok(serde_json::to_string(&response)?)
    }

    async fn handle_prompts_list(&self, id: RequestId) -> Result<String> {
        let response = JSONRPCResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: ListPromptsResult {
                prompts: vec![], // Erstmal eine leere Liste
                next_cursor: None,
            },
        };

        Ok(serde_json::to_string(&response)?)
    }

    pub async fn handle_message(&self, message: &str) -> Result<Option<String>> {
        // Parse the message first
        let message: JSONRPCMessage = match serde_json::from_str(message) {
            Ok(msg) => msg,
            Err(e) => {
                error!("Invalid JSON-RPC message: {}", e);
                return Ok(None);
            }
        };
        match message {
            // Handle requests
            JSONRPCMessage::Request(request) => {
                debug!("Processing request: {:?}", request);
                match (request.method.as_str(), request.id) {
                    // Handle initialize request
                    ("initialize", Some(id)) => {
                        let params: InitializeParams = serde_json::from_value(request.params)?;
                        debug!("Initialize params: {:?}", params);

                        let response = JSONRPCResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: InitializeResult {
                                capabilities: ServerCapabilities {
                                    tools: Some(ToolsCapability {
                                        list_changed: Some(false),
                                    }),
                                    experimental: None,
                                },
                                protocol_version: params.protocol_version,
                                server_info: Implementation {
                                    name: "code-assistant".to_string(),
                                    version: "0.1.0".to_string(),
                                },
                                instructions: Some(
                                    "Code Assistant helps you analyze and modify code.".to_string(),
                                ),
                            },
                        };

                        Ok(Some(serde_json::to_string(&response)?))
                    }

                    // Handle resources/list
                    ("resources/list", Some(id)) => {
                        debug!("Handling resources/list request");
                        let uri = format!("file://{}", self.explorer.root_dir().display());
                        let response = JSONRPCResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: ListResourcesResult {
                                resources: vec![Resource {
                                    name: "Repository".to_string(),
                                    uri,
                                    description: Some(
                                        "The current workspace repository".to_string(),
                                    ),
                                    mime_type: None,
                                }],
                                next_cursor: None,
                            },
                        };
                        Ok(Some(serde_json::to_string(&response)?))
                    }

                    ("prompts/list", Some(id)) => {
                        debug!("Handling prompts/list request");
                        self.handle_prompts_list(id).await.map(Some)
                    }

                    ("tools/list", Some(id)) => {
                        debug!("Handling tools/list request");
                        self.handle_tools_list(id).await.map(Some)
                    }

                    ("tools/call", Some(id)) => {
                        debug!("Handling tools/call request");
                        let params: ToolCallParams = serde_json::from_value(request.params)?;
                        self.handle_tool_call(id, params).await.map(Some)
                    }

                    // Handle notifications (no response needed)
                    (_, None) => {
                        debug!("Received notification: {}", request.method);
                        Ok(None)
                    }

                    // Handle unknown methods
                    (unknown_method, Some(id)) => {
                        let error = JSONRPCError {
                            jsonrpc: "2.0".to_string(),
                            id,
                            error: ErrorObject {
                                code: -32601,
                                message: format!("Method not found: {}", unknown_method),
                                data: None,
                            },
                        };
                        Ok(Some(serde_json::to_string(&error)?))
                    }
                }
            }

            // Handle notifications
            JSONRPCMessage::Notification { method, params, .. } => match method.as_str() {
                "notifications/initialized" => {
                    if let Some(params) = params {
                        debug!("Client initialized with params: {:?}", params);
                    } else {
                        debug!("Client initialized");
                    }
                    Ok(None)
                }
                _ => {
                    debug!("Unknown notification: {}", method);
                    Ok(None)
                }
            },
        }
    }
}
