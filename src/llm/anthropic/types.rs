use crate::llm::types::Message;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: usize,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct AnthropicErrorResponse {
    #[serde(rename = "type")]
    pub error_type: String,
    pub error: AnthropicErrorPayload,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct AnthropicErrorPayload {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}
