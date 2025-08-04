use crate::{
    types::*, utils, ApiError, LLMProvider, RateLimitHandler, StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::debug;

/// Trait for providing authentication headers
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>>;
}

/// Trait for customizing requests before sending
pub trait RequestCustomizer: Send + Sync {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()>;
    fn get_additional_headers(&self) -> Vec<(String, String)>;
    fn customize_url(&self, base_url: &str, streaming: bool) -> String;
}

/// Default API key authentication provider
pub struct ApiKeyAuth {
    api_key: String,
}

impl ApiKeyAuth {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl AuthProvider for ApiKeyAuth {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.api_key),
        )])
    }
}

/// Default request customizer for OpenAI API
pub struct DefaultRequestCustomizer;

impl RequestCustomizer for DefaultRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![("Content-Type".to_string(), "application/json".to_string())]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{}/chat/completions", base_url)
    }
}

#[derive(Debug, Serialize, Clone)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize, Clone)]
struct StreamOptions {
    include_usage: bool,
}

impl OpenAIRequest {
    fn into_streaming(mut self) -> Self {
        self.stream = Some(true);
        self.stream_options = Some(StreamOptions {
            include_usage: true,
        });
        self
    }

    fn into_non_streaming(mut self) -> Self {
        self.stream = None;
        self.stream_options = None;
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIChatMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamResponse {
    choices: Vec<OpenAIStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
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
    #[allow(dead_code)]
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIToolCallDelta {
    #[allow(dead_code)]
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    #[serde(default)]
    call_type: Option<String>,
    #[serde(default)]
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
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
            headers.get(name).and_then(|h| h.to_str().ok()).map(|s| {
                // Parse OpenAI's duration format (e.g., "1s", "6m0s", "7.66s", "2m59.56s")
                let mut total_seconds = 0.0f64;
                let mut current_num = String::new();

                for c in s.chars() {
                    match c {
                        '0'..='9' | '.' => current_num.push(c),
                        'm' => {
                            if let Ok(mins) = current_num.parse::<f64>() {
                                total_seconds += mins * 60.0;
                            }
                            current_num.clear();
                        }
                        's' => {
                            if let Ok(secs) = current_num.parse::<f64>() {
                                total_seconds += secs;
                            }
                            current_num.clear();
                        }
                        _ => current_num.clear(),
                    }
                }
                Duration::from_secs_f64(total_seconds)
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
    base_url: String,
    model: String,
    model_temperatures: HashMap<String, f32>,
    // Customization points
    auth_provider: Box<dyn AuthProvider>,
    request_customizer: Box<dyn RequestCustomizer>,
}

impl OpenAIClient {
    pub fn default_base_url() -> String {
        "https://api.openai.com/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        let model_temperatures = Self::default_temperatures();
        Self {
            client: Client::new(),
            base_url,
            model,
            model_temperatures,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
        }
    }

    /// New constructor for customization
    pub fn with_customization(
        model: String,
        base_url: String,
        auth_provider: Box<dyn AuthProvider>,
        request_customizer: Box<dyn RequestCustomizer>,
    ) -> Self {
        let model_temperatures = Self::default_temperatures();
        Self {
            client: Client::new(),
            base_url,
            model,
            model_temperatures,
            auth_provider,
            request_customizer,
        }
    }

    fn get_url(&self, streaming: bool) -> String {
        self.request_customizer
            .customize_url(&self.base_url, streaming)
    }

    /// Returns default temperature mapping for known model IDs.
    fn default_temperatures() -> HashMap<String, f32> {
        let mut m = HashMap::new();
        m.insert("o3".to_string(), 0.7);
        m.insert("o4-mini".to_string(), 0.7);
        m.insert("moonshotai/kimi-k2-instruct".to_string(), 0.6);
        // Add other model defaults as needed
        m
    }

    /// Returns the temperature for the current model, defaulting to 1.0 if not set.
    fn get_temperature(&self) -> f32 {
        self.model_temperatures
            .get(&self.model)
            .cloned()
            .unwrap_or(1.0)
    }

    /// Convert a single message to OpenAI format without special handling for tool results
    pub fn convert_message(message: &Message) -> OpenAIChatMessage {
        let role = match message.role {
            MessageRole::User => "user".to_string(),
            MessageRole::Assistant => "assistant".to_string(),
        };

        let (content, tool_calls) = match &message.content {
            MessageContent::Text(text) => (Some(serde_json::json!(text)), None),
            MessageContent::Structured(blocks) => {
                // Check if we have mixed content (text + images) or just text
                let has_images = blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Image { .. }));
                let has_text = blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { .. }));

                let content = if has_images || (has_text && blocks.len() > 1) {
                    // Use structured content format for mixed content
                    let content_parts: Vec<serde_json::Value> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(serde_json::json!({
                                "type": "text",
                                "text": text
                            })),
                            ContentBlock::Image { media_type, data } => Some(serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data)
                                }
                            })),
                            _ => None,
                        })
                        .collect();

                    if content_parts.is_empty() {
                        Some(serde_json::json!(""))
                    } else {
                        Some(serde_json::json!(content_parts))
                    }
                } else {
                    // Simple text content
                    let text = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text),
                            _ => None,
                        })
                        .cloned()
                        .collect::<Vec<String>>()
                        .join("");
                    Some(serde_json::json!(text))
                };

                let tool_calls: Vec<OpenAIToolCall> = blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse { id, name, input } => Some(OpenAIToolCall {
                            id: id.clone(),
                            call_type: "function".to_string(),
                            function: OpenAIFunction {
                                name: name.clone(),
                                arguments: serde_json::to_string(input).unwrap_or_default(),
                            },
                        }),
                        _ => None,
                    })
                    .collect();

                (
                    content,
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                )
            }
        };

        OpenAIChatMessage {
            role,
            content,
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Convert messages to OpenAI format with special handling for tool results
    fn convert_messages(messages: &[Message]) -> Vec<OpenAIChatMessage> {
        let mut openai_messages = Vec::new();

        for message in messages {
            match &message.content {
                MessageContent::Structured(blocks) if message.role == MessageRole::User => {
                    // Check if message contains tool result blocks
                    let tool_results: Vec<&ContentBlock> = blocks
                        .iter()
                        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
                        .collect();

                    if !tool_results.is_empty() {
                        // For each tool result, create a separate "tool" message
                        for block in tool_results {
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error: _,
                            } = block
                            {
                                // Ensure content is never empty
                                let safe_content = if content.is_empty() {
                                    "No output".to_string()
                                } else {
                                    content.clone()
                                };

                                openai_messages.push(OpenAIChatMessage {
                                    role: "tool".to_string(),
                                    content: Some(serde_json::json!(safe_content)),
                                    tool_calls: None,
                                    tool_call_id: Some(tool_use_id.clone()),
                                });
                            }
                        }

                        // If there are other content blocks, handle them separately
                        let other_blocks: Vec<&ContentBlock> = blocks
                            .iter()
                            .filter(|block| !matches!(block, ContentBlock::ToolResult { .. }))
                            .collect();

                        if !other_blocks.is_empty() {
                            // Create a user message with the remaining blocks
                            // This creates a clone of the message with only non-tool-result blocks
                            let user_message = Message {
                                role: MessageRole::User,
                                content: MessageContent::Structured(
                                    other_blocks.iter().map(|&b| b.clone()).collect(),
                                ),
                                request_id: None,
                                usage: None,
                            };
                            openai_messages.push(Self::convert_message(&user_message));
                        }
                    } else {
                        // Normal conversion for user messages without tool results
                        openai_messages.push(Self::convert_message(message));
                    }
                }
                _ => {
                    // Normal conversion for all other message types
                    openai_messages.push(Self::convert_message(message));
                }
            }
        }

        openai_messages
    }

    async fn send_with_retry(
        &self,
        request: &OpenAIRequest,
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
                    if utils::handle_retryable_error::<OpenAIRateLimitInfo>(
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
        request: &OpenAIRequest,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        let mut request_json = serde_json::to_value(request.clone().into_non_streaming())?;

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut request_json)?;

        // Get auth headers
        let auth_headers = self.auth_provider.get_auth_headers().await?;

        // Build request
        let mut request_builder = self.client.post(self.get_url(false));

        // Add auth headers
        for (key, value) in auth_headers {
            request_builder = request_builder.header(key, value);
        }

        // Add additional headers
        for (key, value) in self.request_customizer.get_additional_headers() {
            request_builder = request_builder.header(key, value);
        }

        let response = request_builder
            .json(&request_json)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let response = utils::check_response_error::<OpenAIRateLimitInfo>(response).await?;
        let rate_limits = OpenAIRateLimitInfo::from_response(&response);

        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        // Parse the successful response
        let openai_response: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        Ok((
            LLMResponse {
                content: {
                    let mut blocks = Vec::new();

                    // Add text content if present
                    if let Some(content) = &openai_response.choices[0].message.content {
                        if let Some(text) = content.as_str() {
                            if !text.is_empty() {
                                blocks.push(ContentBlock::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }

                    // Add tool calls if present
                    if let Some(ref tool_calls) = openai_response.choices[0].message.tool_calls {
                        for call in tool_calls {
                            let input =
                                serde_json::from_str(&call.function.arguments).map_err(|e| {
                                    ApiError::Unknown(format!(
                                        "Failed to parse tool arguments: {}",
                                        e
                                    ))
                                })?;
                            blocks.push(ContentBlock::ToolUse {
                                id: call.id.clone(),
                                name: call.function.name.clone(),
                                input,
                            });
                        }
                    }

                    blocks
                },
                usage: Usage {
                    input_tokens: openai_response.usage.prompt_tokens,
                    output_tokens: openai_response.usage.completion_tokens,
                    // OpenAI doesn't support our caching markers, so these fields are 0
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                rate_limit_info: None,
            },
            rate_limits,
        ))
    }

    async fn try_send_request_streaming(
        &self,
        request: &OpenAIRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        debug!("Sending streaming request");
        let mut request_json = serde_json::to_value(request.clone().into_streaming())?;

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut request_json)?;

        // Get auth headers
        let auth_headers = self.auth_provider.get_auth_headers().await?;

        // Build request
        let mut request_builder = self.client.post(self.get_url(true));

        // Add auth headers
        for (key, value) in auth_headers {
            request_builder = request_builder.header(key, value);
        }

        // Add additional headers
        for (key, value) in self.request_customizer.get_additional_headers() {
            request_builder = request_builder.header(key, value);
        }

        let response = request_builder
            .json(&request_json)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let mut response = utils::check_response_error::<OpenAIRateLimitInfo>(response).await?;

        let mut accumulated_content: Option<String> = None;
        let mut accumulated_tool_calls: Vec<ContentBlock> = Vec::new();
        let mut current_tool: Option<OpenAIToolCallDelta> = None;

        let mut line_buffer = String::new();
        let mut usage = None;

        fn process_chunk(
            chunk: &[u8],
            line_buffer: &mut String,
            accumulated_content: &mut Option<String>,
            current_tool: &mut Option<OpenAIToolCallDelta>,
            accumulated_tool_calls: &mut Vec<ContentBlock>,
            callback: &StreamingCallback,
            usage: &mut Option<OpenAIUsage>,
        ) -> Result<()> {
            let chunk_str = std::str::from_utf8(chunk)?;

            for c in chunk_str.chars() {
                if c == '\n' {
                    if !line_buffer.is_empty() {
                        match process_sse_line(
                            line_buffer,
                            accumulated_content,
                            current_tool,
                            accumulated_tool_calls,
                            callback,
                            usage,
                        ) {
                            Ok(()) => {
                                line_buffer.clear();
                                continue;
                            }
                            Err(e) if e.to_string().contains("Tool limit reached") => {
                                debug!("Tool limit reached, stopping streaming early");

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
            Ok(())
        }

        fn process_sse_line(
            line: &str,
            accumulated_content: &mut Option<String>,
            current_tool: &mut Option<OpenAIToolCallDelta>,
            accumulated_tool_calls: &mut Vec<ContentBlock>,
            callback: &StreamingCallback,
            usage: &mut Option<OpenAIUsage>,
        ) -> Result<()> {
            if let Some(data) = line.strip_prefix("data: ") {
                // Skip "[DONE]" message
                if data == "[DONE]" {
                    return Ok(());
                }

                if let Ok(chunk_response) = serde_json::from_str::<OpenAIStreamResponse>(data) {
                    if let Some(delta) = chunk_response.choices.first() {
                        // Handle content streaming
                        if let Some(content) = &delta.delta.content {
                            *accumulated_content = Some(
                                accumulated_content
                                    .as_ref()
                                    .unwrap_or(&String::new())
                                    .clone()
                                    + content,
                            );
                            callback(&StreamingChunk::Text(content.clone()))?;
                        }

                        // Handle tool calls
                        if let Some(tool_calls) = &delta.delta.tool_calls {
                            for tool_call in tool_calls {
                                if let Some(function) = &tool_call.function {
                                    if tool_call.id.is_some() {
                                        // New tool call
                                        if let Some(prev_tool) = current_tool.take() {
                                            accumulated_tool_calls
                                                .push(OpenAIClient::build_tool_block(prev_tool)?);
                                        }
                                        *current_tool = Some(tool_call.clone());
                                    } else if let Some(curr_tool) = current_tool {
                                        // Update existing tool
                                        if let Some(args) = &function.arguments {
                                            if let Some(ref mut curr_func) = curr_tool.function {
                                                // Store previous arguments for diffing
                                                let prev_args = curr_func
                                                    .arguments
                                                    .as_ref()
                                                    .unwrap_or(&String::new())
                                                    .clone();

                                                // Update arguments
                                                curr_func.arguments =
                                                    Some(prev_args.clone() + args);

                                                // Stream the JSON input to the callback
                                                callback(&StreamingChunk::InputJson {
                                                    content: args.clone(),
                                                    tool_name: curr_tool
                                                        .function
                                                        .as_ref()
                                                        .and_then(|f| f.name.clone()),
                                                    tool_id: curr_tool.id.clone(),
                                                })?;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Handle completion
                        if delta.finish_reason.is_some() {
                            if let Some(tool) = current_tool.take() {
                                accumulated_tool_calls.push(OpenAIClient::build_tool_block(tool)?);
                            }
                        }
                    }
                    // Capture usage data from final chunk
                    if let Some(chunk_usage) = chunk_response.usage {
                        *usage = Some(chunk_usage);
                    }
                }
            }
            Ok(())
        }

        while let Some(chunk) = response.chunk().await? {
            process_chunk(
                &chunk,
                &mut line_buffer,
                &mut accumulated_content,
                &mut current_tool,
                &mut accumulated_tool_calls,
                streaming_callback,
                &mut usage,
            )?;
        }

        // Process any remaining data in the buffer
        if !line_buffer.is_empty() {
            process_sse_line(
                &line_buffer,
                &mut accumulated_content,
                &mut current_tool,
                &mut accumulated_tool_calls,
                streaming_callback,
                &mut usage,
            )?;
        }

        // Send StreamingComplete to indicate streaming has finished
        streaming_callback(&StreamingChunk::StreamingComplete)?;

        let mut content = Vec::new();
        if let Some(text) = accumulated_content {
            content.push(ContentBlock::Text { text });
        }
        content.extend(accumulated_tool_calls);

        Ok((
            LLMResponse {
                content,
                usage: usage
                    .map(|u| Usage {
                        input_tokens: u.prompt_tokens,
                        output_tokens: u.completion_tokens,
                        // OpenAI doesn't support our caching markers, so these fields are 0
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    })
                    .unwrap_or(Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                rate_limit_info: None,
            },
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
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OpenAIChatMessage> = Vec::new();

        // Add system message
        messages.push(OpenAIChatMessage {
            role: "system".to_string(),
            content: Some(serde_json::json!(request.system_prompt)),
            tool_calls: None,
            tool_call_id: None,
        });

        // Add conversation messages with special handling for tool results
        messages.extend(Self::convert_messages(&request.messages));

        let openai_request = OpenAIRequest {
            model: self.model.clone(),
            messages,
            temperature: self.get_temperature(),
            stream: None,
            stream_options: None,
            tool_choice: None,
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
