use crate::{
    recording::APIRecorder, types::*, utils, ApiError, LLMProvider, RateLimitHandler,
    StreamingCallback, StreamingChunk,
};
use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, SystemTime};
use tracing::{debug, trace, warn};

// ============================================================================
// Customization Traits
// ============================================================================

/// Trait for providing authentication for Vertex API requests
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Get authentication to apply to the request.
    /// Returns either query parameters or headers (or both).
    async fn get_auth(&self) -> Result<VertexAuth>;
}

/// Authentication configuration for Vertex API
pub struct VertexAuth {
    /// Query parameters to add to the URL (e.g., `key=...`)
    pub query_params: Vec<(String, String)>,
    /// Headers to add to the request (e.g., `Authorization: Bearer ...`)
    pub headers: Vec<(String, String)>,
}

/// Trait for customizing Vertex API requests
pub trait RequestCustomizer: Send + Sync {
    /// Customize the request JSON before sending
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()>;
    /// Get additional headers to include in requests
    fn get_additional_headers(&self) -> Vec<(String, String)>;
    /// Customize the URL for a request
    fn customize_url(&self, base_url: &str, model: &str, streaming: bool) -> String;
}

// ============================================================================
// Default Implementations
// ============================================================================

/// Default API key authentication provider (uses query parameter)
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
    async fn get_auth(&self) -> Result<VertexAuth> {
        Ok(VertexAuth {
            query_params: vec![("key".to_string(), self.api_key.clone())],
            headers: vec![],
        })
    }
}

/// Default request customizer for Google Generative Language API
pub struct DefaultRequestCustomizer;

impl RequestCustomizer for DefaultRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![("Content-Type".to_string(), "application/json".to_string())]
    }

    fn customize_url(&self, base_url: &str, model: &str, streaming: bool) -> String {
        if streaming {
            format!("{}/models/{}:streamGenerateContent", base_url, model)
        } else {
            format!("{}/models/{}:generateContent", base_url, model)
        }
    }
}

// ============================================================================
// Request/Response Types
// ============================================================================

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

#[derive(Debug, Deserialize, Default)]
struct VertexUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
    #[allow(dead_code)]
    #[serde(rename = "totalTokenCount", default)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    partial_args: Vec<VertexPartialArg>,
    #[serde(skip_serializing_if = "Option::is_none")]
    will_continue: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexPartialArg {
    json_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    will_continue: Option<bool>,
    #[serde(flatten)]
    value: serde_json::Map<String, serde_json::Value>,
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

#[derive(Debug)]
struct VertexStreamingState {
    content_blocks: Vec<ContentBlock>,
    active_tool_calls: Vec<ActiveVertexToolCall>,
    tool_counter: u32,
    last_usage: Option<VertexUsageMetadata>,
}

impl VertexStreamingState {
    fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            active_tool_calls: Vec::new(),
            tool_counter: 0,
            last_usage: None,
        }
    }
}

#[derive(Debug)]
struct ActiveVertexToolCall {
    block_index: usize,
    id: String,
    name: String,
    completed: bool,
    final_input_emitted: bool,
}

fn finish_last_block(blocks: &mut [ContentBlock]) {
    let now = SystemTime::now();
    if let Some(
        ContentBlock::Text { end_time, .. }
        | ContentBlock::Thinking { end_time, .. }
        | ContentBlock::ToolUse { end_time, .. },
    ) = blocks.last_mut()
    {
        if end_time.is_none() {
            *end_time = Some(now);
        }
    }
}

fn enable_streaming_function_call_arguments(request_json: &mut serde_json::Value) {
    if request_json
        .get("tools")
        .is_none_or(|tools| tools.is_null())
    {
        return;
    }

    request_json["tool_config"]["function_calling_config"] = json!({
        "mode": "AUTO",
        "stream_function_call_arguments": true,
    });
}

fn emit_vertex_input_json_snapshot(
    callback: &StreamingCallback,
    tool: &ActiveVertexToolCall,
    input: &serde_json::Value,
) -> Result<()> {
    callback(&StreamingChunk::InputJson {
        content: serde_json::to_string(input)?,
        tool_name: Some(tool.name.clone()),
        tool_id: Some(tool.id.clone()),
    })
}

fn emit_vertex_tool_start(callback: &StreamingCallback, tool: &ActiveVertexToolCall) -> Result<()> {
    callback(&StreamingChunk::InputJson {
        content: String::new(),
        tool_name: Some(tool.name.clone()),
        tool_id: Some(tool.id.clone()),
    })
}

fn vertex_tool_input_mut<'a>(
    blocks: &'a mut [ContentBlock],
    tool: &ActiveVertexToolCall,
) -> Result<&'a mut serde_json::Value> {
    match blocks.get_mut(tool.block_index) {
        Some(ContentBlock::ToolUse { input, .. }) => Ok(input),
        _ => bail!("Vertex streaming tool state points to a non-tool block"),
    }
}

fn vertex_tool_input<'a>(
    blocks: &'a [ContentBlock],
    tool: &ActiveVertexToolCall,
) -> Result<&'a serde_json::Value> {
    match blocks.get(tool.block_index) {
        Some(ContentBlock::ToolUse { input, .. }) => Ok(input),
        _ => bail!("Vertex streaming tool state points to a non-tool block"),
    }
}

fn start_vertex_tool_call(
    state: &mut VertexStreamingState,
    request_id: u64,
    name: String,
    thought_signature: Option<String>,
    callback: &StreamingCallback,
) -> Result<usize> {
    finish_last_block(&mut state.content_blocks);

    state.tool_counter += 1;
    let tool_id = format!("tool-{}-{}", request_id, state.tool_counter);
    let block_index = state.content_blocks.len();

    state.content_blocks.push(ContentBlock::ToolUse {
        id: tool_id.clone(),
        name: name.clone(),
        input: json!({}),
        thought_signature,
        start_time: Some(SystemTime::now()),
        end_time: None,
    });

    state.active_tool_calls.push(ActiveVertexToolCall {
        block_index,
        id: tool_id,
        name,
        completed: false,
        final_input_emitted: false,
    });

    let tool = state
        .active_tool_calls
        .last()
        .expect("newly pushed Vertex tool call must exist");
    emit_vertex_tool_start(callback, tool)?;

    Ok(state.active_tool_calls.len() - 1)
}

fn active_vertex_tool_call_index(state: &VertexStreamingState) -> Option<usize> {
    state
        .active_tool_calls
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, tool)| (!tool.completed).then_some(index))
}

fn active_vertex_tool_call_index_by_name(
    state: &VertexStreamingState,
    name: &str,
) -> Option<usize> {
    state
        .active_tool_calls
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, tool)| (!tool.completed && tool.name == name).then_some(index))
}

fn handle_vertex_function_call(
    function_call: &VertexFunctionCall,
    thought_signature: Option<String>,
    state: &mut VertexStreamingState,
    request_id: u64,
    callback: &StreamingCallback,
) -> Result<()> {
    let has_name = function_call
        .name
        .as_deref()
        .is_some_and(|name| !name.is_empty());

    let tool_index = if let Some(name) = function_call
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        if let Some(index) = active_vertex_tool_call_index_by_name(state, name) {
            index
        } else {
            start_vertex_tool_call(
                state,
                request_id,
                name.to_string(),
                thought_signature,
                callback,
            )?
        }
    } else {
        match active_vertex_tool_call_index(state) {
            Some(index) => index,
            None => {
                warn!("Vertex functionCall continuation arrived without an active tool call");
                return Ok(());
            }
        }
    };

    if let Some(args) = &function_call.args {
        let tool = &state.active_tool_calls[tool_index];
        *vertex_tool_input_mut(&mut state.content_blocks, tool)? = args.clone();
    }

    for partial_arg in &function_call.partial_args {
        let tool = &state.active_tool_calls[tool_index];
        let input = vertex_tool_input_mut(&mut state.content_blocks, tool)?;
        apply_vertex_partial_arg(input, partial_arg)?;
    }

    let should_complete = function_call.args.is_some()
        || function_call.will_continue == Some(false)
        || (!has_name
            && function_call.args.is_none()
            && function_call.partial_args.is_empty()
            && function_call.will_continue.is_none());

    if should_complete {
        let tool = &state.active_tool_calls[tool_index];
        let input = vertex_tool_input(&state.content_blocks, tool)?;
        if !tool.final_input_emitted {
            emit_vertex_input_json_snapshot(callback, tool, input)?;
        }

        if let Some(ContentBlock::ToolUse { end_time, .. }) =
            state.content_blocks.get_mut(tool.block_index)
        {
            *end_time = Some(SystemTime::now());
        }

        if let Some(tool) = state.active_tool_calls.get_mut(tool_index) {
            tool.completed = true;
            tool.final_input_emitted = true;
        }
    }

    Ok(())
}

fn finish_open_vertex_tool_calls(
    state: &mut VertexStreamingState,
    callback: &StreamingCallback,
) -> Result<()> {
    for tool in &mut state.active_tool_calls {
        if tool.completed {
            continue;
        }

        let input = vertex_tool_input(&state.content_blocks, tool)?;
        if !tool.final_input_emitted {
            emit_vertex_input_json_snapshot(callback, tool, input)?;
            tool.final_input_emitted = true;
        }

        if let Some(ContentBlock::ToolUse { end_time, .. }) =
            state.content_blocks.get_mut(tool.block_index)
        {
            *end_time = Some(SystemTime::now());
        }

        tool.completed = true;
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VertexJsonPathToken {
    ObjectKey(String),
    ArrayIndex(usize),
}

fn parse_vertex_json_path(path: &str) -> Result<Vec<VertexJsonPathToken>> {
    let mut chars = path.chars().peekable();
    if chars.next() != Some('$') {
        bail!("Vertex partialArgs jsonPath must start with '$': {path}");
    }

    let mut tokens = Vec::new();
    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                let mut key = String::new();
                while let Some(next) = chars.peek().copied() {
                    if next == '.' || next == '[' {
                        break;
                    }
                    key.push(next);
                    chars.next();
                }
                if key.is_empty() {
                    bail!("Vertex partialArgs jsonPath has an empty object key: {path}");
                }
                tokens.push(VertexJsonPathToken::ObjectKey(key));
            }
            '[' => {
                let mut index = String::new();
                for next in chars.by_ref() {
                    if next == ']' {
                        break;
                    }
                    index.push(next);
                }
                let index = index
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("Unsupported Vertex jsonPath index in {path}"))?;
                tokens.push(VertexJsonPathToken::ArrayIndex(index));
            }
            _ => bail!("Unsupported Vertex partialArgs jsonPath syntax: {path}"),
        }
    }

    Ok(tokens)
}

fn vertex_partial_arg_value(partial_arg: &VertexPartialArg) -> Option<serde_json::Value> {
    if let Some(value) = partial_arg.value.get("stringValue") {
        return value.as_str().map(|value| json!(value));
    }
    if let Some(value) = partial_arg.value.get("numberValue") {
        return Some(value.clone());
    }
    if let Some(value) = partial_arg.value.get("boolValue") {
        return value.as_bool().map(|value| json!(value));
    }
    if partial_arg.value.contains_key("nullValue") {
        return Some(serde_json::Value::Null);
    }
    None
}

fn apply_vertex_partial_arg(
    target: &mut serde_json::Value,
    partial_arg: &VertexPartialArg,
) -> Result<()> {
    let Some(value) = vertex_partial_arg_value(partial_arg) else {
        return Ok(());
    };
    let tokens = parse_vertex_json_path(&partial_arg.json_path)?;
    if tokens.is_empty() {
        merge_vertex_partial_value(target, value);
        return Ok(());
    }

    let mut current = target;
    for (index, token) in tokens.iter().enumerate() {
        let is_last = index + 1 == tokens.len();
        match token {
            VertexJsonPathToken::ObjectKey(key) => {
                if !current.is_object() {
                    *current = json!({});
                }
                let object = current.as_object_mut().expect("object just created");
                if is_last {
                    let slot = object.entry(key.clone()).or_insert(serde_json::Value::Null);
                    merge_vertex_partial_value(slot, value);
                    return Ok(());
                }
                current = object
                    .entry(key.clone())
                    .or_insert_with(|| match tokens[index + 1] {
                        VertexJsonPathToken::ObjectKey(_) => json!({}),
                        VertexJsonPathToken::ArrayIndex(_) => json!([]),
                    });
            }
            VertexJsonPathToken::ArrayIndex(array_index) => {
                if !current.is_array() {
                    *current = json!([]);
                }
                let array = current.as_array_mut().expect("array just created");
                while array.len() <= *array_index {
                    array.push(serde_json::Value::Null);
                }
                if is_last {
                    merge_vertex_partial_value(&mut array[*array_index], value);
                    return Ok(());
                }
                if array[*array_index].is_null() {
                    array[*array_index] = match tokens[index + 1] {
                        VertexJsonPathToken::ObjectKey(_) => json!({}),
                        VertexJsonPathToken::ArrayIndex(_) => json!([]),
                    };
                }
                current = &mut array[*array_index];
            }
        }
    }

    Ok(())
}

fn merge_vertex_partial_value(target: &mut serde_json::Value, value: serde_json::Value) {
    match (target, value) {
        (serde_json::Value::String(existing), serde_json::Value::String(delta)) => {
            existing.push_str(&delta);
        }
        (slot, value) => *slot = value,
    }
}

pub struct VertexClient {
    client: Client,
    model: String,
    base_url: String,
    recorder: Option<APIRecorder>,
    custom_config: Option<serde_json::Value>,
    // Customization points
    auth_provider: Box<dyn AuthProvider>,
    request_customizer: Box<dyn RequestCustomizer>,
}

impl VertexClient {
    pub fn default_base_url() -> String {
        "https://generativelanguage.googleapis.com/v1beta".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            model,
            base_url,
            recorder: None,
            custom_config: None,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
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
            model,
            base_url,
            recorder: Some(APIRecorder::new(recording_path)),
            custom_config: None,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
        }
    }

    /// Create a new client with custom authentication and request handling
    pub fn with_customization(
        model: String,
        base_url: String,
        auth_provider: Box<dyn AuthProvider>,
        request_customizer: Box<dyn RequestCustomizer>,
    ) -> Self {
        Self {
            client: Client::new(),
            model,
            base_url,
            recorder: None,
            custom_config: None,
            auth_provider,
            request_customizer,
        }
    }

    /// Add recording capability to an existing client
    pub fn with_recorder<P: AsRef<std::path::Path>>(mut self, recording_path: P) -> Self {
        self.recorder = Some(APIRecorder::new(recording_path));
        self
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.custom_config = Some(custom_config);
        self
    }

    fn get_url(&self, streaming: bool) -> String {
        self.request_customizer
            .customize_url(&self.base_url, &self.model, streaming)
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
                        ..
                    } => Some(VertexPart {
                        text: Some(thinking.clone()),
                        inline_data: None,
                        thought: Some(true),
                        thought_signature: Some(signature.clone()),
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::Text { text, .. } => Some(VertexPart {
                        text: Some(text.clone()),
                        inline_data: None,
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                    }),
                    ContentBlock::Image {
                        media_type, data, ..
                    } => Some(VertexPart {
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
                    ContentBlock::ToolUse {
                        name,
                        input,
                        thought_signature,
                        ..
                    } => Some(VertexPart {
                        text: None,
                        inline_data: None,
                        thought: None,
                        thought_signature: thought_signature.clone(),
                        function_call: Some(VertexFunctionCall {
                            name: Some(name.clone()),
                            args: Some(input.clone()),
                            partial_args: Vec::new(),
                            will_continue: None,
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
                            // Wrap content in a proper JSON object (text only)
                            response: json!({ "result": content.text_content() }),
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

        let mut request_json = serde_json::to_value(request)?;

        // Apply custom model configuration if present
        if let Some(ref custom_config) = self.custom_config {
            request_json = crate::config_merge::merge_json(request_json, custom_config.clone());
        }

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut request_json)?;

        debug!(
            "Sending Vertex request to {}:\n{}",
            self.model,
            serde_json::to_string_pretty(&request_json)?
        );

        // Get authentication
        let auth = self.auth_provider.get_auth().await?;

        // Build request
        let mut request_builder = self.client.post(&url);

        // Add query parameters from auth
        if !auth.query_params.is_empty() {
            request_builder = request_builder.query(&auth.query_params);
        }

        // Add headers from auth
        for (key, value) in auth.headers {
            request_builder = request_builder.header(key, value);
        }

        // Add additional headers from customizer
        for (key, value) in self.request_customizer.get_additional_headers() {
            request_builder = request_builder.header(key, value);
        }

        let response = request_builder
            .json(&request_json)
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
                                let tool_id = format!("tool-{request_id}-{tool_counter}");
                                ContentBlock::ToolUse {
                                    id: tool_id,
                                    name: function_call.name.unwrap_or_default(),
                                    input: function_call.args.unwrap_or_default(),
                                    thought_signature: part.thought_signature.clone(),
                                    start_time: None,
                                    end_time: None,
                                }
                            } else if let Some(text) = part.text {
                                // Check if this is a thinking part
                                if part.thought == Some(true) {
                                    ContentBlock::Thinking {
                                        thinking: text,
                                        signature: part.thought_signature.unwrap_or_default(),
                                        start_time: None,
                                        end_time: None,
                                    }
                                } else {
                                    ContentBlock::Text {
                                        text,
                                        start_time: None,
                                        end_time: None,
                                    }
                                }
                            } else {
                                // Fallback if neither function_call nor text is present
                                ContentBlock::Text {
                                    text: "Empty response part".to_string(),
                                    start_time: None,
                                    end_time: None,
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
        let mut request_json = serde_json::to_value(request)?;

        // Apply custom model configuration if present
        if let Some(ref custom_config) = self.custom_config {
            request_json = crate::config_merge::merge_json(request_json, custom_config.clone());
        }

        enable_streaming_function_call_arguments(&mut request_json);

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut request_json)?;

        debug!(
            "Sending Vertex streaming request to {}:\n{}",
            self.model,
            serde_json::to_string_pretty(&request_json)?
        );

        // Start recording if a recorder is available
        if let Some(recorder) = &self.recorder {
            recorder.start_recording(request_json.clone())?;
        }

        // Get authentication
        let auth = self.auth_provider.get_auth().await?;

        // Build request - start with URL and add SSE alt parameter
        let mut request_builder = self.client.post(self.get_url(true));

        // Combine auth query params with alt=sse
        let mut query_params = auth.query_params;
        query_params.push(("alt".to_string(), "sse".to_string()));
        request_builder = request_builder.query(&query_params);

        // Add headers from auth
        for (key, value) in auth.headers {
            request_builder = request_builder.header(key, value);
        }

        // Add additional headers from customizer
        for (key, value) in self.request_customizer.get_additional_headers() {
            request_builder = request_builder.header(key, value);
        }

        let response = request_builder
            .json(&request_json)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let mut response = utils::check_response_error::<VertexRateLimitInfo>(response).await?;
        let rate_limits = VertexRateLimitInfo::from_response(&response);

        let mut state = VertexStreamingState::new();
        let mut line_buffer = String::new();

        // Helper function to process SSE lines
        let process_sse_line = |line: &str,
                                state: &mut VertexStreamingState,
                                callback: &StreamingCallback,
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
                        state.last_usage = Some(usage_metadata);
                    }
                    // Process candidates and their content parts if present
                    if let Some(candidate) = response.candidates.first() {
                        for part in &candidate.content.parts {
                            if let Some(text) = &part.text {
                                // Check if this is a thinking part
                                if part.thought == Some(true) {
                                    // Check if we can extend the last thinking block or need to create a new one
                                    match state.content_blocks.last_mut() {
                                        Some(ContentBlock::Thinking {
                                            thinking,
                                            signature,
                                            ..
                                        }) => {
                                            // Extend existing thinking block
                                            thinking.push_str(text);
                                            // Update signature if provided
                                            if let Some(new_signature) = &part.thought_signature {
                                                *signature = new_signature.clone();
                                            }
                                        }
                                        _ => {
                                            // Complete the previous block if it exists
                                            finish_last_block(&mut state.content_blocks);

                                            // Create new thinking block
                                            state.content_blocks.push(ContentBlock::Thinking {
                                                thinking: text.clone(),
                                                signature: part
                                                    .thought_signature
                                                    .clone()
                                                    .unwrap_or_default(),
                                                start_time: Some(SystemTime::now()),
                                                end_time: None,
                                            });
                                        }
                                    }
                                    // Stream thinking content
                                    callback(&StreamingChunk::Thinking(text.clone()))?;
                                } else {
                                    // Check if we can extend the last text block or need to create a new one
                                    match state.content_blocks.last_mut() {
                                        Some(ContentBlock::Text {
                                            text: last_text, ..
                                        }) => {
                                            // Extend existing text block
                                            last_text.push_str(text);
                                        }
                                        _ => {
                                            finish_last_block(&mut state.content_blocks);

                                            // Create new text block
                                            state.content_blocks.push(ContentBlock::Text {
                                                text: text.clone(),
                                                start_time: Some(SystemTime::now()),
                                                end_time: None,
                                            });
                                        }
                                    }
                                    // Regular text content
                                    callback(&StreamingChunk::Text(text.clone()))?;
                                }
                            } else if let Some(function_call) = &part.function_call {
                                handle_vertex_function_call(
                                    function_call,
                                    part.thought_signature.clone(),
                                    state,
                                    request_id,
                                    callback,
                                )?;
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
                            &mut state,
                            streaming_callback,
                            request_id,
                            &self.recorder,
                        ) {
                            Ok(()) => {
                                line_buffer.clear();
                                continue;
                            }
                            Err(e) if e.to_string().contains("Tool limit reached") => {
                                debug!("Tool limit reached, stopping streaming early. Collected {} blocks so far", state.content_blocks.len());

                                finish_last_block(&mut state.content_blocks);

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
                &mut state,
                streaming_callback,
                request_id,
                &self.recorder,
            )?;
        }

        finish_open_vertex_tool_calls(&mut state, streaming_callback)?;
        finish_last_block(&mut state.content_blocks);

        // Send StreamingComplete to indicate streaming has finished
        streaming_callback(&StreamingChunk::StreamingComplete)?;

        // End recording if a recorder is available
        if let Some(recorder) = &self.recorder {
            recorder.end_recording()?;
        }

        Ok((
            LLMResponse {
                content: state.content_blocks,
                usage: if let Some(usage_metadata) = state.last_usage {
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

        let request_start = std::time::SystemTime::now();
        let mut response = self
            .send_with_retry(&vertex_request, request_id, streaming_callback, 3)
            .await?;
        let response_end = std::time::SystemTime::now();

        // For non-streaming responses, distribute timestamps across blocks
        if streaming_callback.is_none() {
            response.set_distributed_timestamps(request_start, response_end);
        }

        Ok(response)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn partial_string(path: &str, value: &str, will_continue: Option<bool>) -> VertexPartialArg {
        let mut partial_value = serde_json::Map::new();
        partial_value.insert("stringValue".to_string(), json!(value));
        VertexPartialArg {
            json_path: path.to_string(),
            will_continue,
            value: partial_value,
        }
    }

    fn partial_number(path: &str, value: i64) -> VertexPartialArg {
        let mut partial_value = serde_json::Map::new();
        partial_value.insert("numberValue".to_string(), json!(value));
        VertexPartialArg {
            json_path: path.to_string(),
            will_continue: None,
            value: partial_value,
        }
    }

    fn capture_callback() -> (StreamingCallback, Arc<Mutex<Vec<StreamingChunk>>>) {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let captured = chunks.clone();
        let callback = Box::new(move |chunk: &StreamingChunk| {
            captured.lock().unwrap().push(chunk.clone());
            Ok(())
        });
        (callback, chunks)
    }

    fn input_json_chunks(chunks: &Arc<Mutex<Vec<StreamingChunk>>>) -> Vec<StreamingChunk> {
        chunks
            .lock()
            .unwrap()
            .iter()
            .filter(|chunk| matches!(chunk, StreamingChunk::InputJson { .. }))
            .cloned()
            .collect()
    }

    #[test]
    fn streaming_requests_enable_vertex_function_call_argument_streaming() {
        let mut request = json!({
            "contents": [],
            "tools": [{
                "function_declarations": []
            }]
        });

        enable_streaming_function_call_arguments(&mut request);

        assert_eq!(
            request["tool_config"]["function_calling_config"],
            json!({
                "mode": "AUTO",
                "stream_function_call_arguments": true,
            })
        );

        let mut request_without_tools = json!({ "contents": [] });
        enable_streaming_function_call_arguments(&mut request_without_tools);
        assert!(request_without_tools.get("tool_config").is_none());
    }

    #[test]
    fn vertex_partial_args_merge_into_nested_tool_input() -> Result<()> {
        let mut input = json!({});

        apply_vertex_partial_arg(&mut input, &partial_string("$.scene", "dot ", Some(true)))?;
        apply_vertex_partial_arg(&mut input, &partial_string("$.scene", "A", Some(false)))?;
        apply_vertex_partial_arg(&mut input, &partial_number("$.items[0].count", 2))?;

        assert_eq!(
            input,
            json!({
                "scene": "dot A",
                "items": [{
                    "count": 2
                }]
            })
        );

        Ok(())
    }

    #[test]
    fn vertex_streaming_extends_one_tool_call_across_partial_arg_events() -> Result<()> {
        let (callback, chunks) = capture_callback();
        let mut state = VertexStreamingState::new();

        handle_vertex_function_call(
            &VertexFunctionCall {
                name: Some("set_scene".to_string()),
                args: None,
                partial_args: Vec::new(),
                will_continue: Some(true),
            },
            None,
            &mut state,
            7,
            &callback,
        )?;
        handle_vertex_function_call(
            &VertexFunctionCall {
                name: Some("set_scene".to_string()),
                args: None,
                partial_args: vec![partial_string("$.scene", "#A 10 ", Some(true))],
                will_continue: Some(true),
            },
            None,
            &mut state,
            7,
            &callback,
        )?;
        handle_vertex_function_call(
            &VertexFunctionCall {
                name: None,
                args: None,
                partial_args: vec![partial_string("$.scene", "20", Some(false))],
                will_continue: Some(true),
            },
            None,
            &mut state,
            7,
            &callback,
        )?;
        handle_vertex_function_call(
            &VertexFunctionCall {
                name: None,
                args: None,
                partial_args: Vec::new(),
                will_continue: Some(false),
            },
            None,
            &mut state,
            7,
            &callback,
        )?;

        assert_eq!(state.content_blocks.len(), 1);
        match &state.content_blocks[0] {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                assert_eq!(id, "tool-7-1");
                assert_eq!(name, "set_scene");
                assert_eq!(input, &json!({ "scene": "#A 10 20" }));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }

        let chunks = input_json_chunks(&chunks);
        assert_eq!(chunks.len(), 2);
        match &chunks[0] {
            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                assert_eq!(content, "");
                assert_eq!(tool_name.as_deref(), Some("set_scene"));
                assert_eq!(tool_id.as_deref(), Some("tool-7-1"));
            }
            other => panic!("expected InputJson chunk, got {other:?}"),
        }
        match &chunks[1] {
            StreamingChunk::InputJson {
                content,
                tool_name,
                tool_id,
            } => {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(content)?,
                    json!({ "scene": "#A 10 20" })
                );
                assert_eq!(tool_name.as_deref(), Some("set_scene"));
                assert_eq!(tool_id.as_deref(), Some("tool-7-1"));
            }
            other => panic!("expected InputJson chunk, got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn vertex_streaming_keeps_multiple_tool_calls_as_separate_blocks() -> Result<()> {
        let (callback, chunks) = capture_callback();
        let mut state = VertexStreamingState::new();

        handle_vertex_function_call(
            &VertexFunctionCall {
                name: Some("get_weather".to_string()),
                args: Some(json!({ "location": "New Delhi" })),
                partial_args: Vec::new(),
                will_continue: None,
            },
            None,
            &mut state,
            3,
            &callback,
        )?;
        handle_vertex_function_call(
            &VertexFunctionCall {
                name: Some("get_weather".to_string()),
                args: Some(json!({ "location": "San Francisco" })),
                partial_args: Vec::new(),
                will_continue: None,
            },
            None,
            &mut state,
            3,
            &callback,
        )?;

        assert_eq!(state.content_blocks.len(), 2);
        match &state.content_blocks[0] {
            ContentBlock::ToolUse { id, input, .. } => {
                assert_eq!(id, "tool-3-1");
                assert_eq!(input, &json!({ "location": "New Delhi" }));
            }
            other => panic!("expected first ToolUse block, got {other:?}"),
        }
        match &state.content_blocks[1] {
            ContentBlock::ToolUse { id, input, .. } => {
                assert_eq!(id, "tool-3-2");
                assert_eq!(input, &json!({ "location": "San Francisco" }));
            }
            other => panic!("expected second ToolUse block, got {other:?}"),
        }

        let chunks = input_json_chunks(&chunks);
        assert_eq!(chunks.len(), 4);
        assert!(matches!(
            &chunks[0],
            StreamingChunk::InputJson { content, tool_id, .. }
                if content.is_empty() && tool_id.as_deref() == Some("tool-3-1")
        ));
        assert!(matches!(
            &chunks[2],
            StreamingChunk::InputJson { content, tool_id, .. }
                if content.is_empty() && tool_id.as_deref() == Some("tool-3-2")
        ));

        Ok(())
    }
}
