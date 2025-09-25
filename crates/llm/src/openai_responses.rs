//! OpenAI Responses API Provider
//!
//! This module implements an LLM provider for OpenAI's new Responses API, which is the more
//! modern format OpenAI will support going forward. Key features:
//!
//! - **Stateless Mode**: Uses `store: false` for compliance with Zero Data Retention (ZDR) requirements
//! - **Encrypted Reasoning**: Supports encrypted reasoning content that can be passed between requests
//! - **Function Calling**: Native support for the Responses API function calling format
//! - **Streaming**: Full streaming support with proper event handling
//! - **Rate Limiting**: Uses retry-after headers for proper rate limit handling
//!
//! ## Usage
//!
//! ```rust,no_run
//! use llm::{OpenAIResponsesClient, LLMProvider, LLMRequest, MessageRole, MessageContent};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let mut client = OpenAIResponsesClient::new(
//!         "your-api-key".to_string(),
//!         "gpt-5".to_string(),
//!         "https://api.openai.com/v1".to_string(),
//!     );
//!
//!     let request = LLMRequest {
//!         system_prompt: "You are a helpful assistant.".to_string(),
//!         messages: vec![],
//!         tools: None,
//!         stop_sequences: None,
//!         request_id: 1,
//!         session_id: "session-123".to_string(),
//!     };
//!
//!     let response = client.send_message(request, None).await?;
//!     println!("Response: {:?}", response);
//!     Ok(())
//! }
//! ```
//!
//! ## Reasoning Preservation
//!
//! The provider automatically handles encrypted reasoning content preservation across requests.
//! When the API returns encrypted reasoning (as `ContentBlock::RedactedThinking`), it will
//! be automatically included in subsequent requests to maintain reasoning context in stateless mode.

use crate::{
    recording::{APIRecorder, PlaybackState},
    streaming::{ChunkStream, HttpChunkStream, PlaybackChunkStream},
    types::*,
    utils, ApiError, LLMProvider, RateLimitHandler, StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

/// State tracking for reasoning during streaming
#[derive(Debug, Default)]
struct ReasoningState {
    current_item_id: Option<String>,
    current_item_content: String,
    completed_items: Vec<ReasoningSummaryItem>,
    reasoning_block_id: Option<String>, // Track the actual reasoning item ID from OpenAI
    received_completed_reasoning: bool, // Track if we got a completed reasoning item from API
}

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

/// Default request customizer for OpenAI Responses API
pub struct DefaultRequestCustomizer;

impl RequestCustomizer for DefaultRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![(
            "OpenAI-Beta".to_string(),
            "responses=experimental".to_string(),
        )]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/responses")
    }
}

/// OpenAI Responses API request structure
#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(default)]
    parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    store: bool,
    #[serde(default)]
    stream: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

/// Reasoning configuration for the request
#[derive(Debug, Serialize)]
struct ReasoningConfig {
    effort: String,
    summary: String,
}

/// Input item for the Responses API
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseInputItem {
    Message {
        role: String,
        content: Vec<ResponseContentItem>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    Reasoning {
        id: String,
        summary: Vec<serde_json::Value>,
        encrypted_content: String,
    },
}

/// Content item within messages
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseContentItem {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

/// Response structure from the Responses API
#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[allow(dead_code)]
    id: String,
    output: Vec<ResponseOutputItem>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

/// Output item from the response
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseOutputItem {
    Message {
        #[serde(default)]
        #[allow(dead_code)]
        id: Option<String>,
        #[allow(dead_code)]
        role: String,
        content: Vec<ResponseOutputContent>,
    },
    Reasoning {
        #[allow(dead_code)]
        id: String,
        #[serde(default)]
        summary: Vec<ReasoningSummary>,
        #[serde(default)]
        content: Vec<ReasoningContent>,
        #[serde(default)]
        encrypted_content: Option<String>,
    },
    FunctionCall {
        #[serde(default)]
        #[allow(dead_code)]
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
}

/// Content within output messages
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseOutputContent {
    OutputText { text: String },
}

/// Reasoning summary item
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReasoningSummary {
    SummaryText { text: String },
}

/// Reasoning content item
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReasoningContent {
    ReasoningText { text: String },
}

/// Usage information from the response
#[derive(Debug, Deserialize)]
struct ResponsesUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
    #[serde(default)]
    input_tokens_details: Option<InputTokensDetails>,
    #[serde(default)]
    #[allow(dead_code)]
    output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct InputTokensDetails {
    cached_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OutputTokensDetails {
    #[allow(dead_code)]
    reasoning_tokens: u32,
}

/// Streaming event from the Responses API
#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    response: Option<serde_json::Value>,
    #[serde(default)]
    item: Option<serde_json::Value>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    response_id: Option<String>,
    #[serde(default)]
    item_id: Option<String>,
}

/// Rate limit information from response headers
#[derive(Debug, Default)]
struct ResponsesRateLimitInfo {
    retry_after: Option<Duration>,
}

impl RateLimitHandler for ResponsesRateLimitInfo {
    fn from_response(response: &Response) -> Self {
        let headers = response.headers();

        let retry_after = headers
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        Self { retry_after }
    }

    fn get_retry_delay(&self) -> Duration {
        self.retry_after.unwrap_or(Duration::from_secs(60))
    }

    fn log_status(&self) {
        if let Some(retry_after) = self.retry_after {
            debug!("Rate limit - retry after: {}s", retry_after.as_secs());
        }
    }
}

pub struct OpenAIResponsesClient {
    client: Client,
    base_url: String,
    model: String,
    auth_provider: Box<dyn AuthProvider>,
    request_customizer: Box<dyn RequestCustomizer>,
    recorder: Option<APIRecorder>,
    playback: Option<PlaybackState>,
}

impl OpenAIResponsesClient {
    pub fn default_base_url() -> String {
        "https://api.openai.com/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
            recorder: None,
            playback: None,
        }
    }

    pub fn with_customization(
        model: String,
        base_url: String,
        auth_provider: Box<dyn AuthProvider>,
        request_customizer: Box<dyn RequestCustomizer>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
            auth_provider,
            request_customizer,
            recorder: None,
            playback: None,
        }
    }

    /// Add recording capability to the client
    pub fn with_recorder<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
        self.recorder = Some(APIRecorder::new(path));
        self
    }

    /// Add playback capability to the client
    pub fn with_playback(mut self, playback_state: PlaybackState) -> Self {
        self.playback = Some(playback_state);
        self
    }

    fn get_url(&self, streaming: bool) -> String {
        self.request_customizer
            .customize_url(&self.base_url, streaming)
    }

    /// Convert internal messages to Responses API format
    fn convert_messages(&self, messages: Vec<Message>) -> Vec<ResponseInputItem> {
        let mut result = Vec::new();

        for message in messages {
            match message.content {
                MessageContent::Text(text) => {
                    result.push(ResponseInputItem::Message {
                        role: match message.role {
                            MessageRole::User => "user".to_string(),
                            MessageRole::Assistant => "assistant".to_string(),
                        },
                        content: vec![ResponseContentItem::InputText { text }],
                    });
                }
                MessageContent::Structured(blocks) => {
                    self.convert_structured_message(message.role, blocks, &mut result);
                }
            }
        }

        result
    }

    #[inline]
    fn flush_current_message(
        &self,
        role_str: &str,
        current_message_content: &mut Vec<ResponseContentItem>,
        result: &mut Vec<ResponseInputItem>,
    ) {
        if !current_message_content.is_empty() {
            result.push(ResponseInputItem::Message {
                role: role_str.to_string(),
                content: std::mem::take(current_message_content),
            });
        }
    }

    fn convert_structured_message(
        &self,
        role: MessageRole,
        blocks: Vec<ContentBlock>,
        result: &mut Vec<ResponseInputItem>,
    ) {
        let mut current_message_content = Vec::new();
        let role_str = match role {
            MessageRole::User => "user".to_string(),
            MessageRole::Assistant => "assistant".to_string(),
        };

        for block in blocks {
            match block {
                ContentBlock::Text { text, .. } => match role {
                    MessageRole::User => {
                        current_message_content.push(ResponseContentItem::InputText { text });
                    }
                    MessageRole::Assistant => {
                        current_message_content.push(ResponseContentItem::OutputText { text });
                    }
                },
                ContentBlock::Image {
                    media_type, data, ..
                } => {
                    let image_url = format!("data:{media_type};base64,{data}");
                    current_message_content.push(ResponseContentItem::InputImage { image_url });
                }
                ContentBlock::Thinking { thinking, .. } => match role {
                    MessageRole::User => {
                        current_message_content
                            .push(ResponseContentItem::InputText { text: thinking });
                    }
                    MessageRole::Assistant => {
                        current_message_content
                            .push(ResponseContentItem::OutputText { text: thinking });
                    }
                },
                // Non-message content blocks: flush current message and add as separate items
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    // Flush current message content if any
                    self.flush_current_message(&role_str, &mut current_message_content, result);

                    result.push(ResponseInputItem::FunctionCallOutput {
                        call_id: tool_use_id,
                        output: content,
                    });
                }
                ContentBlock::RedactedThinking {
                    id, summary, data, ..
                } => {
                    // Flush current message content if any
                    self.flush_current_message(&role_str, &mut current_message_content, result);

                    // Convert structured summary items back to API format for input
                    let summary_json: Vec<serde_json::Value> = summary
                        .into_iter()
                        .map(|item| match item {
                            ReasoningSummaryItem::SummaryText { text } => {
                                serde_json::json!({"type": "summary_text", "text": text})
                            }
                        })
                        .collect();

                    result.push(ResponseInputItem::Reasoning {
                        id,
                        summary: summary_json,
                        encrypted_content: data,
                    });
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    // Flush current message content if any
                    self.flush_current_message(&role_str, &mut current_message_content, result);

                    // Convert tool use to function call input item
                    if role != MessageRole::Assistant {
                        warn!("ToolUse blocks should only appear in assistant messages");
                    }
                    result.push(ResponseInputItem::FunctionCall {
                        call_id: id,
                        name,
                        arguments: serde_json::to_string(&input)
                            .unwrap_or_else(|_| input.to_string()),
                    });
                }
            }
        }

        // Add any remaining message content
        self.flush_current_message(&role_str, &mut current_message_content, result);
    }

    /// Convert Responses API output to internal format
    fn convert_output(&self, output: Vec<ResponseOutputItem>) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();

        for item in output {
            match item {
                ResponseOutputItem::Message { content, .. } => {
                    for content_item in content {
                        match content_item {
                            ResponseOutputContent::OutputText { text } => {
                                blocks.push(ContentBlock::Text {
                                    text,
                                    start_time: None,
                                    end_time: None,
                                });
                            }
                        }
                    }
                }
                ResponseOutputItem::Reasoning {
                    id,
                    summary,
                    content,
                    encrypted_content,
                } => {
                    if let Some(encrypted) = encrypted_content {
                        // Convert summary items to structured format matching API
                        let summary_items: Vec<ReasoningSummaryItem> = summary
                            .into_iter()
                            .map(|s| match s {
                                ReasoningSummary::SummaryText { text } => {
                                    ReasoningSummaryItem::SummaryText { text }
                                }
                            })
                            .collect();

                        blocks.push(ContentBlock::RedactedThinking {
                            id,
                            summary: summary_items,
                            data: encrypted,
                            start_time: None,
                            end_time: None,
                        });
                    } else {
                        // Convert reasoning content to thinking blocks
                        for reasoning_item in content {
                            match reasoning_item {
                                ReasoningContent::ReasoningText { text } => {
                                    let signature = summary
                                        .first()
                                        .map(|s| match s {
                                            ReasoningSummary::SummaryText { text } => text.clone(),
                                        })
                                        .unwrap_or_default();

                                    blocks.push(ContentBlock::Thinking {
                                        thinking: text,
                                        signature,
                                        start_time: None,
                                        end_time: None,
                                    });
                                }
                            }
                        }
                    }
                }
                ResponseOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    let input = serde_json::from_str(&arguments)
                        .unwrap_or_else(|_| serde_json::Value::String(arguments));

                    blocks.push(ContentBlock::ToolUse {
                        id: call_id,
                        name,
                        input,
                        start_time: None,
                        end_time: None,
                    });
                }
            }
        }

        blocks
    }

    async fn send_with_retry(
        &mut self,
        request: &ResponsesRequest,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        // If playback is enabled, skip HTTP and use recorded data
        if self.playback.is_some() {
            return self.playback_request(request, streaming_callback).await;
        }

        let mut attempts = 0;

        loop {
            match self.try_send_request(request, streaming_callback).await {
                Ok((response, rate_limits)) => {
                    rate_limits.log_status();
                    return Ok(response);
                }
                Err(e) => {
                    if utils::handle_retryable_error::<ResponsesRateLimitInfo>(
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

    async fn playback_request(
        &mut self,
        _request: &ResponsesRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let playback = self.playback.as_ref().unwrap();

        // Get the next session from the recording
        let session = playback
            .next_session()
            .ok_or_else(|| anyhow::anyhow!("No more recorded sessions available"))?;

        debug!("Playing back session with {} chunks", session.chunks.len());

        if let Some(callback) = streaming_callback {
            // Create a playback chunk stream
            let mut chunk_stream = PlaybackChunkStream::new(session.chunks.clone(), playback.fast);
            let rate_limits = ResponsesRateLimitInfo::default();

            // Use the same streaming processing logic
            self.process_chunk_stream(&mut chunk_stream, callback, rate_limits)
                .await
                .map(|(response, _)| response)
        } else {
            // Non-streaming playback - parse the recorded response body from chunks
            let body: String = session.chunks.iter().map(|c| c.data.as_str()).collect();

            let responses_response: ResponsesResponse = serde_json::from_str(&body)
                .map_err(|e| anyhow::anyhow!("Failed to parse recorded response body: {e}"))?;

            let content = self.convert_output(responses_response.output);
            let usage = responses_response
                .usage
                .map_or_else(Usage::zero, |u| Usage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: u
                        .input_tokens_details
                        .map(|d| d.cached_tokens)
                        .unwrap_or(0),
                });

            Ok(LLMResponse {
                content,
                usage,
                rate_limit_info: None,
            })
        }
    }

    async fn try_send_request(
        &mut self,
        request: &ResponsesRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, ResponsesRateLimitInfo)> {
        let mut request_json = serde_json::to_value(request)?;

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut request_json)?;

        // Start recording if recorder is available
        if let Some(recorder) = &self.recorder {
            recorder.start_recording(request_json.clone())?;
        }

        // Get auth headers
        let auth_headers = self.auth_provider.get_auth_headers().await?;

        // Build request
        let mut request_builder = self.client.post(self.get_url(streaming_callback.is_some()));

        // Add auth headers
        for (key, value) in auth_headers {
            request_builder = request_builder.header(key, value);
        }
        request_builder = request_builder.header("Content-Type", "application/json");

        // Add additional headers
        for (key, value) in self.request_customizer.get_additional_headers() {
            request_builder = request_builder.header(key, value);
        }

        if streaming_callback.is_some() {
            request_builder = request_builder.header("Accept", "text/event-stream");
        }

        debug!("Sending request: {request_json}");

        let response = request_builder
            .json(&request_json)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let response = utils::check_response_error::<ResponsesRateLimitInfo>(response).await?;
        let rate_limits = ResponsesRateLimitInfo::from_response(&response);

        let result = if let Some(callback) = streaming_callback {
            self.handle_streaming_response(response, callback, rate_limits)
                .await
        } else {
            self.handle_non_streaming_response(response, rate_limits)
                .await
        };

        // End recording on completion (success or failure)
        if let Some(recorder) = &self.recorder {
            if let Err(e) = recorder.end_recording() {
                warn!("Failed to end recording: {e}");
            }
        }

        result
    }

    async fn handle_non_streaming_response(
        &self,
        response: Response,
        rate_limits: ResponsesRateLimitInfo,
    ) -> Result<(LLMResponse, ResponsesRateLimitInfo)> {
        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        // Record the entire response for non-streaming if recorder is available
        if let Some(recorder) = &self.recorder {
            if let Err(e) = recorder.record_chunk(&response_text) {
                warn!("Failed to record non-streaming response: {e}");
            }
        }

        let responses_response: ResponsesResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {e}")))?;

        let content = self.convert_output(responses_response.output);
        let usage = responses_response
            .usage
            .map_or_else(Usage::zero, |u| Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: u
                    .input_tokens_details
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0),
            });

        Ok((
            LLMResponse {
                content,
                usage,
                rate_limit_info: None,
            },
            rate_limits,
        ))
    }

    async fn handle_streaming_response(
        &self,
        response: Response,
        callback: &StreamingCallback,
        rate_limits: ResponsesRateLimitInfo,
    ) -> Result<(LLMResponse, ResponsesRateLimitInfo)> {
        let mut chunk_stream = HttpChunkStream::new(response);
        self.process_chunk_stream(&mut chunk_stream, callback, rate_limits)
            .await
    }

    #[inline]
    fn process_decoded_segment(
        &self,
        segment: &str,
        line_buffer: &mut String,
        processor: &mut StreamProcessor,
    ) -> Result<()> {
        for c in segment.chars() {
            if c == '\n' {
                if !line_buffer.is_empty() {
                    // Record the SSE line if recorder is available
                    if let Some(recorder) = &self.recorder {
                        if let Err(e) = recorder.record_chunk(line_buffer) {
                            warn!("Failed to record chunk: {e}");
                        }
                    }
                    processor.process_line(line_buffer)?;
                }
                line_buffer.clear();
            } else {
                line_buffer.push(c);
            }
        }
        Ok(())
    }

    async fn process_chunk_stream(
        &self,
        chunk_stream: &mut dyn ChunkStream,
        callback: &StreamingCallback,
        rate_limits: ResponsesRateLimitInfo,
    ) -> Result<(LLMResponse, ResponsesRateLimitInfo)> {
        let mut content_blocks = Vec::new();
        let mut line_buffer = String::new();
        let mut usage = Usage::zero();
        let mut byte_buffer = Vec::new();

        let mut processor = StreamProcessor::new(self, &mut content_blocks, &mut usage, callback);

        while let Some(chunk) = chunk_stream.next_chunk().await? {
            byte_buffer.extend_from_slice(&chunk);

            // Try to decode as much as possible
            match std::str::from_utf8(&byte_buffer) {
                Ok(chunk_str) => {
                    // Successfully decoded all bytes
                    self.process_decoded_segment(chunk_str, &mut line_buffer, &mut processor)?;
                    byte_buffer.clear();
                }
                Err(e) => {
                    // Check if this is just an incomplete sequence at the end
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to > 0 {
                        // Process the valid part
                        let valid_str = std::str::from_utf8(&byte_buffer[..valid_up_to])?;
                        self.process_decoded_segment(valid_str, &mut line_buffer, &mut processor)?;
                        // Keep the incomplete bytes for the next chunk
                        byte_buffer.drain(..valid_up_to);
                    } else {
                        // This shouldn't happen with properly encoded UTF-8
                        debug!(
                            "UTF-8 decode error: {e}, buffer length: {}",
                            byte_buffer.len()
                        );
                        return Err(anyhow::anyhow!("UTF-8 decode error: {e}"));
                    }
                }
            }
        }

        // Process any remaining data
        if !line_buffer.is_empty() {
            // Record the final SSE line if recorder is available
            if let Some(recorder) = &self.recorder {
                if let Err(e) = recorder.record_chunk(&line_buffer) {
                    warn!("Failed to record final chunk: {e}");
                }
            }
            processor.process_line(&line_buffer)?;
        }

        // If there's leftover bytes in buffer (unterminated line without trailing \n), try to process
        if !byte_buffer.is_empty() {
            if let Ok(valid_str) = std::str::from_utf8(&byte_buffer) {
                self.process_decoded_segment(valid_str, &mut line_buffer, &mut processor)?;
                byte_buffer.clear();
            }
        }

        // Finalize internal state (reasoning, usage from response, etc.) if needed
        processor.on_response_completed(None)?;

        callback(&StreamingChunk::StreamingComplete)?;

        Ok((
            LLMResponse {
                content: content_blocks,
                usage,
                rate_limit_info: None,
            },
            rate_limits,
        ))
    }
}

#[derive(Debug, Clone)]
struct FunctionCallInfo {
    name: String,
    call_id: String,
}

use std::collections::HashMap;
use std::time::SystemTime;

type FunctionCalls = HashMap<String, FunctionCallInfo>; // item_id -> info

type BlockStartTimes = HashMap<String, SystemTime>; // item_id -> start_time

struct StreamProcessor<'a> {
    client: &'a OpenAIResponsesClient,
    content_blocks: &'a mut Vec<ContentBlock>,
    usage: &'a mut Usage,
    active_function_calls: FunctionCalls,
    block_start_times: BlockStartTimes,
    reasoning_state: ReasoningState,
    callback: &'a StreamingCallback,
}

impl<'a> StreamProcessor<'a> {
    fn new(
        client: &'a OpenAIResponsesClient,
        content_blocks: &'a mut Vec<ContentBlock>,
        usage: &'a mut Usage,
        callback: &'a StreamingCallback,
    ) -> Self {
        Self {
            client,
            content_blocks,
            usage,
            active_function_calls: HashMap::new(),
            block_start_times: HashMap::new(),
            reasoning_state: ReasoningState::default(),
            callback,
        }
    }

    fn process_line(&mut self, line: &str) -> Result<()> {
        if let Some(data) = line.strip_prefix("data: ") {
            debug!("received event: {data}");

            let event: StreamEvent = serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Failed to parse SSE event: {e}"))?;

            match event.event_type.as_str() {
                "response.output_item.added" => {
                    if let Some(item) = event.item {
                        self.on_output_item_added(item)?;
                    }
                }
                "response.output_text.delta" => {
                    if let Some(delta) = event.delta {
                        self.on_output_text_delta(delta)?;
                    }
                }
                "response.reasoning_text.delta" => {
                    if let Some(delta) = event.delta {
                        self.on_reasoning_text_delta(delta)?;
                    }
                }
                "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = event.delta {
                        let id = event.item_id.unwrap_or_else(|| "default".to_string());
                        self.on_reasoning_summary_delta(id, delta)?;
                    }
                }
                "response.function_call_arguments.delta" => {
                    if let Some(delta) = event.delta {
                        self.on_function_call_arguments_delta(event.item_id.as_deref(), delta)?;
                    }
                }
                "response.output_item.done" => {
                    if let Some(item) = event.item {
                        self.on_output_item_done(item)?;
                    }
                }
                "response.completed" => {
                    self.on_response_completed(event.response.as_ref())?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn on_output_item_added(&mut self, item_data: serde_json::Value) -> Result<()> {
        let now = SystemTime::now();
        if let Ok(item_type) = serde_json::from_value::<serde_json::Value>(item_data.clone()) {
            if let Some(item_type_str) = item_type.get("type").and_then(|v| v.as_str()) {
                let item_id = item_type
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Record start time for this item
                self.block_start_times.insert(item_id.clone(), now);

                if item_type_str == "function_call" {
                    let call_id = item_type
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item_type
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    self.active_function_calls
                        .insert(item_id, FunctionCallInfo { name, call_id });
                } else if item_type_str == "reasoning" {
                    // Track the reasoning item ID for proper round-trip preservation
                    self.reasoning_state.reasoning_block_id = Some(item_id);
                }
            }
        }
        Ok(())
    }

    fn on_output_text_delta(&mut self, delta: String) -> Result<()> {
        (self.callback)(&StreamingChunk::Text(delta))
    }

    fn on_reasoning_text_delta(&mut self, delta: String) -> Result<()> {
        (self.callback)(&StreamingChunk::Thinking(delta))
    }

    fn on_reasoning_summary_delta(&mut self, item_id: String, delta: String) -> Result<()> {
        // Check if this is a new item
        if self.reasoning_state.current_item_id.as_ref() != Some(&item_id) {
            // If we had a previous item, finalize it
            if let Some(_prev_id) = &self.reasoning_state.current_item_id {
                self.reasoning_state
                    .completed_items
                    .push(ReasoningSummaryItem::SummaryText {
                        text: self.reasoning_state.current_item_content.clone(),
                    });
            }

            // Start tracking the new item and notify downstream consumers
            self.reasoning_state.current_item_id = Some(item_id.clone());
            self.reasoning_state.current_item_content.clear();
            (self.callback)(&StreamingChunk::ReasoningSummaryStart)?;
        }

        // Append the delta to the current item content
        self.reasoning_state.current_item_content.push_str(&delta);

        // Emit the reasoning summary chunk with raw content delta
        (self.callback)(&StreamingChunk::ReasoningSummaryDelta(delta))
    }

    fn on_function_call_arguments_delta(
        &mut self,
        item_id: Option<&str>,
        delta: String,
    ) -> Result<()> {
        let (tool_name, tool_id) = if let Some(id) = item_id {
            self.active_function_calls
                .get(id)
                .map(|info| (Some(info.name.clone()), Some(info.call_id.clone())))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };

        (self.callback)(&StreamingChunk::InputJson {
            content: delta,
            tool_name,
            tool_id,
        })
    }

    fn on_output_item_done(&mut self, item_data: serde_json::Value) -> Result<()> {
        let now = SystemTime::now();
        let output_item: ResponseOutputItem = serde_json::from_value(item_data.clone())?;

        // Get the item ID for timestamp lookup
        let item_id = item_data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let start_time = self.block_start_times.get(&item_id).copied();

        let mut converted = self.client.convert_output(vec![output_item]);

        // Set timestamps on the converted blocks
        for block in &mut converted {
            if let Some(start) = start_time {
                block.set_timestamps(start, now);
            }
        }

        // Check if this is a reasoning item to avoid duplicate creation
        for block in &converted {
            if matches!(block, ContentBlock::RedactedThinking { .. }) {
                self.reasoning_state.received_completed_reasoning = true;
            }
        }

        self.content_blocks.extend(converted);
        Ok(())
    }

    fn on_response_completed(&mut self, response: Option<&serde_json::Value>) -> Result<()> {
        // Finalize any remaining reasoning item
        if self.reasoning_state.current_item_id.is_some() {
            self.reasoning_state
                .completed_items
                .push(ReasoningSummaryItem::SummaryText {
                    text: self.reasoning_state.current_item_content.clone(),
                });

            self.reasoning_state.current_item_id = None;
            self.reasoning_state.current_item_content.clear();
        }

        // Emit reasoning complete if we had any reasoning items
        if !self.reasoning_state.completed_items.is_empty() {
            (self.callback)(&StreamingChunk::ReasoningComplete)?;
        }

        if let Some(response_data) = response {
            if let Ok(usage_data) = serde_json::from_value::<ResponsesUsage>(
                response_data
                    .get("usage")
                    .unwrap_or(&serde_json::Value::Null)
                    .clone(),
            ) {
                self.usage.input_tokens = usage_data.input_tokens;
                self.usage.output_tokens = usage_data.output_tokens;
                self.usage.cache_read_input_tokens = usage_data
                    .input_tokens_details
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0);
            }
        }

        // Create a RedactedThinking block from collected reasoning items if any
        // Only do this if we didn't receive a completed reasoning item from the API
        if !self.reasoning_state.completed_items.is_empty()
            && !self.reasoning_state.received_completed_reasoning
        {
            self.content_blocks.push(ContentBlock::RedactedThinking {
                id: self
                    .reasoning_state
                    .reasoning_block_id
                    .clone()
                    .unwrap_or_else(|| "reasoning_stream".to_string()),
                summary: self.reasoning_state.completed_items.clone(),
                data: "".to_string(), // No encrypted data for streaming
                start_time: None,
                end_time: None,
            });
        }

        Ok(())
    }
}

#[async_trait]
impl LLMProvider for OpenAIResponsesClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let input = self.convert_messages(request.messages);
        let instructions = if request.system_prompt.is_empty() {
            None
        } else {
            Some(request.system_prompt)
        };

        let tools = request.tools.map(|tools| {
            tools
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters
                    })
                })
                .collect()
        });

        // Configure for stateless mode with encrypted reasoning
        let store = false;
        // Always request encrypted reasoning content when operating statelessly
        let include = if !store {
            vec!["reasoning.encrypted_content".to_string()]
        } else {
            vec![]
        };

        let responses_request = ResponsesRequest {
            model: self.model.clone(),
            instructions,
            input,
            tools,
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: false,
            reasoning: Some(ReasoningConfig {
                effort: "low".to_string(),
                summary: "detailed".to_string(),
            }),
            store,
            stream: streaming_callback.is_some(),
            include,
            prompt_cache_key: Some(request.session_id),
        };

        let request_start = std::time::SystemTime::now();
        let mut response = self
            .send_with_retry(&responses_request, streaming_callback, 3)
            .await?;
        let response_end = std::time::SystemTime::now();

        // For non-streaming responses, distribute timestamps across blocks
        if streaming_callback.is_none() {
            response.set_distributed_timestamps(request_start, response_end);
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_simple_text_message() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text("Hello".to_string()),
            request_id: None,
            usage: None,
        }];

        let converted = client.convert_messages(messages);
        assert_eq!(converted.len(), 1);

        match &converted[0] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::InputText { text } => {
                        assert_eq!(text, "Hello");
                    }
                    _ => panic!("Expected InputText"),
                }
            }
            _ => panic!("Expected Message"),
        }
    }

    #[test]
    fn test_convert_tool_result_message() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                tool_use_id: "test_id".to_string(),
                content: "Tool output".to_string(),
                is_error: Some(false),
                start_time: None,
                end_time: None,
            }]),
            request_id: None,
            usage: None,
        }];

        let converted = client.convert_messages(messages);
        assert_eq!(converted.len(), 1);

        match &converted[0] {
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "test_id");
                assert_eq!(output, "Tool output");
            }
            _ => panic!("Expected FunctionCallOutput"),
        }
    }

    #[test]
    fn test_convert_output_with_reasoning() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let output = vec![
            ResponseOutputItem::Reasoning {
                id: "reasoning_1".to_string(),
                summary: vec![ReasoningSummary::SummaryText {
                    text: "Summary".to_string(),
                }],
                content: vec![ReasoningContent::ReasoningText {
                    text: "Thinking...".to_string(),
                }],
                encrypted_content: None,
            },
            ResponseOutputItem::Message {
                id: Some("msg_1".to_string()),
                role: "assistant".to_string(),
                content: vec![ResponseOutputContent::OutputText {
                    text: "Hello!".to_string(),
                }],
            },
        ];

        let converted = client.convert_output(output);
        assert_eq!(converted.len(), 2);

        match &converted[0] {
            ContentBlock::Thinking {
                thinking,
                signature,
                ..
            } => {
                assert_eq!(thinking, "Thinking...");
                assert_eq!(signature, "Summary");
            }
            _ => panic!("Expected Thinking block"),
        }

        match &converted[1] {
            ContentBlock::Text { text, .. } => {
                assert_eq!(text, "Hello!");
            }
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_convert_encrypted_reasoning() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let output = vec![ResponseOutputItem::Reasoning {
            id: "rs_12345".to_string(),
            summary: vec![ReasoningSummary::SummaryText {
                text: "Test summary".to_string(),
            }],
            content: vec![],
            encrypted_content: Some("encrypted_data".to_string()),
        }];

        let converted = client.convert_output(output);
        assert_eq!(converted.len(), 1);

        match &converted[0] {
            ContentBlock::RedactedThinking {
                id, summary, data, ..
            } => {
                assert_eq!(id, "rs_12345");
                assert_eq!(summary.len(), 1);
                match &summary[0] {
                    ReasoningSummaryItem::SummaryText { text } => {
                        assert_eq!(text, "Test summary");
                    }
                }
                assert_eq!(data, "encrypted_data");
            }
            _ => panic!("Expected RedactedThinking block"),
        }
    }

    #[test]
    fn test_reasoning_round_trip_preservation() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        // Simulate a conversation where:
        // 1. User asks a question
        // 2. Assistant responds with encrypted reasoning + text
        // 3. User asks a follow-up
        // 4. The encrypted reasoning should be preserved in the next request

        let conversation = vec![
            Message {
                role: MessageRole::User,
                content: MessageContent::Text("What's 2+2?".to_string()),
                request_id: None,
                usage: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: MessageContent::Structured(vec![
                    ContentBlock::RedactedThinking {
                        id: "rs_12345".to_string(),
                        summary: vec![ReasoningSummaryItem::SummaryText {
                            text: "Math reasoning".to_string(),
                        }],
                        data: "encrypted_math_reasoning".to_string(),
                        start_time: None,
                        end_time: None,
                    },
                    ContentBlock::Text {
                        text: "2+2 equals 4.".to_string(),
                        start_time: None,
                        end_time: None,
                    },
                ]),
                request_id: None,
                usage: None,
            },
            Message {
                role: MessageRole::User,
                content: MessageContent::Text("What about 3+3?".to_string()),
                request_id: None,
                usage: None,
            },
        ];

        let converted = client.convert_messages(conversation);
        assert_eq!(converted.len(), 4);

        // First: User question
        match &converted[0] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content.len(), 1);
            }
            _ => panic!("Expected user message"),
        }

        // Second: Encrypted reasoning preserved (maintains original order)
        match &converted[1] {
            ResponseInputItem::Reasoning {
                id,
                summary,
                encrypted_content,
            } => {
                assert_eq!(id, "rs_12345");
                assert_eq!(summary.len(), 1);
                // Verify the JSON format is correct
                assert_eq!(
                    summary[0],
                    serde_json::json!({"type": "summary_text", "text": "Math reasoning"})
                );
                assert_eq!(encrypted_content, "encrypted_math_reasoning");
            }
            _ => panic!("Expected Reasoning item"),
        }

        // Third: Assistant response text
        match &converted[2] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::OutputText { text } => {
                        assert_eq!(text, "2+2 equals 4.");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected assistant message"),
        }

        // Fourth: Follow-up user question
        match &converted[3] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::InputText { text } => {
                        assert_eq!(text, "What about 3+3?");
                    }
                    _ => panic!("Expected InputText"),
                }
            }
            _ => panic!("Expected user message"),
        }
    }

    #[test]
    fn test_order_preservation_mixed_content() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(vec![
                ContentBlock::Text {
                    text: "First text".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_123".to_string(),
                    name: "test_tool".to_string(),
                    input: serde_json::json!({"arg": "value"}),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::Text {
                    text: "Second text".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::RedactedThinking {
                    id: "rs_456".to_string(),
                    summary: vec![ReasoningSummaryItem::SummaryText {
                        text: "Some reasoning".to_string(),
                    }],
                    data: "encrypted_data".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::Text {
                    text: "Third text".to_string(),
                    start_time: None,
                    end_time: None,
                },
            ]),
            request_id: None,
            usage: None,
        }];

        let converted = client.convert_messages(messages);
        assert_eq!(converted.len(), 5);

        // First: First text
        match &converted[0] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::OutputText { text } => {
                        assert_eq!(text, "First text");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }

        // Second: Tool use
        match &converted[1] {
            ResponseInputItem::FunctionCall { call_id, name, .. } => {
                assert_eq!(call_id, "tool_123");
                assert_eq!(name, "test_tool");
            }
            _ => panic!("Expected FunctionCall"),
        }

        // Third: Second text
        match &converted[2] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::OutputText { text } => {
                        assert_eq!(text, "Second text");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }

        // Fourth: Reasoning
        match &converted[3] {
            ResponseInputItem::Reasoning { id, .. } => {
                assert_eq!(id, "rs_456");
            }
            _ => panic!("Expected Reasoning"),
        }

        // Fifth: Third text
        match &converted[4] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::OutputText { text } => {
                        assert_eq!(text, "Third text");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }
    }

    #[test]
    fn test_convert_redacted_thinking_to_input() {
        let client = OpenAIResponsesClient::new(
            "test_key".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(vec![
                ContentBlock::RedactedThinking {
                    id: "rs_67890".to_string(),
                    summary: vec![ReasoningSummaryItem::SummaryText {
                        text: "Previous reasoning".to_string(),
                    }],
                    data: "encrypted_reasoning_data".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::Text {
                    text: "Based on my reasoning, here's the answer.".to_string(),
                    start_time: None,
                    end_time: None,
                },
            ]),
            request_id: None,
            usage: None,
        }];

        let converted = client.convert_messages(messages);
        assert_eq!(converted.len(), 2);

        // First should be the encrypted reasoning (maintains original order)
        match &converted[0] {
            ResponseInputItem::Reasoning {
                id,
                summary,
                encrypted_content,
            } => {
                assert_eq!(id, "rs_67890");
                assert_eq!(summary.len(), 1);
                assert_eq!(
                    summary[0],
                    serde_json::json!({"type": "summary_text", "text": "Previous reasoning"})
                );
                assert_eq!(encrypted_content, "encrypted_reasoning_data");
            }
            _ => panic!("Expected Reasoning item"),
        }

        // Second should be the message content
        match &converted[1] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ResponseContentItem::OutputText { text } => {
                        assert_eq!(text, "Based on my reasoning, here's the answer.");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }
    }
}
