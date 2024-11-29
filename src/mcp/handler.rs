use super::types::*;
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
    root_path: PathBuf,
}

impl MessageHandler {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
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

                    // Handle notifications (no response needed)
                    ("notifications/initialized", None) => {
                        // Parse notification params - they're optional but should be validated if present
                        if let Some(params) = request.params.as_object() {
                            debug!(
                                "Received initialized notification with params: {:?}",
                                params
                            );
                        }
                        Ok(None)
                    }

                    // Handle resources/list
                    ("resources/list", Some(id)) => {
                        debug!("Handling resources/list request");
                        let uri = format!("file://{}", self.root_path.display());
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

                    ("tools/list", Some(id)) => {
                        debug!("Handling tools/list request");
                        self.handle_tools_list(id).await.map(Some)
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
