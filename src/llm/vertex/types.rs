use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct VertexRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<SystemInstruction>,
    pub contents: Vec<VertexMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct SystemInstruction {
    pub parts: Parts,
}

#[derive(Debug, Serialize)]
pub struct Parts {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VertexMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<VertexPart>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VertexPart {
    #[serde(rename = "functionCall")]
    pub function_call: Option<VertexFunctionCall>,
    pub text: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GenerationConfig {
    pub temperature: f32,
    pub max_output_tokens: usize,
}

#[derive(Debug, Deserialize)]
pub struct VertexResponse {
    pub candidates: Vec<VertexCandidate>,
    #[serde(rename = "usageMetadata")]
    pub usage_metadata: Option<VertexUsageMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct VertexUsageMetadata {
    #[serde(rename = "promptTokenCount")]
    pub prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount")]
    pub candidates_token_count: u32,
    #[allow(dead_code)]
    #[serde(rename = "totalTokenCount")]
    pub total_token_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct VertexCandidate {
    pub content: VertexContent,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VertexContent {
    pub parts: Vec<VertexPart>,
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VertexFunctionCall {
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct VertexErrorResponse {
    #[allow(dead_code)]
    pub error: VertexError,
}

#[derive(Debug, Deserialize)]
pub struct VertexError {
    #[allow(dead_code)]
    pub message: String,
    #[allow(dead_code)]
    pub code: Option<i32>,
}
