use super::resources::ResourceManager;
use super::types::*;
use crate::config::{DefaultProjectManager, ProjectManager};
use crate::tools::{parse_tool_json, MCPToolHandler, ToolExecutor};
use crate::types::Tools;
use crate::utils::{CommandExecutor, DefaultCommandExecutor};
use anyhow::Result;
use tokio::io::{AsyncWriteExt, Stdout};
use tracing::{debug, error, trace};

pub struct MessageHandler {
    project_manager: Box<dyn ProjectManager>,
    command_executor: Box<dyn CommandExecutor>,
    resources: ResourceManager,
    stdout: Stdout,
}

impl MessageHandler {
    pub fn new(stdout: Stdout) -> Result<Self> {
        Ok(Self {
            project_manager: Box::new(DefaultProjectManager::new()),
            command_executor: Box::new(DefaultCommandExecutor),
            resources: ResourceManager::new(),
            stdout,
        })
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

        self.stdout.write_all(message_str.as_bytes()).await?;
        self.stdout.write_all(b"\n").await?;
        self.stdout.flush().await?;
        Ok(())
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

        // Map tool definitions to the expected JSON structure
        let tools_json = Tools::mcp()
            .into_iter()
            .map(|tool| {
                let mut json = serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.parameters
                });

                // Include annotations if present
                if let Some(annotations) = &tool.annotations {
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

            let tool = parse_tool_json(&params.name, arguments)?;
            let mut handler = MCPToolHandler::new();

            let (output, result) = ToolExecutor::execute(
                &mut handler,
                &self.project_manager,
                &self.command_executor,
                None,
                &tool,
            )
            .await?;

            Ok::<_, anyhow::Error>((output, result.is_success()))
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
                        match serde_json::from_value::<ToolCallParams>(request.params) {
                            Ok(params) => {
                                self.handle_tool_call(id, params).await?;
                            }
                            Err(e) => {
                                self.send_response(
                                    id,
                                    ToolCallResult {
                                        content: vec![ToolResultContent::Text {
                                            text: format!("Invalid tool parameters: {}", e),
                                        }],
                                        is_error: true,
                                    },
                                )
                                .await?;
                            }
                        }
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
