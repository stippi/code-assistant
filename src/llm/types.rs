use serde::{Deserialize, Serialize};

/// Generic request structure that can be mapped to different providers
#[derive(Debug, Serialize)]
pub struct LLMRequest {
    pub messages: Vec<Message>,
    pub max_tokens: usize,
    pub temperature: f32,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: MessageContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Structured(Vec<ContentBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// Generic response structure
#[derive(Debug, Deserialize)]
pub struct LLMResponse {
    pub id: String,
    pub content: Vec<ContentBlock>,
}
