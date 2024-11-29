use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct JSONRPCRequest {
    pub jsonrpc: String, // Always "2.0"
    pub id: RequestId,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Number(i64),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JSONRPCResponse {
    pub jsonrpc: String, // Always "2.0"
    pub id: RequestId,
    pub result: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JSONRPCError {
    pub jsonrpc: String, // Always "2.0"
    pub id: RequestId,
    pub error: ErrorObject,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

// Initialize Request/Response types
#[derive(Debug, Serialize, Deserialize)]
pub struct InitializeParams {
    pub capabilities: ClientCapabilities,
    pub client_info: Implementation,
    pub protocol_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClientCapabilities {
    // For now we only implement what we need
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolsCapability {
    pub list_changed: Option<bool>,
}
