
use crate::llm::{
    types::*, utils, ApiError, LLMProvider, RateLimitHandler, StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

impl OpenRouterRequest {
    fn into_streaming(mut self) -> Self {
        self.stream = Some(true);
        self
    }

    fn into_non_streaming(mut self) -> Self {
        self.stream = None;
        self
    }
}

#[derive(Debug, Serialize, Clone)]
struct OpenRouterRequest {
    model: String,
    messages: Vec<OpenRouterMessage>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Clone)]
struct OpenRouterMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenRouterToolCall>>,
}

#[derive(Debug, Serialize, Clone)]
struct OpenRouterToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenRouterFunction,
}

#[derive(Debug, Serialize, Clone)]
struct OpenRouterFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    choices: Vec<OpenRouterChoice>,
    usage: OpenRouterUsage,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChoice {
    message: OpenRouterResponseMessage,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponseMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenRouterStreamResponse {
    choices: Vec<OpenRouterStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterStreamChoice {
    delta: OpenRouterDelta,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterDelta {
    #[serde(default)]
    content: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    role: Option<String>,
}

/// Rate limit information for OpenRouter
///
/// Note: OpenRouter doesn't provide detailed rate limit information in response headers.
/// Instead, they provide this information through a separate API endpoint.
/// For simplicity, we'll use a basic implementation that handles rate limiting
/// with exponential backoff when we encounter rate limit errors.
#[derive(Debug)]
struct OpenRouterRateLimitInfo {
    status_code: StatusCode,
}

impl RateLimitHandler for OpenRouterRateLimitInfo {
    fn from_response(response: &Response) -> Self {
        Self {
            status_code: response.status(),
        }
    }

    fn get_retry_delay(&self) -> Duration {
        // OpenRouter rate limits are based on credits remaining
        // For simplicity, we'll use a fixed delay for rate limit errors
        if self.status_code == StatusCode::TOO_MANY_REQUESTS || self.status_code == StatusCode::PAYMENT_REQUIRED {
            // Use a longer delay for rate limit errors
            Duration::from_secs(5)
        } else if self.status_code.is_server_error() {
            // Use a shorter delay for server errors
            Duration::from_secs(2)
        } else {
            // Default delay for other errors
            Duration::from_secs(1)
        }
    }

    fn log_status(&self) {
        // Since we don't have detailed rate limit information, just log the status code
        if self.status_code == StatusCode::TOO_MANY_REQUESTS || self.status_code == StatusCode::PAYMENT_REQUIRED {
            debug!("OpenRouter rate limit hit. Status code: {}", self.status_code);
        } else if !self.status_code.is_success() {
            debug!("OpenRouter request status: {}", self.status_code);
        }
    }
}

pub struct OpenRouterClient {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

// Process a Server-Sent Events (SSE) line from the streaming response
fn process_sse_line(line: &str, callback: &StreamingCallback) -> Result<Option<String>> {
    if let Some(data) = line.strip_prefix("data: ") {
        // Skip "[DONE]" message
        if data == "[DONE]" {
            return Ok(None);
        }

        if let Ok(chunk_response) = serde_json::from_str::<OpenRouterStreamResponse>(data) {
            if let Some(delta) = chunk_response.choices.get(0) {
                // Handle content streaming
                if let Some(content) = &delta.delta.content {
                    callback(&StreamingChunk::Text(content.clone()))?;
                    return Ok(Some(content.clone()));
                }
            }
        }
    }
    Ok(None)
}

impl OpenRouterClient {
    async fn send_with_retry(
        &self,
        request: &OpenRouterRequest,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            let result = if let Some(callback) = streaming_callback {
                self.try_send_request_streaming(request, callback).await
            } else {
                self.try_send_request(request).await
            };

            match result {
                Ok((response, rate_limits)) => {
                    rate_limits.log_status();
                    return Ok(response);
                }
                Err(e) => {
                    if utils::handle_retryable_error::<OpenRouterRateLimitInfo>(
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
    pub fn default_base_url() -> String {
        "https://openrouter.ai/api/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
            model
        }
    }

    async fn send_request(&self, request: &OpenRouterRequest) -> Result<Response> {
        let url = format!("{}/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(request)
            .send()
            .await?;

        let response = utils::check_response_error::<OpenRouterRateLimitInfo>(response).await?;
        Ok(response)
    }

    async fn try_send_request_streaming(
        &self,
        request: &OpenRouterRequest,
        callback: &StreamingCallback,
    ) -> Result<(LLMResponse, OpenRouterRateLimitInfo)> {
        debug!("Sending streaming request to OpenRouter");
        let request = request.clone().into_streaming();
        let mut response = self.send_request(&request).await?;

        let mut accumulated_content = String::new();
        let mut line_buffer = String::new();

        // Process the streaming response
        while let Some(chunk) = response.chunk().await? {
            let chunk_str = std::str::from_utf8(&chunk)?;

            for c in chunk_str.chars() {
                if c == '\n' {
                    if !line_buffer.is_empty() {
                        if let Some(content) = process_sse_line(&line_buffer, callback)? {
                            accumulated_content.push_str(&content);
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
            if let Some(content) = process_sse_line(&line_buffer, callback)? {
                accumulated_content.push_str(&content);
            }
        }

        // Return the final response with rate limits
        let rate_limits = OpenRouterRateLimitInfo::from_response(&response);
        Ok((LLMResponse {
            content: vec![ContentBlock::Text { text: accumulated_content }],
            usage: Usage {
                input_tokens: 0,  // We don't have this information
                output_tokens: 0, // We don't have this information
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        }, rate_limits))
    }

    async fn try_send_request(
        &self,
        request: &OpenRouterRequest,
    ) -> Result<(LLMResponse, OpenRouterRateLimitInfo)> {
        let request = request.clone().into_non_streaming();
        let response = self.send_request(&request).await?;
        let rate_limits = OpenRouterRateLimitInfo::from_response(&response);

        // Parse the response as JSON
        let response_text = response.text().await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        // Parse the JSON response
        let openrouter_response: OpenRouterResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Extract the content
        if let Some(choice) = openrouter_response.choices.first() {
            let content = choice.message.content.clone();

            Ok((LLMResponse {
                content: vec![ContentBlock::Text { text: content }],
                usage: Usage {
                    input_tokens: openrouter_response.usage.prompt_tokens,
                    output_tokens: openrouter_response.usage.completion_tokens,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            }, rate_limits))
        } else {
            Err(ApiError::Unknown("No choices in response".to_string()).into())
        }
    }
}

#[async_trait]
impl LLMProvider for OpenRouterClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OpenRouterMessage> = Vec::new();

        // Add system message
        messages.push(OpenRouterMessage {
            role: "system".to_string(),
            content: request.system_prompt,
            tool_calls: None,
        });

        // Add conversation messages
        messages.extend(request.messages.iter().map(|msg| {
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            }.to_string();

            let content = match &msg.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Structured(blocks) => {
                    // Concatenate all text blocks into the content string
                    blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text),
                            _ => None,
                        })
                        .cloned()
                        .collect::<Vec<String>>()
                        .join("")
                }
            };

            let tool_calls = match &msg.content {
                MessageContent::Structured(blocks) => {
                    let tool_calls: Vec<OpenRouterToolCall> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolUse { id, name, input } => Some(OpenRouterToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenRouterFunction {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            }),
                            _ => None,
                        })
                        .collect();
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    }
                }
                _ => None,
            };

            OpenRouterMessage {
                role,
                content,
                tool_calls,
            }
        }));

        let openrouter_request = OpenRouterRequest {
            model: self.model.clone(),
            messages,
            temperature: 1.0,
            stream: streaming_callback.map(|_| true),
            tool_choice: match &request.tools {
                Some(_) => Some(serde_json::json!("required")),
                _ => None,
            },
            tools: request.tools.map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.parameters
                            }
                        })
                    })
                    .collect()
            }),
        };

        self.send_with_retry(&openrouter_request, streaming_callback, 3).await
    }
}
