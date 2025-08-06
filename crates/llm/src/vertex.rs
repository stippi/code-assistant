use crate::{
    recording::APIRecorder, types::*, utils, ApiError, LLMProvider, RateLimitHandler,
    StreamingCallback, StreamingChunk,
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
    inline_data: Option<VertexInlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<VertexFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<VertexFunctionResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexInlineData {
    mime_type: String,
    data: String,
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
    #[serde(rename = "modelVersion")]
    #[allow(dead_code)]
    model_version: Option<String>,
    #[serde(rename = "responseId")]
    #[allow(dead_code)]
    response_id: Option<String>,
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
    #[serde(rename = "thoughtsTokenCount")]
    #[allow(dead_code)]
    thoughts_token_count: Option<u32>,
    #[serde(rename = "promptTokensDetails")]
    #[allow(dead_code)]
    prompt_tokens_details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct VertexCandidate {
    content: VertexContent,
    #[serde(rename = "finishReason")]
    #[allow(dead_code)]
    finish_reason: Option<String>,
    #[allow(dead_code)]
    index: Option<u32>,
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
    recorder: Option<APIRecorder>,
}

impl VertexClient {
    pub fn default_base_url() -> String {
        "https://generativelanguage.googleapis.com/v1beta".to_string()
        //"https://aiplatform.googleapis.com/v1/publishers/google".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url,
            recorder: None,
        }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        api_key: String,
        model: String,
        base_url: String,
        recording_path: P,
    ) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url,
            recorder: Some(APIRecorder::new(recording_path)),
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
                inline_data: None,
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
                        inline_data: None,
                        thought: Some(true),
                        thought_signature: Some(signature.clone()),
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::Text { text } => Some(VertexPart {
                        text: Some(text.clone()),
                        inline_data: None,
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::Image { media_type, data } => Some(VertexPart {
                        text: None,
                        inline_data: Some(VertexInlineData {
                            mime_type: media_type.clone(),
                            data: data.clone(),
                        }),
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::ToolUse { name, input, .. } => Some(VertexPart {
                        text: None,
                        inline_data: None,
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
                        inline_data: None,
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
        request_id: u64,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            match if let Some(callback) = streaming_callback {
                self.try_send_request_streaming(request, request_id, callback)
                    .await
            } else {
                self.try_send_request(request, request_id).await
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
                        streaming_callback,
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
        request_id: u64,
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
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {e}")))?;

        // Convert to our generic LLMResponse format
        let mut tool_counter = 0;
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
                                tool_counter += 1;
                                let tool_id = format!("tool-{}-{}", request_id, tool_counter);
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
                    cache_read_input_tokens: usage_metadata
                        .cached_content_token_count
                        .unwrap_or_default(),
                }
            } else {
                Usage::default()
            },
            rate_limit_info: None,
        };

        Ok((response, rate_limits))
    }

    async fn try_send_request_streaming(
        &self,
        request: &VertexRequest,
        request_id: u64,
        streaming_callback: &StreamingCallback,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        // Start recording if a recorder is available
        if let Some(recorder) = &self.recorder {
            let request_json = serde_json::to_value(request)?;
            recorder.start_recording(request_json)?;
        }
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
        let mut tool_counter = 0;

        // Helper function to process SSE lines
        let process_sse_line = |line: &str,
                                blocks: &mut Vec<ContentBlock>,
                                usage: &mut Option<VertexUsageMetadata>,
                                callback: &StreamingCallback,
                                tool_counter: &mut u32,
                                request_id: u64,
                                recorder: &Option<APIRecorder>|
         -> Result<()> {
            if let Some(data) = line.strip_prefix("data: ") {
                debug!("Received data line: {}", data);
                // Record the chunk if recorder is available
                if let Some(recorder) = recorder {
                    recorder.record_chunk(data)?;
                }
                if let Ok(response) = serde_json::from_str::<VertexResponse>(data) {
                    // Always update usage metadata if present (including final responses)
                    if let Some(usage_metadata) = response.usage_metadata {
                        *usage = Some(usage_metadata);
                    }
                    // Process candidates and their content parts if present
                    if let Some(candidate) = response.candidates.first() {
                        for part in &candidate.content.parts {
                            if let Some(text) = &part.text {
                                // Check if this is a thinking part
                                if part.thought == Some(true) {
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
                                    // Stream thinking content
                                    callback(&StreamingChunk::Thinking(text.clone()))?;
                                } else {
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
                                    // Regular text content
                                    callback(&StreamingChunk::Text(text.clone()))?;
                                }
                            } else if let Some(function_call) = &part.function_call {
                                // Generate a tool ID using request_id and counter
                                *tool_counter += 1;
                                let tool_id = format!("tool-{}-{}", request_id, *tool_counter);

                                // Always create a new tool use block (they don't get extended)
                                blocks.push(ContentBlock::ToolUse {
                                    id: tool_id.clone(),
                                    name: function_call.name.clone(),
                                    input: function_call.args.clone(),
                                });

                                // Stream the JSON input for tools
                                if let Ok(args_str) = serde_json::to_string(&function_call.args) {
                                    callback(&StreamingChunk::InputJson {
                                        content: args_str,
                                        tool_name: Some(function_call.name.clone()),
                                        tool_id: Some(tool_id),
                                    })?;
                                }
                            }
                        }
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
                        match process_sse_line(
                            &line_buffer,
                            &mut content_blocks,
                            &mut last_usage,
                            streaming_callback,
                            &mut tool_counter,
                            request_id,
                            &self.recorder,
                        ) {
                            Ok(()) => {
                                line_buffer.clear();
                                continue;
                            }
                            Err(e) if e.to_string().contains("Tool limit reached") => {
                                debug!("Tool limit reached, stopping streaming early. Collected {} blocks so far", content_blocks.len());

                                line_buffer.clear(); // Make sure we stop processing
                                break; // Exit chunk processing loop early
                            }
                            Err(e) => return Err(e), // Propagate other errors
                        }
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
                &mut tool_counter,
                request_id,
                &self.recorder,
            )?;
        }

        // Send StreamingComplete to indicate streaming has finished
        streaming_callback(&StreamingChunk::StreamingComplete)?;

        // End recording if a recorder is available
        if let Some(recorder) = &self.recorder {
            recorder.end_recording()?;
        }

        Ok((
            LLMResponse {
                content: content_blocks,
                usage: if let Some(usage_metadata) = last_usage {
                    Usage {
                        input_tokens: usage_metadata.prompt_token_count,
                        output_tokens: usage_metadata.candidates_token_count,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: usage_metadata
                            .cached_content_token_count
                            .unwrap_or_default(),
                    }
                } else {
                    Usage::default()
                },
                rate_limit_info: None,
            },
            rate_limits,
        ))
    }
}

#[async_trait]
impl LLMProvider for VertexClient {
    async fn send_message(
        &mut self,
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

        let request_id = request.request_id;

        self.send_with_retry(&vertex_request, request_id, streaming_callback, 3)
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
