use crate::llm::{
    types::*, ApiError, ApiErrorContext, LLMProvider, RateLimitHandler, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIChatMessage>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIChatMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamResponse {
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIDelta,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    call_type: Option<String>,
    #[serde(default)]
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: OpenAIError,
}

#[derive(Debug, Deserialize)]
struct OpenAIError {
    message: String,
    #[serde(rename = "type")]
    code: Option<String>,
}

/// Rate limit information extracted from response headers
#[derive(Debug)]
struct OpenAIRateLimitInfo {
    requests_limit: Option<u32>,
    requests_remaining: Option<u32>,
    requests_reset: Option<Duration>,
    tokens_limit: Option<u32>,
    tokens_remaining: Option<u32>,
    tokens_reset: Option<Duration>,
}

impl RateLimitHandler for OpenAIRateLimitInfo {
    fn from_response(response: &Response) -> Self {
        let headers = response.headers();

        fn parse_header<T: std::str::FromStr>(
            headers: &reqwest::header::HeaderMap,
            name: &str,
        ) -> Option<T> {
            headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse().ok())
        }

        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {
            headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| {
                    // Parse OpenAI's duration format (e.g., "1s", "6m0s")
                    let mut seconds = 0u64;
                    let mut current_num = String::new();

                    for c in s.chars() {
                        match c {
                            '0'..='9' => current_num.push(c),
                            'm' => {
                                if let Ok(mins) = current_num.parse::<u64>() {
                                    seconds += mins * 60;
                                }
                                current_num.clear();
                            }
                            's' => {
                                if let Ok(secs) = current_num.parse::<u64>() {
                                    seconds += secs;
                                }
                                current_num.clear();
                            }
                            _ => current_num.clear(),
                        }
                    }
                    Some(Duration::from_secs(seconds))
                })
        }

        Self {
            requests_limit: parse_header(headers, "x-ratelimit-limit-requests"),
            requests_remaining: parse_header(headers, "x-ratelimit-remaining-requests"),
            requests_reset: parse_duration(headers, "x-ratelimit-reset-requests"),
            tokens_limit: parse_header(headers, "x-ratelimit-limit-tokens"),
            tokens_remaining: parse_header(headers, "x-ratelimit-remaining-tokens"),
            tokens_reset: parse_duration(headers, "x-ratelimit-reset-tokens"),
        }
    }

    fn get_retry_delay(&self) -> Duration {
        // Take the longer of the two reset times if both are present
        let mut delay = Duration::from_secs(2); // Default fallback

        if let Some(requests_reset) = self.requests_reset {
            delay = delay.max(requests_reset);
        }

        if let Some(tokens_reset) = self.tokens_reset {
            delay = delay.max(tokens_reset);
        }

        // Add a small buffer
        delay + Duration::from_secs(1)
    }

    fn log_status(&self) {
        debug!(
            "OpenAI Rate limits - Requests: {}/{} (reset in: {}s), Tokens: {}/{} (reset in: {}s)",
            self.requests_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.requests_limit
                .map_or("?".to_string(), |l| l.to_string()),
            self.requests_reset.map_or(0, |d| d.as_secs()),
            self.tokens_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.tokens_limit.map_or("?".to_string(), |l| l.to_string()),
            self.tokens_reset.map_or(0, |d| d.as_secs()),
        );
    }
}

pub struct OpenAIClient {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl OpenAIClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1/chat/completions".to_string(),
            model,
        }
    }

    fn convert_message(message: &Message) -> OpenAIChatMessage {
        OpenAIChatMessage {
            role: match message.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            },
            content: match &message.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Structured(_) => {
                    // For now, we'll just convert structured content to a simple text message
                    // This could be enhanced to handle OpenAI's specific formats
                    "[Structured content not supported]".to_string()
                }
            },
        }
    }

    async fn send_with_retry(
        &self,
        request: &OpenAIRequest,
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
                    let rate_limits = e
                        .downcast_ref::<ApiErrorContext<OpenAIRateLimitInfo>>()
                        .and_then(|ctx| ctx.rate_limits.as_ref());

                    match e.downcast_ref::<ApiError>() {
                        Some(ApiError::RateLimit(_)) => {
                            if let Some(rate_limits) = rate_limits {
                                if attempts < max_retries {
                                    attempts += 1;
                                    let delay = rate_limits.get_retry_delay();
                                    warn!(
                                        "OpenAI rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                                        attempts,
                                        max_retries,
                                        delay.as_secs()
                                    );
                                    sleep(delay).await;
                                    continue;
                                }
                            }
                        }
                        Some(ApiError::ServiceError(_)) | Some(ApiError::NetworkError(_)) => {
                            if attempts < max_retries {
                                attempts += 1;
                                let delay = Duration::from_secs(2u64.pow(attempts - 1));
                                warn!(
                                    "Error: {} (attempt {}/{}), retrying in {} seconds",
                                    e,
                                    attempts,
                                    max_retries,
                                    delay.as_secs()
                                );
                                sleep(delay).await;
                                continue;
                            }
                        }
                        _ => {} // Don't retry other types of errors
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn try_send_request(
        &self,
        request: &OpenAIRequest,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let rate_limits = OpenAIRateLimitInfo::from_response(&response);

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        if !status.is_success() {
            let error = if let Ok(error_response) =
                serde_json::from_str::<OpenAIErrorResponse>(&response_text)
            {
                match (status, error_response.error.code.as_deref()) {
                    (StatusCode::TOO_MANY_REQUESTS, _) => {
                        ApiError::RateLimit(error_response.error.message)
                    }
                    (StatusCode::UNAUTHORIZED, _) => {
                        ApiError::Authentication(error_response.error.message)
                    }
                    (StatusCode::BAD_REQUEST, _) => {
                        ApiError::InvalidRequest(error_response.error.message)
                    }
                    (status, _) if status.is_server_error() => {
                        ApiError::ServiceError(error_response.error.message)
                    }
                    _ => ApiError::Unknown(error_response.error.message),
                }
            } else {
                ApiError::Unknown(format!("Status {}: {}", status, response_text))
            };

            return Err(ApiErrorContext {
                error,
                rate_limits: Some(rate_limits),
            }
            .into());
        }

        // Parse the successful response
        let openai_response: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        // TODO: Handle tools
        let response = LLMResponse {
            content: vec![ContentBlock::Text {
                text: openai_response.choices[0].message.content.clone(),
            }],
        };

        Ok((response, rate_limits))
    }

    async fn try_send_request_streaming(
        &self,
        request: &OpenAIRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        let mut response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let mut accumulated_content: Option<String> = None;
        let mut accumulated_tool_calls: Vec<ContentBlock> = Vec::new();
        let mut current_tool: Option<OpenAIToolCallDelta> = None;

        while let Some(chunk) = response.chunk().await? {
            if let Ok(chunk_str) = std::str::from_utf8(&chunk) {
                if let Ok(chunk_response) = serde_json::from_str::<OpenAIStreamResponse>(chunk_str)
                {
                    if let Some(delta) = chunk_response.choices.get(0) {
                        // Handle content streaming
                        if let Some(content) = &delta.delta.content {
                            streaming_callback(content)?;
                            accumulated_content =
                                Some(accumulated_content.unwrap_or_default() + content);
                        }

                        // Handle tool calls
                        if let Some(tool_calls) = &delta.delta.tool_calls {
                            for tool_call in tool_calls {
                                if let Some(function) = &tool_call.function {
                                    if tool_call.id.is_some() {
                                        // New tool call
                                        if let Some(prev_tool) = current_tool.take() {
                                            accumulated_tool_calls
                                                .push(Self::build_tool_block(prev_tool)?);
                                        }
                                        current_tool = Some(tool_call.clone());
                                    } else if let Some(curr_tool) = &mut current_tool {
                                        // Update existing tool
                                        if let Some(args) = &function.arguments {
                                            if let Some(ref mut curr_func) = curr_tool.function {
                                                curr_func.arguments = Some(
                                                    curr_func
                                                        .arguments
                                                        .as_ref()
                                                        .unwrap_or(&String::new())
                                                        .clone()
                                                        + args,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Handle completion
                        if delta.finish_reason.is_some() {
                            if let Some(tool) = current_tool.take() {
                                accumulated_tool_calls.push(Self::build_tool_block(tool)?);
                            }
                        }
                    }
                }
            }
        }

        let mut content = Vec::new();
        if let Some(text) = accumulated_content {
            content.push(ContentBlock::Text { text });
        }
        content.extend(accumulated_tool_calls);

        Ok((
            LLMResponse { content },
            OpenAIRateLimitInfo::from_response(&response),
        ))
    }

    fn build_tool_block(tool: OpenAIToolCallDelta) -> Result<ContentBlock> {
        let function = tool
            .function
            .ok_or_else(|| anyhow::anyhow!("Tool call without function"))?;
        let name = function
            .name
            .ok_or_else(|| anyhow::anyhow!("Function without name"))?;
        let args = function.arguments.unwrap_or_default();

        Ok(ContentBlock::ToolUse {
            id: tool.id.unwrap_or_default(),
            name,
            input: serde_json::from_str(&args)
                .map_err(|e| anyhow::anyhow!("Invalid JSON in arguments: {}", e))?,
        })
    }
}

#[async_trait]
impl LLMProvider for OpenAIClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OpenAIChatMessage> = Vec::new();

        // Add system message
        messages.push(OpenAIChatMessage {
            role: "system".to_string(),
            content: request.system_prompt,
        });

        // Add conversation messages
        messages.extend(request.messages.iter().map(Self::convert_message));

        let openai_request = OpenAIRequest {
            model: self.model.clone(),
            messages,
            temperature: 1.0,
            stream: streaming_callback.map(|_| true),
            tool_choice: match &request.tools {
                Some(_) => Some(serde_json::json!({
                    "type": "any",
                })),
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

        self.send_with_retry(&openai_request, streaming_callback, 3)
            .await
    }
}
