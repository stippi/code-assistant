use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Number(i64),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JSONRPCMessage {
    Request {
        jsonrpc: String,
        id: RequestId,
        method: String,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },
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

#[derive(Debug, Serialize)]
pub struct EmptyResult {
    #[serde(skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<serde_json::Value>,
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
    #[serde(rename = "listChanged")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResourcesCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: Option<bool>,
    pub subscribe: Option<bool>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
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
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadResourceRequest {
    pub uri: String,
}

#[derive(Debug, Deserialize)]
pub struct SubscribeResourceRequest {
    pub uri: String,
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeResourceRequest {
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
pub struct ListToolsResult {
    pub tools: Vec<serde_json::Value>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
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
    #[serde(rename = "isError")]
    pub is_error: bool,
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
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_deserialization_string_id_with_params() {
        // Request mit String-ID und Parametern
        let json_str = r#"{
            "jsonrpc": "2.0",
            "id": "test-id-1",
            "method": "test_method",
            "params": {"key": "value"}
        }"#;

        let message: JSONRPCMessage = serde_json::from_str(json_str).unwrap();

        match message {
            JSONRPCMessage::Request {
                jsonrpc,
                id,
                method,
                params,
            } => {
                assert_eq!(jsonrpc, "2.0");
                assert!(matches!(id, RequestId::String(s) if s == "test-id-1"));
                assert_eq!(method, "test_method");
                assert!(params.is_some());
                if let Some(p) = params {
                    assert_eq!(p["key"], "value");
                }
            }
            _ => panic!("Deserialized to wrong variant"),
        }
    }

    #[test]
    fn test_request_deserialization_string_id_without_params() {
        // Request mit String-ID aber ohne Parameter
        let json_str = r#"{
            "jsonrpc": "2.0",
            "id": "test-id-2",
            "method": "test_method"
        }"#;

        let message: JSONRPCMessage = serde_json::from_str(json_str).unwrap();

        match message {
            JSONRPCMessage::Request {
                jsonrpc,
                id,
                method,
                params,
            } => {
                assert_eq!(jsonrpc, "2.0");
                assert!(matches!(id, RequestId::String(s) if s == "test-id-2"));
                assert_eq!(method, "test_method");
                assert!(params.is_none());
            }
            _ => panic!("Deserialized to wrong variant"),
        }
    }

    #[test]
    fn test_request_deserialization_number_id_with_params() {
        // Request mit Number-ID und Parametern
        let json_str = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "method": "test_method",
            "params": {"key": "value"}
        }"#;

        let message: JSONRPCMessage = serde_json::from_str(json_str).unwrap();

        match message {
            JSONRPCMessage::Request {
                jsonrpc,
                id,
                method,
                params,
            } => {
                assert_eq!(jsonrpc, "2.0");
                assert!(matches!(id, RequestId::Number(n) if n == 42));
                assert_eq!(method, "test_method");
                assert!(params.is_some());
                if let Some(p) = params {
                    assert_eq!(p["key"], "value");
                }
            }
            _ => panic!("Deserialized to wrong variant"),
        }
    }

    #[test]
    fn test_request_deserialization_number_id_without_params() {
        // Request mit Number-ID aber ohne Parameter
        let json_str = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "method": "test_method"
        }"#;

        let message: JSONRPCMessage = serde_json::from_str(json_str).unwrap();

        match message {
            JSONRPCMessage::Request {
                jsonrpc,
                id,
                method,
                params,
            } => {
                assert_eq!(jsonrpc, "2.0");
                assert!(matches!(id, RequestId::Number(n) if n == 42));
                assert_eq!(method, "test_method");
                assert!(params.is_none());
            }
            _ => panic!("Deserialized to wrong variant"),
        }
    }

    #[test]
    fn test_notification_deserialization_with_params() {
        // Notification mit Parametern
        let json_str = r#"{
            "jsonrpc": "2.0",
            "method": "notification_method",
            "params": {"event": "something_happened"}
        }"#;

        let message: JSONRPCMessage = serde_json::from_str(json_str).unwrap();

        match message {
            JSONRPCMessage::Notification {
                jsonrpc,
                method,
                params,
            } => {
                assert_eq!(jsonrpc, "2.0");
                assert_eq!(method, "notification_method");
                assert!(params.is_some());
                if let Some(p) = params {
                    assert_eq!(p["event"], "something_happened");
                }
            }
            _ => panic!("Deserialized to wrong variant"),
        }
    }

    #[test]
    fn test_notification_deserialization_without_params() {
        // Notification ohne Parameter
        let json_str = r#"{
            "jsonrpc": "2.0",
            "method": "notification_method"
        }"#;

        let message: JSONRPCMessage = serde_json::from_str(json_str).unwrap();

        match message {
            JSONRPCMessage::Notification {
                jsonrpc,
                method,
                params,
            } => {
                assert_eq!(jsonrpc, "2.0");
                assert_eq!(method, "notification_method");
                assert!(params.is_none());
            }
            _ => panic!("Deserialized to wrong variant"),
        }
    }
}
