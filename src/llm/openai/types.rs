use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Clone)]
pub struct OpenAIRequest {
    pub model: String,
    pub messages: Vec<OpenAIChatMessage>,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StreamOptions {
    pub include_usage: bool,
}

impl OpenAIRequest {
    pub fn into_streaming(mut self) -> Self {
        self.stream = Some(true);
        self.stream_options = Some(StreamOptions {
            include_usage: true,
        });
        self
    }

    pub fn into_non_streaming(mut self) -> Self {
        self.stream = None;
        self.stream_options = None;
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: OpenAIFunction,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OpenAIUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[allow(dead_code)]
    pub total_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct OpenAIResponse {
    pub choices: Vec<OpenAIChoice>,
    pub usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
pub struct OpenAIChoice {
    pub message: OpenAIChatMessage,
}

#[derive(Debug, Deserialize)]
pub struct OpenAIErrorResponse {
    #[allow(dead_code)]
    pub error: OpenAIError,
}

#[derive(Debug, Deserialize)]
pub struct OpenAIError {
    #[allow(dead_code)]
    pub message: String,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    pub code: Option<String>,
}
