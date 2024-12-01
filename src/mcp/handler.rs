use super::resources::ResourceManager;
use super::types::*;
use crate::explorer::Explorer;
use crate::types::CodeExplorer;
use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{AsyncWriteExt, Stdout};
use tracing::{debug, error};

pub struct MessageHandler {
    explorer: Box<dyn CodeExplorer>,
    resources: ResourceManager,
    stdout: Stdout,
}

impl MessageHandler {
    pub fn new(root_path: PathBuf, stdout: Stdout) -> Result<Self> {
        Ok(Self {
            explorer: Box::new(Explorer::new(root_path.clone())),
            resources: ResourceManager::new(),
            stdout,
        })
    }

    /// Creates the initial file tree when starting up
    pub async fn create_initial_tree(&mut self) -> Result<()> {
        let tree = self.explorer.create_initial_tree(2)?;
        self.resources.update_file_tree(tree);
        self.send_list_changed_notification().await?;
        self.send_resource_updated_notification("tree:///").await?;
        Ok(())
    }

    /// Sends a JSON-RPC response
    async fn send_response<T: serde::Serialize>(&mut self, id: RequestId, result: T) -> Result<()> {
        let response = JSONRPCResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result,
        };
        self.send_message(&serde_json::to_value(response)?).await
    }

    /// Sends a JSON-RPC error response
    async fn send_error(
        &mut self,
        id: RequestId,
        code: i32,
        message: String,
        data: Option<serde_json::Value>,
    ) -> Result<()> {
        let error = JSONRPCError {
            jsonrpc: "2.0".to_string(),
            id,
            error: ErrorObject {
                code,
                message,
                data,
            },
        };
        self.send_message(&serde_json::to_value(error)?).await
    }

    /// Sends a notification
    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<()> {
        let notification = if let Some(params) = params {
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            })
        } else {
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": method
            })
        };

        self.send_message(&notification).await
    }

    /// Helper method to send any JSON message
    async fn send_message(&mut self, message: &serde_json::Value) -> Result<()> {
        let message_str = serde_json::to_string(message)?;
        debug!("Sending message: {}", message_str);
        self.stdout.write_all(message_str.as_bytes()).await?;
        self.stdout.write_all(b"\n").await?;
        self.stdout.flush().await?;
        Ok(())
    }

    /// Notify clients that the resource list has changed
    async fn send_list_changed_notification(&mut self) -> Result<()> {
        self.send_notification("notifications/resources/list_changed", None)
            .await
    }

    /// Notify clients that a specific resource has been updated
    async fn send_resource_updated_notification(&mut self, uri: &str) -> Result<()> {
        self.send_notification(
            "notifications/resources/updated",
            Some(serde_json::json!({ "uri": uri })),
        )
        .await
    }

    /// Handle initialize request
    async fn handle_initialize(&mut self, id: RequestId, params: InitializeParams) -> Result<()> {
        debug!("Initialize params: {:?}", params);

        self.send_response(
            id,
            InitializeResult {
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
                instructions: Some("Code Assistant helps you analyze and modify code.".to_string()),
            },
        )
        .await
    }

    /// Handle resources/list request
    async fn handle_resources_list(&mut self, id: RequestId) -> Result<()> {
        debug!("Handling resources/list request");
        self.send_response(
            id,
            ListResourcesResult {
                resources: self.resources.list_resources(),
                next_cursor: None,
            },
        )
        .await
    }

    /// Handle resources/read request
    async fn handle_resources_read(&mut self, id: RequestId, uri: String) -> Result<()> {
        debug!("Handling resources/read request for {}", uri);
        match self.resources.read_resource(&uri) {
            Some(content) => {
                self.send_response(
                    id,
                    ReadResourceResult {
                        contents: vec![content],
                    },
                )
                .await
            }
            None => {
                self.send_error(id, -32001, format!("Resource not found: {}", uri), None)
                    .await
            }
        }
    }

    /// Handle tools/list request
    async fn handle_tools_list(&mut self, id: RequestId) -> Result<()> {
        debug!("Handling tools/list request");
        self.send_response(
            id,
            ListToolsResult {
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
        )
        .await
    }

    /// Handle tools/call request
    async fn handle_tool_call(&mut self, id: RequestId, params: ToolCallParams) -> Result<()> {
        debug!("Handling tool call for {}", params.name);
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
                            .update_loaded_file(path.clone(), content.clone());
                        self.send_list_changed_notification().await?;
                        self.send_resource_updated_notification(&format!(
                            "file://{}",
                            path.display()
                        ))
                        .await?;

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
                        self.resources.update_file_tree(tree_entry.clone());
                        self.send_list_changed_notification().await?;
                        self.send_resource_updated_notification("tree:///").await?;

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

            _ => {
                return self
                    .send_error(id, -32601, format!("Unknown tool: {}", params.name), None)
                    .await;
            }
        };

        self.send_response(id, result).await
    }

    /// Handle prompts/list request
    async fn handle_prompts_list(&mut self, id: RequestId) -> Result<()> {
        debug!("Handling prompts/list request");
        self.send_response(
            id,
            ListPromptsResult {
                prompts: vec![],
                next_cursor: None,
            },
        )
        .await
    }

    /// Main message handling entry point
    pub async fn handle_message(&mut self, message: &str) -> Result<()> {
        // Parse the message first
        let message: JSONRPCMessage = match serde_json::from_str(message) {
            Ok(msg) => msg,
            Err(e) => {
                error!("Invalid JSON-RPC message: {}", e);
                return Ok(());
            }
        };

        match message {
            JSONRPCMessage::Request(request) => {
                debug!("Processing request: {:?}", request);
                match (request.method.as_str(), request.id) {
                    ("initialize", Some(id)) => {
                        let params: InitializeParams = serde_json::from_value(request.params)?;
                        self.handle_initialize(id, params).await?;
                    }

                    ("resources/list", Some(id)) => {
                        self.handle_resources_list(id).await?;
                    }

                    ("resources/read", Some(id)) => {
                        let params: ReadResourceRequest = serde_json::from_value(request.params)?;
                        self.handle_resources_read(id, params.uri).await?;
                    }

                    ("tools/list", Some(id)) => {
                        self.handle_tools_list(id).await?;
                    }

                    ("tools/call", Some(id)) => {
                        let params: ToolCallParams = serde_json::from_value(request.params)?;
                        self.handle_tool_call(id, params).await?;
                    }

                    ("prompts/list", Some(id)) => {
                        self.handle_prompts_list(id).await?;
                    }

                    (method, Some(id)) => {
                        self.send_error(id, -32601, format!("Method not found: {}", method), None)
                            .await?;
                    }

                    (_, None) => {
                        debug!("Received notification request - ignoring");
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
                }
                _ => {
                    debug!("Unknown notification: {}", method);
                }
            },
        }

        Ok(())
    }
}
