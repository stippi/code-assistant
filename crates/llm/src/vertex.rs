use crate::{
    types::*, utils, ApiError, LLMProvider, RateLimitHandler, StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, trace, warn};

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
#[serde(rename_all = "camelCase")]
struct VertexPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<VertexFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<VertexFunctionResponse>,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f32,
    max_output_tokens: usize,
    response_mime_type: String,
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
    #[serde(rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<u32>,
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

// Tool ID generation trait and implementations
pub trait ToolIDGenerator {
    fn generate_id(&self, name: &str) -> String;
}

/// Default implementation using an atomic counter
pub struct DefaultToolIDGenerator {
    counter: std::sync::atomic::AtomicU64,
}

impl DefaultToolIDGenerator {
    pub fn new() -> Self {
        Self {
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl Default for DefaultToolIDGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolIDGenerator for DefaultToolIDGenerator {
    fn generate_id(&self, name: &str) -> String {
        let counter = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("tool-{}-{}", name, counter)
    }
}

/// Fixed pattern implementation for testing
pub struct FixedToolIDGenerator {
    id_pattern: String,
}

impl FixedToolIDGenerator {
    pub fn new(id_pattern: String) -> Self {
        Self { id_pattern }
    }
}

impl ToolIDGenerator for FixedToolIDGenerator {
    fn generate_id(&self, name: &str) -> String {
        format!("tool-{}-{}", name, self.id_pattern)
    }
}

pub struct VertexClient {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    tool_id_generator: Box<dyn ToolIDGenerator + Send + Sync>,
}

impl VertexClient {
    pub fn default_base_url() -> String {
        "https://generativelanguage.googleapis.com/v1beta".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self::new_with_tool_id_generator(
            api_key,
            model,
            base_url,
            Box::new(DefaultToolIDGenerator::new()),
        )
    }

    pub fn new_with_tool_id_generator(
        api_key: String,
        model: String,
        base_url: String,
        tool_id_generator: Box<dyn ToolIDGenerator + Send + Sync>,
    ) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url,
            tool_id_generator,
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
                thought: None,
                thought_signature: None,
                function_call: None,
                function_response: None,
            }],
            MessageContent::Structured(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => Some(VertexPart {
                        text: Some(thinking.clone()),
                        thought: Some(true),
                        thought_signature: Some(signature.clone()),
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::Text { text } => Some(VertexPart {
                        text: Some(text.clone()),
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::ToolUse { name, input, .. } => Some(VertexPart {
                        text: None,
                        thought: None,
                        thought_signature: None,
                        function_call: Some(VertexFunctionCall {
                            name: name.clone(),
                            args: input.clone(),
                        }),
                        function_response: None,
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Some(VertexPart {
                        text: None,
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: Some(VertexFunctionResponse {
                            // Extract the function name from the tool_use_id
                            // Format is typically "tool-{name}-{index}"
                            name: tool_use_id
                                .split('-')
                                .nth(1)
                                .unwrap_or(tool_use_id)
                                .to_string(),
                            // Wrap content in a proper JSON object
                            response: json!({ "result": content }),
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
                        .map(|part| {
                            if let Some(function_call) = part.function_call {
                                let tool_id =
                                    self.tool_id_generator.generate_id(&function_call.name);
                                ContentBlock::ToolUse {
                                    id: tool_id,
                                    name: function_call.name,
                                    input: function_call.args,
                                }
                            } else if let Some(text) = part.text {
                                // Check if this is a thinking part
                                if part.thought == Some(true) {
                                    ContentBlock::Thinking {
                                        thinking: text,
                                        signature: part.thought_signature.unwrap_or_default(),
                                    }
                                } else {
                                    ContentBlock::Text { text }
                                }
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
            usage: if let Some(usage_metadata) = vertex_response.usage_metadata {
                Usage {
                    input_tokens: usage_metadata.prompt_token_count,
                    output_tokens: usage_metadata.candidates_token_count,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: if let Some(cached_content_token_count) =
                        usage_metadata.cached_content_token_count
                    {
                        cached_content_token_count
                    } else {
                        0
                    },
                }
            } else {
                Usage::default()
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
            .post(self.get_url(true))
            .query(&[("key", &self.api_key), ("alt", &"sse".to_string())])
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let mut response = utils::check_response_error::<VertexRateLimitInfo>(response).await?;
        let rate_limits = VertexRateLimitInfo::from_response(&response);

        let mut content_blocks = Vec::new();
        let mut last_usage: Option<VertexUsageMetadata> = None;
        let mut line_buffer = String::new();

        // Helper function to process SSE lines
        let process_sse_line = |line: &str,
                                blocks: &mut Vec<ContentBlock>,
                                usage: &mut Option<VertexUsageMetadata>,
                                callback: &StreamingCallback,
                                tool_id_generator: &Box<dyn ToolIDGenerator + Send + Sync>|
         -> Result<()> {
            if let Some(data) = line.strip_prefix("data: ") {
                debug!("Received data line: {}", data);
                if let Ok(response) = serde_json::from_str::<VertexResponse>(data) {
                    if let Some(candidate) = response.candidates.first() {
                        for part in &candidate.content.parts {
                            if let Some(text) = &part.text {
                                // Check if this is a thinking part
                                if part.thought == Some(true) {
                                    // Stream thinking content
                                    callback(&StreamingChunk::Thinking(text.clone()))?;

                                    // Check if we can extend the last thinking block or need to create a new one
                                    match blocks.last_mut() {
                                        Some(ContentBlock::Thinking {
                                            thinking,
                                            signature,
                                        }) => {
                                            // Extend existing thinking block
                                            thinking.push_str(text);
                                            // Update signature if provided
                                            if let Some(new_signature) = &part.thought_signature {
                                                *signature = new_signature.clone();
                                            }
                                        }
                                        _ => {
                                            // Create new thinking block
                                            blocks.push(ContentBlock::Thinking {
                                                thinking: text.clone(),
                                                signature: part
                                                    .thought_signature
                                                    .clone()
                                                    .unwrap_or_default(),
                                            });
                                        }
                                    }
                                } else {
                                    // Regular text content
                                    callback(&StreamingChunk::Text(text.clone()))?;

                                    // Check if we can extend the last text block or need to create a new one
                                    match blocks.last_mut() {
                                        Some(ContentBlock::Text { text: last_text }) => {
                                            // Extend existing text block
                                            last_text.push_str(text);
                                        }
                                        _ => {
                                            // Create new text block
                                            blocks.push(ContentBlock::Text { text: text.clone() });
                                        }
                                    }
                                }
                            } else if let Some(function_call) = &part.function_call {
                                // Generate a tool ID that includes the function name for later extraction
                                let tool_id = tool_id_generator.generate_id(&function_call.name);

                                // Stream the JSON input for tools
                                if let Ok(args_str) = serde_json::to_string(&function_call.args) {
                                    callback(&StreamingChunk::InputJson {
                                        content: args_str,
                                        tool_name: Some(function_call.name.clone()),
                                        tool_id: Some(tool_id.clone()),
                                    })?;
                                }

                                // Always create a new tool use block (they don't get extended)
                                blocks.push(ContentBlock::ToolUse {
                                    id: tool_id,
                                    name: function_call.name.clone(),
                                    input: function_call.args.clone(),
                                });
                            }
                        }
                    }
                    if let Some(usage_metadata) = response.usage_metadata {
                        *usage = Some(usage_metadata);
                    }
                } else {
                    warn!("Failed to parse Vertex response from data: {}", data);
                }
            } else if line.len() > 1 {
                warn!("Received line without 'data' prefix: {}", line);
            }
            Ok(())
        };

        while let Some(chunk) = response.chunk().await? {
            let chunk_str = std::str::from_utf8(&chunk)?;

            for c in chunk_str.chars() {
                if c == '\n' {
                    if !line_buffer.is_empty() {
                        process_sse_line(
                            &line_buffer,
                            &mut content_blocks,
                            &mut last_usage,
                            streaming_callback,
                            &self.tool_id_generator,
                        )?;
                        line_buffer.clear();
                    }
                } else {
                    line_buffer.push(c);
                }
            }
        }

        // Process any remaining data in the buffer
        if !line_buffer.is_empty() {
            process_sse_line(
                &line_buffer,
                &mut content_blocks,
                &mut last_usage,
                streaming_callback,
                &self.tool_id_generator,
            )?;
        }

        Ok((
            LLMResponse {
                content: content_blocks,
                usage: if let Some(usage_metadata) = last_usage {
                    Usage {
                        input_tokens: usage_metadata.prompt_token_count,
                        output_tokens: usage_metadata.candidates_token_count,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: if let Some(cached_content_token_count) =
                            usage_metadata.cached_content_token_count
                        {
                            cached_content_token_count
                        } else {
                            0
                        },
                    }
                } else {
                    Usage::default()
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
                temperature: 1.,
                max_output_tokens: 65536,
                response_mime_type: "text/plain".to_string(),
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
            tool_config: None,
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
