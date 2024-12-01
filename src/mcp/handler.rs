use super::resources::ResourceManager;
use super::types::*;
use crate::explorer::Explorer;
use crate::types::{CodeExplorer, FileUpdate};
use crate::utils::{CommandExecutor, DefaultCommandExecutor};
use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{AsyncWriteExt, Stdout};
use tracing::{debug, error};

pub struct MessageHandler {
    explorer: Box<dyn CodeExplorer>,
    command_executor: Box<dyn CommandExecutor>,
    resources: ResourceManager,
    stdout: Stdout,
}

impl MessageHandler {
    pub fn new(root_path: PathBuf, stdout: Stdout) -> Result<Self> {
        Ok(Self {
            explorer: Box::new(Explorer::new(root_path.clone())),
            command_executor: Box::new(DefaultCommandExecutor),
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
                        name: "execute-command".to_string(),
                        description: Some("Execute a command line program".to_string()),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "command_line": {
                                    "type": "string",
                                    "description": "The complete command to execute"
                                },
                                "working_dir": {
                                    "type": "string",
                                    "description": "Optional: working directory for the command"
                                }
                            },
                            "required": ["command_line"]
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
                    Tool {
                        name: "load-file".to_string(),
                        description: Some(
                            "Load a file into working memory for access as a resource".to_string(),
                        ),
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
                        name: "summarize".to_string(),
                        description: Some("Replace file content with a summary in working memory, unloading the full content.".to_string()),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "files": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "path": {
                                                "type": "string",
                                                "description": "Path to the file to summarize"
                                            },
                                            "summary": {
                                                "type": "string",
                                                "description": "Your summary of the file contents"
                                            }
                                        },
                                        "required": ["path", "summary"]
                                    }
                                }
                            },
                            "required": ["files"]
                        }),
                    },
                    Tool {
                        name: "update-file".to_string(),
                        description: Some(
                            "Update sections in an existing file based on line numbers. IMPORTANT: Line numbers are 1-based, \
                             matching the line numbers shown when viewing file resources. For example, to replace the first \
                             line of a file, use start_line: 1, not 0.".to_string()
                        ),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Relative path to the file to update"
                                },
                                "updates": {
                                    "type": "array",
                                    "description": "List of updates to apply to the file",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "start_line": {
                                                "type": "integer",
                                                "description": "First line number to replace (1-based, matching the displayed line numbers)"
                                            },
                                            "end_line": {
                                                "type": "integer",
                                                "description": "Last line number to replace (1-based, matching the displayed line numbers)"
                                            },
                                            "new_content": {
                                                "type": "string",
                                                "description": "The new content to insert (without line numbers)"
                                            }
                                        },
                                        "required": ["start_line", "end_line", "new_content"]
                                    }
                                }
                            },
                            "required": ["path", "updates"]
                        }),
                    },
                    Tool {
                        name: "delete-file".to_string(),
                        description: Some("Delete a file from the workspace. This operation cannot be undone!".to_string()),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Relative path to the file to delete"
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
            "load-file" => {
                // Changed from "read-file"
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
                        self.resources.update_loaded_file(path.clone(), content);
                        self.send_list_changed_notification().await?;
                        let resource_uri = format!("file://{}", path.display());
                        self.send_resource_updated_notification(&resource_uri)
                            .await?;

                        ToolCallResult {
                            content: vec![ToolResultContent::Text {
                                text: format!("File loaded as resource: {}", resource_uri),
                            }],
                            is_error: None,
                        }
                    }
                    Err(e) => ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!("Error loading file: {}", e),
                        }],
                        is_error: Some(true),
                    },
                }
            }

            "summarize" => {
                let args = params
                    .arguments
                    .ok_or_else(|| anyhow::anyhow!("No arguments provided"))?;

                let files = args["files"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'files' array"))?;

                let mut processed_files = Vec::new();

                for file_entry in files {
                    let path = PathBuf::from(
                        file_entry["path"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing or invalid path"))?,
                    );
                    let summary = file_entry["summary"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing or invalid summary"))?
                        .to_string();

                    // Update the resources - remove file content and add summary
                    self.resources.remove_loaded_file(&path);
                    self.resources.update_file_summary(path.clone(), summary);

                    processed_files.push(format!("file://{}", path.display()));
                }

                // Notify about changes
                self.send_list_changed_notification().await?;
                for uri in &processed_files {
                    self.send_resource_updated_notification(uri).await?;
                    self.send_resource_updated_notification(&uri.replace("file://", "summary://"))
                        .await?;
                }

                ToolCallResult {
                    content: vec![ToolResultContent::Text {
                        text: format!(
                            "Replaced {} file(s) with summaries. Files unloaded: {}",
                            processed_files.len(),
                            processed_files.join(", ")
                        ),
                    }],
                    is_error: None,
                }
            }

            "update-file" => {
                let args = params
                    .arguments
                    .ok_or_else(|| anyhow::anyhow!("No arguments provided"))?;

                let path = PathBuf::from(
                    args["path"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'path' argument"))?,
                );
                let updates = args["updates"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'updates' array"))?;

                let mut file_updates = Vec::new();
                for update in updates {
                    file_updates.push(FileUpdate {
                        start_line: update["start_line"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid start_line"))?
                            as usize,
                        end_line: update["end_line"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid end_line"))?
                            as usize,
                        new_content: update["new_content"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing new_content"))?
                            .to_string(),
                    });
                }

                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir().join(&path)
                };

                match self.explorer.apply_updates(&full_path, &file_updates) {
                    Ok(new_content) => {
                        // If the file is currently loaded as a resource, update it
                        if self.resources.is_file_loaded(&path) {
                            self.resources.update_loaded_file(path.clone(), new_content);
                            self.send_resource_updated_notification(&format!(
                                "file://{}",
                                path.display()
                            ))
                            .await?;
                        }

                        ToolCallResult {
                            content: vec![ToolResultContent::Text {
                                text: format!(
                                    "Successfully applied {} updates to {}",
                                    file_updates.len(),
                                    path.display()
                                ),
                            }],
                            is_error: None,
                        }
                    }
                    Err(e) => ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!("Error updating file: {}", e),
                        }],
                        is_error: Some(true),
                    },
                }
            }

            "delete-file" => {
                let args = params
                    .arguments
                    .ok_or_else(|| anyhow::anyhow!("No arguments provided"))?;
                let path = PathBuf::from(
                    args["path"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'path' argument"))?,
                );
                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir().join(&path)
                };
                // First check if file exists and is actually a file
                if full_path.is_file() {
                    // Try to delete the file
                    match std::fs::remove_file(&full_path) {
                        Ok(_) => {
                            // Remove from resources if loaded
                            self.resources.remove_loaded_file(&path);
                            // Remove summary if exists
                            self.resources.remove_file_summary(&path);
                            // Notify clients
                            self.send_list_changed_notification().await?;
                            ToolCallResult {
                                content: vec![ToolResultContent::Text {
                                    text: format!("Successfully deleted {}", path.display()),
                                }],
                                is_error: None,
                            }
                        }
                        Err(e) => ToolCallResult {
                            content: vec![ToolResultContent::Text {
                                text: format!("Error deleting file: {}", e),
                            }],
                            is_error: Some(true),
                        },
                    }
                } else {
                    ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!(
                                "Error: {} is not a file or doesn't exist",
                                path.display()
                            ),
                        }],
                        is_error: Some(true),
                    }
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
            "execute-command" => {
                let args = params
                    .arguments
                    .ok_or_else(|| anyhow::anyhow!("No arguments provided"))?;
                let command_line = args["command_line"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'command_line' argument"))?;
                let working_dir = args
                    .get("working_dir")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
                match self
                    .command_executor
                    .execute(command_line, working_dir.as_ref())
                    .await
                {
                    Ok(output) => {
                        let mut result = String::new();
                        if !output.stdout.is_empty() {
                            result.push_str("Output:\n");
                            result.push_str(&output.stdout);
                        }
                        if !output.stderr.is_empty() {
                            if !result.is_empty() {
                                result.push_str("\n");
                            }
                            result.push_str("Errors:\n");
                            result.push_str(&output.stderr);
                        }
                        ToolCallResult {
                            content: vec![ToolResultContent::Text { text: result }],
                            is_error: if output.success { None } else { Some(true) },
                        }
                    }
                    Err(e) => ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!("Failed to execute command: {}", e),
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
