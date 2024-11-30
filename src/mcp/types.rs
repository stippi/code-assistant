use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Number(i64),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JSONRPCRequest {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JSONRPCMessage {
    Request(JSONRPCRequest),
    Notification {
        jsonrpc: String,
        method: String,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },
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

// Client capabilities types
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientCapabilities {
    pub tools: Option<ToolsCapability>,
    #[serde(default)]
    pub experimental: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolsCapability {
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

// Initialize request/response types
#[derive(Debug, Serialize, Deserialize)]
pub struct InitializeParams {
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: Implementation,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "serverInfo")]
    pub server_info: Implementation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

// Resource types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResourcesResult {
    pub resources: Vec<Resource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadResourceRequest {
    pub uri: String,
}

#[derive(Debug, Serialize)]
pub struct ReadResourceResult {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

// Tool types
#[derive(Debug, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolResultContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

// Prompt types
#[derive(Debug, Serialize, Deserialize)]
pub struct ListPromptsResult {
    pub prompts: Vec<Prompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Prompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}
