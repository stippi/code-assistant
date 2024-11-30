use super::resources::ResourceManager;
use super::types::*;
use crate::explorer::Explorer;
use crate::mcp::types::ResourceContent;
use crate::types::{CodeExplorer, FileSystemEntryType, FileTreeEntry, Tool as AgentTool};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, error};

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
    resources: ResourceManager,
}

impl MessageHandler {
    pub fn new(root_path: PathBuf, resources: ResourceManager) -> Result<Self> {
        Ok(Self {
            explorer: Box::new(Explorer::new(root_path.clone())),
            resources,
        })
    }

    /// Creates the initial file tree when starting up
    pub async fn create_initial_tree(&self) -> Result<FileTreeEntry> {
        self.explorer.create_initial_tree(2)
    }

    async fn handle_resources_list(&self, id: RequestId) -> Result<String> {
        let resources = self.resources.list_resources().await;
        let response = JSONRPCResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: ListResourcesResult {
                resources,
                next_cursor: None,
            },
        };

        Ok(serde_json::to_string(&response)?)
    }

    async fn handle_resources_read(&self, id: RequestId, uri: String) -> Result<String> {
        let response = match self.resources.read_resource(&uri).await {
            Some(content) => serde_json::to_string(&JSONRPCResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: ReadResourceResult {
                    contents: vec![content],
                },
            })?,
            None => serde_json::to_string(&JSONRPCError {
                jsonrpc: "2.0".to_string(),
                id,
                error: ErrorObject {
                    code: -32001,
                    message: format!("Resource not found: {}", uri),
                    data: None,
                },
            })?,
        };

        Ok(response)
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

                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir().join(&path)
                };

                match self.explorer.read_file(&full_path) {
                    Ok(content) => {
                        // Update resources when a file is read
                        self.resources
                            .update_loaded_file(path, content.clone())
                            .await;
                        ToolCallResult {
                            content: vec![ToolResultContent::Text { text: content }],
                            is_error: None,
                        }
                    }
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
                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir().join(path)
                };

                match self.explorer.list_files(&full_path, max_depth) {
                    Ok(tree_entry) => {
                        // Update the file tree resource when listing files
                        self.resources.update_file_tree(tree_entry.clone()).await;
                        ToolCallResult {
                            content: vec![ToolResultContent::Text {
                                text: tree_entry.to_string(),
                            }],
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
            JSONRPCMessage::Request(request) => {
                debug!("Processing request: {:?}", request);
                match (request.method.as_str(), request.id) {
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

                    ("resources/list", Some(id)) => {
                        debug!("Handling resources/list request");
                        self.handle_resources_list(id).await.map(Some)
                    }

                    ("resources/read", Some(id)) => {
                        debug!("Handling resources/read request");
                        let params: ReadResourceRequest = serde_json::from_value(request.params)?;
                        self.handle_resources_read(id, params.uri).await.map(Some)
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

                    (_, None) => {
                        debug!("Received notification: {}", request.method);
                        Ok(None)
                    }

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

// Add ReadResourceRequest struct to types.rs if not already present
#[derive(Debug, Deserialize)]
struct ReadResourceRequest {
    uri: String,
}

// Add ReadResourceResult struct to types.rs if not already present
#[derive(Debug, Serialize)]
struct ReadResourceResult {
    contents: Vec<ResourceContent>,
}
