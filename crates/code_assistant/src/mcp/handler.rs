use super::resources::ResourceManager;
use super::types::*;
use crate::config::{DefaultProjectManager, ProjectManager};
use crate::tools::core::ToolRegistry;
use crate::utils::{MessageWriter, StdoutWriter};
use anyhow::Result;
use command_executor::{CommandExecutor, DefaultCommandExecutor};
use tokio::io::Stdout;
use tracing::{debug, error, trace};

pub struct MessageHandler {
    project_manager: Box<dyn ProjectManager>,
    command_executor: Box<dyn CommandExecutor>,
    resources: ResourceManager,
    message_writer: Box<dyn MessageWriter>,
}

impl MessageHandler {
    pub fn new(stdout: Stdout) -> Result<Self> {
        Ok(Self {
            project_manager: Box::new(DefaultProjectManager::new()),
            command_executor: Box::new(DefaultCommandExecutor),
            resources: ResourceManager::new(),
            message_writer: Box::new(StdoutWriter::new(stdout)),
        })
    }

    #[cfg(test)]
    pub fn with_dependencies(
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        message_writer: Box<dyn MessageWriter>,
    ) -> Self {
        Self {
            project_manager,
            command_executor,
            resources: ResourceManager::new(),
            message_writer,
        }
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

        self.message_writer.write_message(&message_str).await
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
                        list_changed: Some(true),
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

    /// Notify clients that a specific resource has been updated
    #[allow(dead_code)]
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
                self.send_error(id, -32001, format!("Resource not found: {uri}"), None)
                    .await
            }
        }
    }

    /// Handle resources/subscribe request
    async fn handle_resources_subscribe(&mut self, id: RequestId, uri: String) -> Result<()> {
        debug!("Handling resources/subscribe request for {}", uri);
        if self.resources.read_resource(&uri).is_none() {
            return self
                .send_error(id, -32001, format!("Resource not found: {uri}"), None)
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

        // Use the ToolRegistry to get tool definitions
        let registry = ToolRegistry::global();
        let tool_defs =
            registry.get_tool_definitions_for_scope(crate::tools::core::ToolScope::McpServer);

        // Map tool definitions to the expected JSON structure
        let tools_json = tool_defs
            .iter()
            .map(|tool_def| {
                let mut json = serde_json::json!({
                    "name": tool_def.name,
                    "description": tool_def.description,
                    "inputSchema": tool_def.parameters
                });

                // Include annotations if present
                if let Some(annotations) = &tool_def.annotations {
                    json["annotations"] = annotations.clone();
                }

                json
            })
            .collect();

        self.send_response(
            id,
            ListToolsResult {
                tools: tools_json,
                next_cursor: None,
            },
        )
        .await
    }

    /// Notify clients that the tools list has changed
    #[allow(dead_code)]
    async fn send_tools_changed_notification(&mut self) -> Result<()> {
        self.send_notification("notifications/tools/list_changed", None)
            .await
    }

    /// Handle tools/call request
    async fn handle_tool_call(&mut self, id: RequestId, params: ToolCallParams) -> Result<()> {
        debug!("Handling tool call for {}", params.name);

        let result = async {
            let arguments = params
                .arguments
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Missing parameters"))?;

            // Get the tool from the registry
            let registry = ToolRegistry::global();
            let tool = registry
                .get(&params.name)
                .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", params.name))?;

            // Create a tool context with references (no UI for MCP)
            let mut context = crate::tools::core::ToolContext {
                project_manager: self.project_manager.as_ref(),
                command_executor: self.command_executor.as_ref(),
                plan: None,
                ui: None,
                tool_id: None,
                permission_handler: None,
                sub_agent_runner: None,
                sub_agent_cancellation_registry: None,
            };

            // Invoke the tool
            let mut input = arguments.clone();
            let result = tool.invoke(&mut context, &mut input).await?;
            // input might have changed, but we have to ignore it in MCP mode

            // Format the output
            let mut tracker = crate::tools::core::ResourcesTracker::new();
            let output = result.as_render().render(&mut tracker);
            let is_success = result.is_success();

            Ok::<_, anyhow::Error>((output, is_success))
        }
        .await;

        // Convert the result into a ToolCallResult response
        match result {
            Ok((output, is_success)) => {
                self.send_response(
                    id,
                    ToolCallResult {
                        content: vec![ToolResultContent::Text { text: output }],
                        is_error: !is_success,
                    },
                )
                .await
            }
            Err(e) => self.send_error(id, -32602, e.to_string(), None).await,
        }
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
            JSONRPCMessage::Request {
                method, id, params, ..
            } => {
                trace!("Processing request: method={}, id={:?}", method, id);
                match method.as_str() {
                    "initialize" => {
                        let params: InitializeParams =
                            serde_json::from_value(params.unwrap_or_default())?;
                        self.handle_initialize(id, params).await?;
                    }

                    "resources/list" => {
                        self.handle_resources_list(id).await?;
                    }
                    "resources/read" => {
                        let params: ReadResourceRequest =
                            serde_json::from_value(params.unwrap_or_default())?;
                        self.handle_resources_read(id, params.uri).await?;
                    }
                    "resources/subscribe" => {
                        let params: SubscribeResourceRequest =
                            serde_json::from_value(params.unwrap_or_default())?;
                        self.handle_resources_subscribe(id, params.uri).await?;
                    }
                    "resources/unsubscribe" => {
                        let params: UnsubscribeResourceRequest =
                            serde_json::from_value(params.unwrap_or_default())?;
                        self.handle_resources_unsubscribe(id, params.uri).await?;
                    }

                    "tools/list" => {
                        self.handle_tools_list(id).await?;
                    }

                    "tools/call" => {
                        match serde_json::from_value::<ToolCallParams>(params.unwrap_or_default()) {
                            Ok(params) => {
                                self.handle_tool_call(id, params).await?;
                            }
                            Err(e) => {
                                self.send_response(
                                    id,
                                    ToolCallResult {
                                        content: vec![ToolResultContent::Text {
                                            text: format!("Invalid tool parameters: {e}"),
                                        }],
                                        is_error: true,
                                    },
                                )
                                .await?;
                            }
                        }
                    }

                    "prompts/list" => {
                        self.handle_prompts_list(id).await?;
                    }

                    method => {
                        self.send_error(id, -32601, format!("Method not found: {method}"), None)
                            .await?;
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
