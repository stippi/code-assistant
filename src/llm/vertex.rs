use crate::llm::{
    types::*, utils, ApiError, LLMProvider, RateLimitHandler, StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, trace};

#[derive(Debug, Serialize)]
struct VertexRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    contents: Vec<VertexMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct SystemInstruction {
    parts: Parts,
}

#[derive(Debug, Serialize)]
struct Parts {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<VertexPart>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexPart {
    #[serde(rename = "functionCall")]
    function_call: Option<VertexFunctionCall>,
    #[serde(rename = "functionResponse")]
    function_response: Option<VertexFunctionResponse>,
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f32,
    max_output_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct VertexResponse {
    candidates: Vec<VertexCandidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<VertexUsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct VertexUsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[allow(dead_code)]
    #[serde(rename = "totalTokenCount")]
    total_token_count: u32,
}

#[derive(Debug, Deserialize)]
struct VertexCandidate {
    content: VertexContent,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexContent {
    parts: Vec<VertexPart>,
    role: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexFunctionResponse {
    name: String,
    response: serde_json::Value,
}

/// Rate limit information extracted from response headers
#[derive(Debug)]
struct VertexRateLimitInfo {
    // TODO: Add actual rate limit fields once we know what headers Vertex AI uses
    requests_remaining: Option<u32>,
    #[allow(dead_code)]
    requests_reset: Option<Duration>,
}

impl RateLimitHandler for VertexRateLimitInfo {
    fn from_response(_response: &Response) -> Self {
        // TODO: Parse actual rate limit headers once we know what Vertex AI provides
        Self {
            requests_remaining: None,
            requests_reset: None,
        }
    }

    fn get_retry_delay(&self) -> Duration {
        // Default exponential backoff strategy
        Duration::from_secs(2)
    }

    fn log_status(&self) {
        debug!(
            "Vertex AI Rate limits - Requests remaining: {}",
            self.requests_remaining
                .map_or("unknown".to_string(), |r| r.to_string())
        );
    }
}

pub struct VertexClient {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl VertexClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
        }
    }

    #[cfg(test)]
    pub fn new_with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url,
        }
    }

    fn get_url(&self, streaming: bool) -> String {
        if streaming {
            format!(
                "{}/models/{}:streamGenerateContent",
                self.base_url, self.model
            )
        } else {
            format!("{}/models/{}:generateContent", self.base_url, self.model)
        }
    }

    fn convert_message(message: &Message) -> VertexMessage {
        let role = Some(match message.role {
            MessageRole::User => "user".to_string(),
            MessageRole::Assistant => "model".to_string(),
        });

        let parts = match &message.content {
            MessageContent::Text(text) => vec![VertexPart {
                text: Some(text.clone()),
                function_call: None,
                function_response: None,
            }],
            MessageContent::Structured(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(VertexPart {
                        text: Some(text.clone()),
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::ToolUse { name, input, .. } => Some(VertexPart {
                        text: None,
                        function_call: Some(VertexFunctionCall {
                            name: name.clone(),
                            args: input.clone(),
                        }),
                        function_response: None,
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => Some(VertexPart {
                        text: None,
                        function_call: None,
                        function_response: Some(VertexFunctionResponse {
                            name: tool_use_id.clone(), // TODO: Should be function name
                            response: serde_json::Value::String(content.clone()),
                        }),
                    }),
                    _ => None,
                })
                .collect(),
        };

        VertexMessage { role, parts }
    }

    async fn send_with_retry(
        &self,
        request: &VertexRequest,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            match if let Some(callback) = streaming_callback {
                self.try_send_request_streaming(request, callback).await
            } else {
                self.try_send_request(request).await
            } {
                Ok((response, rate_limits)) => {
                    rate_limits.log_status();
                    return Ok(response);
                }
                Err(e) => {
                    if utils::handle_retryable_error::<VertexRateLimitInfo>(
                        &e,
                        attempts,
                        max_retries,
                    )
                    .await
                    {
                        attempts += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn try_send_request(
        &self,
        request: &VertexRequest,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let url = self.get_url(false);

        trace!(
            "Sending Vertex request to {}:\n{}",
            self.model,
            serde_json::to_string_pretty(request)?
        );

        let response = self
            .client
            .post(&url)
            .query(&[("key", &self.api_key)])
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let response = utils::check_response_error::<VertexRateLimitInfo>(response).await?;
        let rate_limits = VertexRateLimitInfo::from_response(&response);

        trace!("Response headers: {:?}", response.headers());

        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        trace!(
            "Vertex response: {}",
            serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(
                &response_text
            )?)?
        );

        let vertex_response: VertexResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        let response = LLMResponse {
            content: vertex_response
                .candidates
                .into_iter()
                .flat_map(|candidate| {
                    candidate
                        .content
                        .parts
                        .into_iter()
                        .enumerate()
                        .map(|(index, part)| {
                            if let Some(function_call) = part.function_call {
                                ContentBlock::ToolUse {
                                    id: format!("tool-{}", index), // Generate a unique ID
                                    name: function_call.name,
                                    input: function_call.args,
                                }
                            } else if let Some(text) = part.text {
                                ContentBlock::Text { text }
                            } else {
                                // Fallback if neither function_call nor text is present
                                ContentBlock::Text {
                                    text: "Empty response part".to_string(),
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
            usage: Usage {
                input_tokens: vertex_response
                    .usage_metadata
                    .as_ref()
                    .map(|u| u.prompt_token_count)
                    .unwrap_or(0),
                output_tokens: vertex_response
                    .usage_metadata
                    .as_ref()
                    .map(|u| u.candidates_token_count)
                    .unwrap_or(0),
                // Vertex doesn't support our caching markers, so these fields are 0
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };

        Ok((response, rate_limits))
    }

    async fn try_send_request_streaming(
        &self,
        request: &VertexRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let response = self
            .client
            .post(&self.get_url(true))
            .query(&[("key", &self.api_key), ("alt", &"sse".to_string())])
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let mut response = utils::check_response_error::<VertexRateLimitInfo>(response).await?;
        let rate_limits = VertexRateLimitInfo::from_response(&response);

        let mut content_blocks = Vec::new();
        let mut current_text = String::new();
        let mut last_usage: Option<VertexUsageMetadata> = None;
        let mut line_buffer = String::new();

        while let Some(chunk) = response.chunk().await? {
            let chunk_str = std::str::from_utf8(&chunk)?;

            for c in chunk_str.chars() {
                if c == '\n' {
                    if !line_buffer.is_empty() {
                        if let Some(data) = line_buffer.strip_prefix("data: ") {
                            if let Ok(response) = serde_json::from_str::<VertexResponse>(data) {
                                if let Some(candidate) = response.candidates.first() {
                                    for part in &candidate.content.parts {
                                        if let Some(text) = &part.text {
                                            streaming_callback(&StreamingChunk::Text(
                                                text.clone(),
                                            ))?;
                                            current_text.push_str(text);
                                        } else if let Some(function_call) = &part.function_call {
                                            // If we have accumulated text, push it as a content block
                                            if !current_text.is_empty() {
                                                content_blocks.push(ContentBlock::Text {
                                                    text: current_text.clone(),
                                                });
                                                current_text.clear();
                                            }

                                            content_blocks.push(ContentBlock::ToolUse {
                                                id: format!("tool-{}", content_blocks.len()),
                                                name: function_call.name.clone(),
                                                input: function_call.args.clone(),
                                            });
                                        }
                                    }
                                }
                                if let Some(usage) = response.usage_metadata {
                                    last_usage = Some(usage);
                                }
                            }
                        }
                        line_buffer.clear();
                    }
                } else {
                    line_buffer.push(c);
                }
            }
        }

        // Process any remaining data in the buffer
        if !line_buffer.is_empty() {
            if let Some(data) = line_buffer.strip_prefix("data: ") {
                if let Ok(response) = serde_json::from_str::<VertexResponse>(data) {
                    if let Some(usage) = response.usage_metadata {
                        last_usage = Some(usage);
                    }
                }
            }
        }

        // Push any remaining text as a final content block
        if !current_text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: current_text });
        }

        Ok((
            LLMResponse {
                content: content_blocks,
                usage: Usage {
                    input_tokens: last_usage
                        .as_ref()
                        .map(|u| u.prompt_token_count)
                        .unwrap_or(0),
                    output_tokens: last_usage
                        .as_ref()
                        .map(|u| u.candidates_token_count)
                        .unwrap_or(0),
                    // Vertex doesn't support our caching markers, so these fields are 0
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            },
            rate_limits,
        ))
    }
}

#[async_trait]
impl LLMProvider for VertexClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut contents = Vec::new();

        // Convert messages
        contents.extend(request.messages.iter().map(Self::convert_message));

        let vertex_request = VertexRequest {
            system_instruction: Some(SystemInstruction {
                parts: Parts {
                    text: request.system_prompt,
                },
            }),
            contents,
            generation_config: Some(GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            }),
            tools: request.tools.map(|tools| {
                vec![json!({
                    "function_declarations": tools.into_iter().map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        })
                    }).collect::<Vec<_>>()
                })]
            }),
            tool_config: Some(json!({
                "function_calling_config": {
                    "mode": "ANY",
                }
            })),
        };

        self.send_with_retry(&vertex_request, streaming_callback, 3)
            .await
    }
}

/*
Communicating tool call results back to LLM (including parallel function calls):
Note, there is no ID associated with each function call/result, only the order.

```json
{
    "role": "user",
    "parts": {
        "text": "What is difference in temperature in New Delhi and San Francisco?"
    }
},
{
    "role": "model",
    "parts": [
        {
            "functionCall": {
                "name": "get_current_weather",
                "args": {
                    "location": "New Delhi"
                }
            }
        },
        {
            "functionCall": {
                "name": "get_current_weather",
                "args": {
                    "location": "San Francisco"
                }
            }
        }
    ]
},
{
    "role": "user",
    "parts": [
        {
            "functionResponse": {
                "name": "get_current_weather",
                "response": {
                    "temperature": 30.5,
                    "unit": "C"
                }
            }
        },
        {
            "functionResponse": {
                "name": "get_current_weather",
                "response": {
                    "temperature": 20,
                    "unit": "C"
                }
            }
        }
    ]
}
```
*/
