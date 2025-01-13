use super::resources::ResourceManager;
use super::types::*;
use crate::explorer::Explorer;
use crate::tool_definitions::Tools;
use crate::types::{CodeExplorer, FileUpdate, SearchMode, SearchOptions};
use crate::utils::format_with_line_numbers;
use crate::utils::{CommandExecutor, DefaultCommandExecutor};
use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{AsyncWriteExt, Stdout};
use tracing::{debug, error, trace};

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

        // Skip logging for certain message types
        let skip_logging = ["\"result\":{\"prompts\":", "\"result\":{\"resources\":"]
            .iter()
            .any(|s| message_str.contains(s));

        if !skip_logging {
            debug!("Sending message: {}", message_str);
        }

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
        if !self.resources.is_subscribed(uri) {
            debug!("Resource changed, but is not subscribed: {}", uri);
            return Ok(());
        }
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
                    resources: Some(ResourcesCapability {
                        list_changed: Some(true),
                        subscribe: Some(true),
                    }),
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
        trace!("Handling resources/list request");
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

    /// Handle resources/subscribe request
    async fn handle_resources_subscribe(&mut self, id: RequestId, uri: String) -> Result<()> {
        debug!("Handling resources/subscribe request for {}", uri);
        if self.resources.read_resource(&uri).is_none() {
            return self
                .send_error(id, -32001, format!("Resource not found: {}", uri), None)
                .await;
        }

        self.resources.subscribe(&uri);
        self.send_response(id, EmptyResult { meta: None }).await
    }

    /// Handle resources/unsubscribe request
    async fn handle_resources_unsubscribe(&mut self, id: RequestId, uri: String) -> Result<()> {
        debug!("Handling resources/unsubscribe request for {}", uri);

        self.resources.unsubscribe(&uri);
        self.send_response(id, EmptyResult { meta: None }).await
    }

    /// Handle tools/list request
    async fn handle_tools_list(&mut self, id: RequestId) -> Result<()> {
        debug!("Handling tools/list request");
        self.send_response(
            id,
            ListToolsResult {
                tools: Tools::mcp()
                    .into_iter()
                    .map(|tool| {
                        serde_json::json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.parameters
                        })
                    })
                    .collect(),
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
                        let resource_uri = format!("file://{}", path.display());
                        self.send_resource_updated_notification(&resource_uri)
                            .await?;

                        ToolCallResult {
                            content: vec![ToolResultContent::Text {
                                text: format!(
                                    "File loaded as resource: {}\nContent:\n{}",
                                    resource_uri,
                                    format_with_line_numbers(content.as_str())
                                ),
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

            "search" => {
                let args = params
                    .arguments
                    .ok_or_else(|| anyhow::anyhow!("No arguments provided"))?;
                let options = SearchOptions {
                    query: args["query"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'query' argument"))?
                        .to_string(),
                    case_sensitive: args
                        .get("case_sensitive")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    whole_words: args
                        .get("whole_words")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    mode: match args.get("mode").and_then(|v| v.as_str()) {
                        Some("regex") => SearchMode::Regex,
                        _ => SearchMode::Exact,
                    },
                    max_results: args
                        .get("max_results")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
                };

                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| self.explorer.root_dir().join(p))
                    .unwrap_or_else(|| self.explorer.root_dir().clone());

                match self.explorer.search(&path, options) {
                    Ok(results) => {
                        let mut output = String::new();
                        for result in results {
                            output.push_str(&format!(
                                "{}:{}:{}\n",
                                result.file.display(),
                                result.line_number,
                                result.line_content
                            ));
                        }
                        ToolCallResult {
                            content: vec![ToolResultContent::Text { text: output }],
                            is_error: None,
                        }
                    }
                    Err(e) => ToolCallResult {
                        content: vec![ToolResultContent::Text {
                            text: format!("Error searching files: {}", e),
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
                    .map(|dir| self.explorer.root_dir().join(dir));
                // Use root_dir as default working directory
                let root_dir = self.explorer.root_dir();
                let working_dir = working_dir.as_ref().unwrap_or(&root_dir);
                match self
                    .command_executor
                    .execute(command_line, Some(working_dir))
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
        trace!("Handling prompts/list request");
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
                trace!("Processing request: {:?}", request);
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

                    ("resources/subscribe", Some(id)) => {
                        let params: SubscribeResourceRequest =
                            serde_json::from_value(request.params)?;
                        self.handle_resources_subscribe(id, params.uri).await?;
                    }

                    ("resources/unsubscribe", Some(id)) => {
                        let params: UnsubscribeResourceRequest =
                            serde_json::from_value(request.params)?;
                        self.handle_resources_unsubscribe(id, params.uri).await?;
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
