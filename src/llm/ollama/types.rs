use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct OllamaRequest {
    pub model: String,
    pub messages: Vec<OllamaMessage>,
    pub stream: bool,
    pub options: OllamaOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
pub struct OllamaOptions {
    pub num_ctx: usize,
}

#[derive(Debug, Serialize)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct OllamaResponse {
    pub message: OllamaResponseMessage,
    #[allow(dead_code)]
    pub done_reason: Option<String>,
    #[allow(dead_code)]
    pub done: bool,
    #[serde(default)]
    pub prompt_eval_count: u32,
    #[serde(default)]
    pub eval_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaToolCall {
    pub function: OllamaFunction,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaFunction {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct OllamaResponseMessage {
    pub content: String,
    pub tool_calls: Option<Vec<OllamaToolCall>>,
}
