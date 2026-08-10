//! OpenAI Responses API Provider — WebSocket Transport
//!
//! This module implements an LLM provider that uses a persistent WebSocket connection
//! to the OpenAI Responses API, matching the protocol used by the Codex CLI.
//!
//! ## Key differences from the HTTP/SSE transport (`openai_responses.rs`)
//!
//! - **Persistent connection**: A single WebSocket connection is reused across multiple
//!   requests within a session instead of creating a new HTTP POST per turn.
//! - **Message framing**: Requests are sent as JSON text frames tagged with
//!   `{"type": "response.create", ...}`. Responses arrive as individual JSON text frames
//!   (the same event types as SSE but without the `data: ` prefix).
//! - **Incremental input**: When the current input is a strict extension of the previous
//!   request, only the delta items are sent along with `previous_response_id`.
//! - **Wrapped errors**: The server may send top-level `{"type": "error", "status": 429, ...}`
//!   frames in addition to `response.failed` events.
//!
//! ## Keepalive
//!
//! A background reader task continuously reads from the WebSocket stream. It responds
//! to server pings with pongs even when no request is active, preventing the server
//! from closing the connection due to keepalive timeout. Application-level frames
//! (Text, Close) are forwarded through an mpsc channel to `process_ws_stream`.
//!
//! ## Usage
//!
//! The provider is created via [`OpenAIResponsesWsClient::new`] (API-key auth) or
//! [`OpenAIResponsesWsClient::with_customization`] (custom auth, e.g. Codex OAuth).
//! It implements the same [`LLMProvider`] trait as all other providers.

use crate::{
    LLMProvider, StreamingCallback, StreamingChunk,
    openai_responses::{ApiKeyAuth, AuthProvider, RequestCustomizer},
    types::*,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
        protocol::Message as WsMessage,
    },
};
use tracing::{debug, info, warn};

// Re-export types shared with the HTTP provider
use crate::openai_responses::{PromptCacheBreakpoint, Verbosity, model_supports_explicit_cache};

// ============================================================================
// Request / Response types (WebSocket-specific envelope)
// ============================================================================

/// WebSocket request envelope — tagged with `"type": "response.create"`.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsRequest {
    #[serde(rename = "response.create")]
    ResponseCreate(WsResponseCreateRequest),
}

/// The body of a `response.create` WebSocket message.
#[derive(Debug, Serialize)]
struct WsResponseCreateRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    input: Vec<WsInputItem>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<TextControls>,
}

/// Reasoning configuration for the request.
#[derive(Debug, Serialize, Clone)]
struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

/// Text controls for the Responses API.
#[derive(Debug, Serialize, Clone)]
struct TextControls {
    #[serde(skip_serializing_if = "Option::is_none")]
    verbosity: Option<Verbosity>,
}

// ---------------------------------------------------------------------------
// Input items (sent to the API)
// ---------------------------------------------------------------------------

/// Input item in the WebSocket request payload.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsInputItem {
    Message {
        role: String,
        content: Vec<WsContentItem>,
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

/// Content item within messages.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsContentItem {
    InputText {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_breakpoint: Option<PromptCacheBreakpoint>,
    },
    InputImage {
        image_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_breakpoint: Option<PromptCacheBreakpoint>,
    },
    OutputText {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
}

impl WsContentItem {
    fn input_text(text: String) -> Self {
        Self::InputText {
            text,
            prompt_cache_breakpoint: None,
        }
    }

    fn input_image(image_url: String) -> Self {
        Self::InputImage {
            image_url,
            prompt_cache_breakpoint: None,
        }
    }

    /// Set the explicit cache breakpoint if this is an input block that
    /// supports one. Returns whether the marker could be applied.
    fn try_mark_cache_breakpoint(&mut self) -> bool {
        match self {
            Self::InputText {
                prompt_cache_breakpoint,
                ..
            }
            | Self::InputImage {
                prompt_cache_breakpoint,
                ..
            } => {
                *prompt_cache_breakpoint = Some(PromptCacheBreakpoint::explicit());
                true
            }
            Self::OutputText { .. } => false,
        }
    }
}

/// Place an explicit cache breakpoint on the last breakpoint-eligible content
/// block within `items[..end]`.
///
/// A breakpoint marks the end of the cached prefix including everything
/// rendered before it, but is only valid on input blocks (`input_text`,
/// `input_image`). Function calls, tool outputs and assistant text cannot
/// carry one, so the marker is shifted backwards to the nearest eligible
/// block. The scan only depends on content before the anchor, which is stable
/// across requests, so placement stays deterministic.
///
/// Markers serialize as part of the input items, so `compute_delta` naturally
/// forces a full resend whenever marker positions move — the previously
/// cached token prefix stays valid because markers are not prompt content.
fn apply_cache_breakpoint(items: &mut [WsInputItem], end: usize) {
    for item in items[..end].iter_mut().rev() {
        if let WsInputItem::Message { content, .. } = item {
            for content_item in content.iter_mut().rev() {
                if content_item.try_mark_cache_breakpoint() {
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Output / event types (received from the API)
// ---------------------------------------------------------------------------

/// A streaming event received as a JSON text frame over WebSocket.
/// The same event types as SSE but delivered as bare JSON objects.
#[derive(Debug, Deserialize)]
struct WsStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    response: Option<serde_json::Value>,
    #[serde(default)]
    item: Option<serde_json::Value>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    item_id: Option<String>,

    // WebSocket-specific fields for wrapped errors
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    error: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    headers: Option<serde_json::Value>,
}

/// Output item from a completed response.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsResponseOutputItem {
    Message {
        #[serde(default)]
        #[allow(dead_code)]
        id: Option<String>,
        #[allow(dead_code)]
        role: String,
        content: Vec<WsResponseOutputContent>,
    },
    Reasoning {
        #[allow(dead_code)]
        id: String,
        #[serde(default)]
        summary: Vec<WsReasoningSummary>,
        #[serde(default)]
        content: Vec<WsReasoningContent>,
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

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsResponseOutputContent {
    OutputText { text: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsReasoningSummary {
    SummaryText { text: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsReasoningContent {
    ReasoningText { text: String },
}

/// Usage information from the completed response.
#[derive(Debug, Deserialize)]
struct WsResponsesUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
    #[serde(default)]
    input_tokens_details: Option<WsInputTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct WsInputTokensDetails {
    cached_tokens: u32,
    /// Tokens written to the prompt cache (explicit caching, GPT-5.6+).
    #[serde(default)]
    cache_write_tokens: u32,
}

impl WsResponsesUsage {
    /// Convert to the provider-independent `Usage`.
    ///
    /// OpenAI reports `input_tokens` inclusive of cached (and cache-write)
    /// tokens, while the internal `Usage` follows Anthropic semantics where
    /// `input_tokens` counts only uncached input.
    fn to_usage(&self) -> Usage {
        let (cache_read, cache_write) = self
            .input_tokens_details
            .as_ref()
            .map(|d| (d.cached_tokens, d.cache_write_tokens))
            .unwrap_or((0, 0));
        Usage {
            input_tokens: self
                .input_tokens
                .saturating_sub(cache_read.saturating_add(cache_write)),
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: cache_write,
            cache_read_input_tokens: cache_read,
        }
    }
}

// ============================================================================
// Model capabilities (shared logic, duplicated to stay self-contained)
// ============================================================================

#[derive(Debug, Clone)]
struct ModelCapabilities {
    supports_reasoning: bool,
    default_effort: Option<String>,
    default_summary: Option<String>,
    supports_verbosity: bool,
    default_verbosity: Option<Verbosity>,
    /// Whether the model supports explicit prompt cache breakpoints
    /// (`prompt_cache_breakpoint`, GPT-5.6 and later; older models reject the field)
    supports_explicit_cache: bool,
}

impl ModelCapabilities {
    fn for_model(model: &str) -> Self {
        let m = model.to_lowercase();
        let supports_explicit_cache = model_supports_explicit_cache(&m);

        if m.contains("gpt-5") || m.starts_with("gpt5") {
            return Self {
                supports_reasoning: true,
                default_effort: Some("medium".into()),
                default_summary: Some("auto".into()),
                supports_verbosity: true,
                default_verbosity: Some(Verbosity::Medium),
                supports_explicit_cache,
            };
        }
        if m.starts_with("o3") || m.starts_with("o4") {
            return Self {
                supports_reasoning: true,
                default_effort: Some("medium".into()),
                default_summary: Some("auto".into()),
                supports_verbosity: false,
                default_verbosity: None,
                supports_explicit_cache,
            };
        }
        if m.starts_with("o1") {
            return Self {
                supports_reasoning: true,
                default_effort: Some("low".into()),
                default_summary: Some("auto".into()),
                supports_verbosity: false,
                default_verbosity: None,
                supports_explicit_cache,
            };
        }
        if m.contains("gpt-4o") || m.contains("gpt4o") || m.contains("gpt-4") || m.contains("gpt4")
        {
            return Self {
                supports_reasoning: false,
                default_effort: None,
                default_summary: None,
                supports_verbosity: false,
                default_verbosity: None,
                supports_explicit_cache,
            };
        }
        // Default: assume reasoning support with conservative defaults
        Self {
            supports_reasoning: true,
            default_effort: Some("low".into()),
            default_summary: Some("auto".into()),
            supports_verbosity: false,
            default_verbosity: None,
            supports_explicit_cache,
        }
    }
}

// ============================================================================
// Connection wrapper with background keepalive
// ============================================================================

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// Frame forwarded from the background reader to the foreground consumer.
/// Ping/Pong are handled internally by the reader task and never forwarded.
enum ReaderFrame {
    /// A JSON text frame (API event).
    Text(String),
    /// The server closed the connection.
    Close(Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>),
    /// The WebSocket stream ended or encountered a read error.
    Error(String),
}

/// A persistent WebSocket connection with background keepalive.
///
/// The background reader task continuously reads from the raw `WsStream`,
/// responds to server pings with pongs (keeping the connection alive), and
/// forwards application-level frames through the `rx` channel.
struct WsConnection {
    /// Shared sink — used by both the background reader (for pong) and the
    /// foreground (for sending requests).
    sink: Arc<TokioMutex<WsSink>>,
    /// Receiver for frames forwarded by the background reader.
    rx: mpsc::UnboundedReceiver<ReaderFrame>,
    /// Handle to the background reader task (aborted on drop).
    _reader_handle: tokio::task::JoinHandle<()>,
}

/// Spawn the background reader task.
///
/// Reads frames from `stream`, responds to Ping with Pong via `sink`,
/// and forwards Text / Close / error frames through `tx`.
fn spawn_reader_task(
    mut stream: WsStream,
    sink: Arc<TokioMutex<WsSink>>,
    tx: mpsc::UnboundedSender<ReaderFrame>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(msg)) => match msg {
                    WsMessage::Ping(data) => {
                        let mut sink_guard = sink.lock().await;
                        if sink_guard.send(WsMessage::Pong(data)).await.is_err() {
                            debug!("WS reader: failed to send pong, sink closed");
                            let _ = tx.send(ReaderFrame::Error(
                                "Failed to send pong — sink closed".into(),
                            ));
                            break;
                        }
                    }
                    WsMessage::Pong(_) => {
                        // Ignore unsolicited pongs
                    }
                    WsMessage::Text(text) => {
                        if tx.send(ReaderFrame::Text(text.to_string())).is_err() {
                            // Receiver dropped — stop reading
                            break;
                        }
                    }
                    WsMessage::Close(frame) => {
                        let _ = tx.send(ReaderFrame::Close(frame));
                        break;
                    }
                    WsMessage::Binary(_) => {
                        debug!("WS reader: ignoring unexpected binary frame");
                    }
                    _ => {
                        debug!("WS reader: ignoring unexpected frame type");
                    }
                },
                Some(Err(e)) => {
                    let _ = tx.send(ReaderFrame::Error(format!("WebSocket read error: {e}")));
                    break;
                }
                None => {
                    // Stream ended
                    let _ = tx.send(ReaderFrame::Error(
                        "WebSocket stream ended unexpectedly".into(),
                    ));
                    break;
                }
            }
        }
    })
}

// ============================================================================
// The WebSocket LLM Provider
// ============================================================================

/// An LLM provider that communicates with the OpenAI Responses API over WebSocket.
pub struct OpenAIResponsesWsClient {
    base_url: String,
    model: String,
    auth_provider: Box<dyn AuthProvider>,
    request_customizer: Box<dyn RequestCustomizer>,
    custom_config: Option<serde_json::Value>,
    /// Persistent WebSocket connection, reused across turns.
    connection: Option<WsConnection>,
    /// The response ID from the last completed response (for incremental input).
    last_response_id: Option<String>,
    /// The input items sent in the previous request (for delta computation).
    last_input_items: Vec<WsInputItem>,
    /// Idle timeout for reading from the WebSocket (per-event).
    idle_timeout: Duration,
}

impl OpenAIResponsesWsClient {
    pub fn default_base_url() -> String {
        "https://api.openai.com/v1".to_string()
    }

    /// Create a new WebSocket Responses API client with API key authentication.
    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            base_url,
            model,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(WsDefaultRequestCustomizer),
            custom_config: None,
            connection: None,
            last_response_id: None,
            last_input_items: Vec::new(),
            idle_timeout: Duration::from_secs(600), // 10 min default
        }
    }

    /// Create a new WebSocket Responses API client with custom auth/request handling.
    pub fn with_customization(
        model: String,
        base_url: String,
        auth_provider: Box<dyn AuthProvider>,
        request_customizer: Box<dyn RequestCustomizer>,
    ) -> Self {
        Self {
            base_url,
            model,
            auth_provider,
            request_customizer,
            custom_config: None,
            connection: None,
            last_response_id: None,
            last_input_items: Vec::new(),
            idle_timeout: Duration::from_secs(600),
        }
    }

    /// Set custom model configuration to be merged into API requests.
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.custom_config = Some(custom_config);
        self
    }

    // -----------------------------------------------------------------------
    // Connection management
    // -----------------------------------------------------------------------

    /// Get the WebSocket URL by converting https → wss (or http → ws).
    fn ws_url(&self) -> Result<String> {
        let http_url = self.request_customizer.customize_url(&self.base_url, true);

        let ws_url = if http_url.starts_with("https://") {
            http_url.replacen("https://", "wss://", 1)
        } else if http_url.starts_with("http://") {
            http_url.replacen("http://", "ws://", 1)
        } else {
            http_url
        };
        Ok(ws_url)
    }

    /// Ensure we have a live WebSocket connection, creating one if necessary.
    ///
    /// If a connection exists but the background reader task has already
    /// exited (server closed the socket, TCP timeout, etc.) the stale
    /// connection is dropped and a fresh one is established.
    async fn ensure_connection(&mut self) -> Result<()> {
        if let Some(conn) = self.connection.as_ref() {
            if conn._reader_handle.is_finished() {
                info!("WebSocket reader task has ended — connection is stale, reconnecting");
                self.drop_connection();
            } else {
                return Ok(());
            }
        }

        let url_str = self.ws_url()?;
        info!("Connecting WebSocket to {}", url_str);

        // Build the request with auth + custom headers
        let mut request = url_str
            .clone()
            .into_client_request()
            .context("Failed to build WebSocket request")?;
        let auth_headers = self.auth_provider.get_auth_headers().await?;
        info!("  WS auth: {} header(s) provided", auth_headers.len());
        for (key, value) in &auth_headers {
            let redacted = if value.len() > 12 {
                format!("{}…{}", &value[..6], &value[value.len() - 4..])
            } else {
                "(short)".to_string()
            };
            debug!("  WS auth header: {} = {}", key, redacted);
            request.headers_mut().insert(
                HeaderName::from_bytes(key.as_bytes()).context("Invalid header name")?,
                HeaderValue::from_str(value).context("Invalid header value")?,
            );
        }
        for (key, value) in self.request_customizer.get_additional_headers() {
            debug!("  WS custom header: {} = {}", key, value);
            request.headers_mut().insert(
                HeaderName::from_bytes(key.as_bytes()).context("Invalid header name")?,
                HeaderValue::from_str(&value).context("Invalid header value")?,
            );
        }

        let (ws_stream, response) = connect_async_with_config(request, None, false)
            .await
            .map_err(|e| {
                warn!("WebSocket connection to {} failed: {:#}", url_str, e);
                anyhow::anyhow!("WebSocket connection failed: {}", e)
            })?;

        // Log handshake response details for debugging
        info!(
            "WebSocket connected to {} — status: {}, headers: {:?}",
            url_str,
            response.status(),
            response.headers()
        );

        let (sink, stream) = ws_stream.split();
        let shared_sink = Arc::new(TokioMutex::new(sink));
        let (tx, rx) = mpsc::unbounded_channel();
        let reader_handle = spawn_reader_task(stream, Arc::clone(&shared_sink), tx);

        self.connection = Some(WsConnection {
            sink: shared_sink,
            rx,
            _reader_handle: reader_handle,
        });
        Ok(())
    }

    /// Drop the current connection (e.g. after an error).
    ///
    /// This aborts the background reader task and clears incremental state.
    fn drop_connection(&mut self) {
        if let Some(conn) = self.connection.take() {
            conn._reader_handle.abort();
        }
        self.last_response_id = None;
        self.last_input_items.clear();
    }

    // -----------------------------------------------------------------------
    // Message conversion (internal types → WS input items)
    // -----------------------------------------------------------------------

    /// Convert internal messages to WS input items, optionally placing
    /// explicit cache breakpoints at stable history positions.
    fn convert_messages_with_cache(
        &self,
        messages: Vec<Message>,
        explicit_cache: bool,
    ) -> Vec<WsInputItem> {
        let cache_positions = if explicit_cache {
            crate::prompt_caching::cache_marker_positions(&messages)
        } else {
            Vec::new()
        };

        let mut items = Vec::new();
        // Number of converted items after each message, so marker positions
        // (message indices) can be mapped back to converted input items.
        let mut items_after_message = Vec::with_capacity(messages.len());

        for message in messages {
            match &message.content {
                MessageContent::Text(text) => {
                    let role = match message.role {
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    };
                    // Assistant text must use OutputText; InputText is only
                    // valid for user-role messages. Simple text messages have
                    // no tool use, so they always get phase = "final_answer".
                    let content_item = if message.role == MessageRole::Assistant {
                        WsContentItem::OutputText {
                            text: text.clone(),
                            phase: Some("final_answer".to_string()),
                        }
                    } else {
                        WsContentItem::input_text(text.clone())
                    };
                    items.push(WsInputItem::Message {
                        role: role.to_string(),
                        content: vec![content_item],
                    });
                }
                MessageContent::Structured(blocks) => {
                    self.convert_structured_message(&message.role, blocks, &mut items);
                }
            }
            items_after_message.push(items.len());
        }

        for &position in &cache_positions {
            apply_cache_breakpoint(&mut items, items_after_message[position]);
        }

        items
    }

    fn convert_structured_message(
        &self,
        role: &MessageRole,
        blocks: &[ContentBlock],
        items: &mut Vec<WsInputItem>,
    ) {
        let role_str = match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        };

        // Pre-scan: determine the phase for OutputText items in this message.
        let has_tool_use = *role == MessageRole::Assistant
            && blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let phase: Option<String> = if *role == MessageRole::Assistant {
            Some(if has_tool_use {
                "commentary".to_string()
            } else {
                "final_answer".to_string()
            })
        } else {
            None
        };

        let mut current_content: Vec<WsContentItem> = Vec::new();

        for block in blocks {
            match block {
                ContentBlock::Text { text, .. } => {
                    let item = if *role == MessageRole::User {
                        WsContentItem::input_text(text.clone())
                    } else {
                        WsContentItem::OutputText {
                            text: text.clone(),
                            phase: phase.clone(),
                        }
                    };
                    current_content.push(item);
                }
                ContentBlock::Image { data, .. } => {
                    current_content.push(WsContentItem::input_image(format!(
                        "data:image/png;base64,{}",
                        data
                    )));
                }
                ContentBlock::Thinking {
                    thinking,
                    signature,
                    ..
                } => {
                    // Flush any accumulated content
                    if !current_content.is_empty() {
                        items.push(WsInputItem::Message {
                            role: role_str.to_string(),
                            content: std::mem::take(&mut current_content),
                        });
                    }
                    // Thinking blocks don't have a direct input representation;
                    // the important part is RedactedThinking with encrypted_content
                    let _ = (thinking, signature);
                }
                ContentBlock::RedactedThinking {
                    id, summary, data, ..
                } => {
                    // Flush
                    if !current_content.is_empty() {
                        items.push(WsInputItem::Message {
                            role: role_str.to_string(),
                            content: std::mem::take(&mut current_content),
                        });
                    }
                    if !data.is_empty() {
                        let summary_json: Vec<serde_json::Value> = summary
                            .iter()
                            .map(|s| match s {
                                ReasoningSummaryItem::SummaryText { text } => {
                                    serde_json::json!({"type": "summary_text", "text": text})
                                }
                            })
                            .collect();
                        items.push(WsInputItem::Reasoning {
                            id: id.clone(),
                            summary: summary_json,
                            encrypted_content: data.clone(),
                        });
                    }
                }
                ContentBlock::ToolUse {
                    id: tool_id,
                    name,
                    input,
                    ..
                } => {
                    // Flush
                    if !current_content.is_empty() {
                        items.push(WsInputItem::Message {
                            role: role_str.to_string(),
                            content: std::mem::take(&mut current_content),
                        });
                    }
                    items.push(WsInputItem::FunctionCall {
                        call_id: tool_id.clone(),
                        name: name.clone(),
                        arguments: input.to_string(),
                    });
                }

                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    // Flush
                    if !current_content.is_empty() {
                        items.push(WsInputItem::Message {
                            role: role_str.to_string(),
                            content: std::mem::take(&mut current_content),
                        });
                    }
                    items.push(WsInputItem::FunctionCallOutput {
                        call_id: tool_use_id.clone(),
                        output: content.text_content().to_string(),
                    });
                }
            }
        }

        // Flush remaining
        if !current_content.is_empty() {
            items.push(WsInputItem::Message {
                role: role_str.to_string(),
                content: current_content,
            });
        }
    }

    // -----------------------------------------------------------------------
    // Response ContentBlocks → WsInputItems (for tracking server-known state)
    // -----------------------------------------------------------------------

    /// Convert response ContentBlocks back into WsInputItems so they can be
    /// appended to `last_input_items`. This lets the delta computation know
    /// which items the server already has (from its own response output).
    fn response_blocks_to_input_items(blocks: &[ContentBlock]) -> Vec<WsInputItem> {
        let mut items = Vec::new();
        let mut current_text_parts: Vec<WsContentItem> = Vec::new();

        // Pre-scan to determine the phase for OutputText items. The response
        // blocks from a single assistant turn share the same phase.
        let has_tool_use = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let phase: Option<String> = Some(if has_tool_use {
            "commentary".to_string()
        } else {
            "final_answer".to_string()
        });

        for block in blocks {
            match block {
                ContentBlock::Text { text, .. } => {
                    current_text_parts.push(WsContentItem::OutputText {
                        text: text.clone(),
                        phase: phase.clone(),
                    });
                }
                ContentBlock::Thinking { .. } => {
                    // Visible thinking — no standard input representation, skip
                }
                ContentBlock::RedactedThinking {
                    id, summary, data, ..
                } => {
                    // Flush any accumulated text as an assistant message first
                    if !current_text_parts.is_empty() {
                        items.push(WsInputItem::Message {
                            role: "assistant".to_string(),
                            content: std::mem::take(&mut current_text_parts),
                        });
                    }
                    if !data.is_empty() {
                        let summary_json: Vec<serde_json::Value> = summary
                            .iter()
                            .map(|s| match s {
                                ReasoningSummaryItem::SummaryText { text } => {
                                    serde_json::json!({"type": "summary_text", "text": text})
                                }
                            })
                            .collect();
                        items.push(WsInputItem::Reasoning {
                            id: id.clone(),
                            summary: summary_json,
                            encrypted_content: data.clone(),
                        });
                    }
                }
                ContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input,
                    ..
                } => {
                    // Flush text
                    if !current_text_parts.is_empty() {
                        items.push(WsInputItem::Message {
                            role: "assistant".to_string(),
                            content: std::mem::take(&mut current_text_parts),
                        });
                    }
                    items.push(WsInputItem::FunctionCall {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: input.to_string(),
                    });
                }

                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    // Flush text
                    if !current_text_parts.is_empty() {
                        items.push(WsInputItem::Message {
                            role: "assistant".to_string(),
                            content: std::mem::take(&mut current_text_parts),
                        });
                    }
                    items.push(WsInputItem::FunctionCallOutput {
                        call_id: tool_use_id.clone(),
                        output: content.text_content().to_string(),
                    });
                }
                ContentBlock::Image { .. } => {
                    // Images in responses are not round-tripped
                }
            }
        }

        // Flush remaining text
        if !current_text_parts.is_empty() {
            items.push(WsInputItem::Message {
                role: "assistant".to_string(),
                content: current_text_parts,
            });
        }

        items
    }

    // -----------------------------------------------------------------------
    // Incremental input (delta computation)
    // -----------------------------------------------------------------------

    /// Check if `current` is a strict prefix-extension of the previous input
    /// and return (previous_response_id, delta_items) if so.
    fn compute_delta(&self, current: &[WsInputItem]) -> Option<(String, Vec<WsInputItem>)> {
        let prev_id = self.last_response_id.as_ref()?;
        let prev = &self.last_input_items;

        if current.len() < prev.len() {
            return None;
        }

        // Check that the prefix matches by comparing JSON serialization
        for (old, new) in prev.iter().zip(current.iter()) {
            let old_json = serde_json::to_string(old).unwrap_or_default();
            let new_json = serde_json::to_string(new).unwrap_or_default();
            if old_json != new_json {
                return None;
            }
        }

        let delta: Vec<WsInputItem> = current[prev.len()..].to_vec();
        Some((prev_id.clone(), delta))
    }

    // -----------------------------------------------------------------------
    // Core: send a request and process the response stream
    // -----------------------------------------------------------------------

    async fn send_ws_request(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let capabilities = ModelCapabilities::for_model(&self.model);

        // Explicit cache breakpoints are only sent to models that support
        // them (older models reject the field). The system prompt travels as
        // `instructions` (a plain string, no breakpoint possible); a marker on
        // a history message still caches instructions and tools, since a
        // breakpoint covers everything rendered before it.
        let input = self
            .convert_messages_with_cache(request.messages, capabilities.supports_explicit_cache);

        // Add system prompt as the top-level `instructions` field (WebSocket style)
        // but also keep it as a developer message in `input` for compatibility.
        let instructions = if !request.system_prompt.is_empty() {
            Some(request.system_prompt.clone())
        } else {
            None
        };

        let tools = request.tools.map(|tools| {
            tools
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    })
                })
                .collect()
        });

        let reasoning = if capabilities.supports_reasoning {
            Some(ReasoningConfig {
                effort: capabilities.default_effort,
                summary: capabilities.default_summary,
            })
        } else {
            None
        };

        let include = if reasoning.is_some() {
            vec!["reasoning.encrypted_content".to_string()]
        } else {
            vec![]
        };

        let text = if capabilities.supports_verbosity {
            capabilities
                .default_verbosity
                .map(|v| TextControls { verbosity: Some(v) })
        } else {
            None
        };

        // Compute incremental delta if possible
        let (previous_response_id, send_input) =
            if let Some((prev_id, delta)) = self.compute_delta(&input) {
                debug!(
                    "Using incremental input: {} delta items (prev_response_id={})",
                    delta.len(),
                    prev_id
                );
                (Some(prev_id), delta)
            } else {
                (None, input.clone())
            };

        let ws_request = WsResponseCreateRequest {
            model: self.model.clone(),
            instructions,
            previous_response_id,
            input: send_input,
            tools,
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: false,
            reasoning,
            store: false,
            stream: true, // Always stream over WebSocket
            include,
            prompt_cache_key: Some(request.session_id),
            text,
        };

        // Apply custom config if present
        let mut request_json = serde_json::to_value(WsRequest::ResponseCreate(ws_request))?;
        if let Some(ref custom_config) = self.custom_config {
            // Merge custom config into the request (not the envelope `type` field)
            request_json = crate::config_merge::merge_json(request_json, custom_config.clone());
        }
        self.request_customizer
            .customize_request(&mut request_json)?;

        let request_text = serde_json::to_string(&request_json)?;
        info!(
            "WS request ({} bytes): {}",
            request_text.len(),
            &request_text[..request_text.len().min(1000)]
        );

        // Ensure connection
        self.ensure_connection().await?;

        // Send the request via the shared sink
        {
            let conn = self
                .connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("WebSocket connection not established"))?;
            let mut sink_guard = conn.sink.lock().await;
            sink_guard
                .send(WsMessage::Text(request_text.into()))
                .await
                .context("Failed to send WebSocket message")?;
        }

        // Process response events
        let request_start = SystemTime::now();
        let result = self.process_ws_stream(streaming_callback).await;

        match result {
            Ok((mut response, response_id)) => {
                let response_end = SystemTime::now();

                // Store state for incremental requests.
                // The server now knows about:
                //   1. Everything we sent as input
                //   2. Everything it produced as output (reasoning, text, tool calls)
                // We must track both so the next delta only contains truly new items.
                let response_output_items = Self::response_blocks_to_input_items(&response.content);
                let mut server_known_items = input;
                server_known_items.extend(response_output_items);

                self.last_response_id = response_id;
                self.last_input_items = server_known_items;

                // For non-streaming, distribute timestamps
                if streaming_callback.is_none() {
                    response.set_distributed_timestamps(request_start, response_end);
                }

                Ok(response)
            }
            Err(e) => {
                warn!("WebSocket request failed, dropping connection: {}", e);
                self.drop_connection();
                Err(e)
            }
        }
    }

    /// Read events from the channel (fed by the background reader) until
    /// `response.completed` or error. Returns (LLMResponse, Option<response_id>).
    async fn process_ws_stream(
        &mut self,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, Option<String>)> {
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut usage = Usage::zero();
        let mut response_id: Option<String> = None;

        // Stream processing state
        let mut active_function_calls: HashMap<String, FunctionCallInfo> = HashMap::new();
        let mut block_start_times: HashMap<String, SystemTime> = HashMap::new();
        let mut reasoning_state = ReasoningState::default();

        let idle_timeout = self.idle_timeout;
        let conn = self
            .connection
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No WebSocket connection"))?;

        loop {
            let frame = tokio::time::timeout(idle_timeout, conn.rx.recv())
                .await
                .context("WebSocket idle timeout")?
                .ok_or_else(|| anyhow::anyhow!("WebSocket reader task ended unexpectedly"))?;

            match frame {
                ReaderFrame::Text(text) => {
                    debug!("WS event: {}", text);

                    let event: WsStreamEvent = serde_json::from_str(&text)
                        .with_context(|| format!("Failed to parse WS event: {}", text))?;

                    // Handle wrapped WebSocket errors (top-level `"type": "error"`)
                    if event.event_type == "error" {
                        let status = event.status.unwrap_or(500);
                        let error_msg = event
                            .error
                            .as_ref()
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("Unknown WebSocket error");
                        let error_code = event
                            .error
                            .as_ref()
                            .and_then(|e| e.get("code").or_else(|| e.get("type")))
                            .and_then(|c| c.as_str())
                            .unwrap_or("unknown");

                        if status == 429 || error_code == "rate_limit_exceeded" {
                            bail!("{}", ApiError::RateLimit(error_msg.to_string()));
                        }
                        if error_code == "websocket_connection_limit_reached" {
                            // Reconnect on next request
                            self.drop_connection();
                            bail!("WebSocket connection limit reached, will reconnect");
                        }
                        bail!(
                            "WebSocket error ({}): {} - {}",
                            status,
                            error_code,
                            error_msg
                        );
                    }

                    match event.event_type.as_str() {
                        "response.created" => {
                            // Response started; nothing specific to do
                        }
                        "response.output_item.added" => {
                            if let Some(item) = event.item {
                                let now = SystemTime::now();
                                if let Some(item_type) = item.get("type").and_then(|v| v.as_str()) {
                                    let item_id = item
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();

                                    block_start_times.insert(item_id.clone(), now);

                                    if item_type == "function_call" {
                                        let call_id = item
                                            .get("call_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let name = item
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        active_function_calls
                                            .insert(item_id, FunctionCallInfo { name, call_id });
                                    } else if item_type == "reasoning" {
                                        reasoning_state.reasoning_block_id = Some(item_id);
                                    }
                                }
                            }
                        }
                        "response.output_text.delta" => {
                            if let Some(delta) = event.delta
                                && let Some(cb) = streaming_callback
                            {
                                cb(&StreamingChunk::Text(delta))?;
                            }
                        }
                        "response.reasoning_text.delta" => {
                            if let Some(delta) = event.delta
                                && let Some(cb) = streaming_callback
                            {
                                cb(&StreamingChunk::Thinking(delta))?;
                            }
                        }
                        "response.reasoning_summary_text.delta" => {
                            if let Some(delta) = event.delta {
                                let item_id =
                                    event.item_id.unwrap_or_else(|| "default".to_string());
                                if let Some(cb) = streaming_callback {
                                    if reasoning_state.current_item_id.as_ref() != Some(&item_id) {
                                        if reasoning_state.current_item_id.is_some() {
                                            reasoning_state.completed_items.push(
                                                ReasoningSummaryItem::SummaryText {
                                                    text: reasoning_state
                                                        .current_item_content
                                                        .clone(),
                                                },
                                            );
                                        }
                                        reasoning_state.current_item_id = Some(item_id.clone());
                                        reasoning_state.current_item_content.clear();
                                        cb(&StreamingChunk::ReasoningSummaryStart)?;
                                    }
                                    reasoning_state.current_item_content.push_str(&delta);
                                    cb(&StreamingChunk::ReasoningSummaryDelta(delta))?;
                                }
                            }
                        }
                        "response.reasoning_summary_part.added" => {
                            // Tracked via reasoning_summary_text.delta state changes
                        }
                        "response.function_call_arguments.delta" => {
                            if let Some(delta) = event.delta
                                && let Some(cb) = streaming_callback
                            {
                                let (tool_name, tool_id) = if let Some(id) =
                                    event.item_id.as_deref()
                                {
                                    active_function_calls
                                        .get(id)
                                        .map(|info| {
                                            (Some(info.name.clone()), Some(info.call_id.clone()))
                                        })
                                        .unwrap_or((None, None))
                                } else {
                                    (None, None)
                                };
                                cb(&StreamingChunk::InputJson {
                                    content: delta,
                                    tool_name,
                                    tool_id,
                                })?;
                            }
                        }
                        "response.output_item.done" => {
                            if let Some(item_data) = event.item {
                                let now = SystemTime::now();
                                let output_item: WsResponseOutputItem =
                                    serde_json::from_value(item_data.clone())?;

                                let item_id = item_data
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                let start_time = block_start_times.get(&item_id).copied();

                                let mut converted = convert_output_items(vec![output_item]);

                                for block in &mut converted {
                                    if let Some(start) = start_time {
                                        block.set_timestamps(start, now);
                                    }
                                }

                                for block in &converted {
                                    if matches!(block, ContentBlock::RedactedThinking { .. }) {
                                        reasoning_state.received_completed_reasoning = true;
                                    }
                                }

                                content_blocks.extend(converted);
                            }
                        }
                        "response.completed" => {
                            // Finalize reasoning state
                            if reasoning_state.current_item_id.is_some() {
                                reasoning_state.completed_items.push(
                                    ReasoningSummaryItem::SummaryText {
                                        text: reasoning_state.current_item_content.clone(),
                                    },
                                );
                                reasoning_state.current_item_id = None;
                                reasoning_state.current_item_content.clear();
                            }

                            if !reasoning_state.completed_items.is_empty()
                                && let Some(cb) = streaming_callback
                            {
                                cb(&StreamingChunk::ReasoningComplete)?;
                            }

                            // Extract usage and response_id
                            if let Some(resp) = &event.response {
                                response_id = resp
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                if let Some(usage_val) = resp.get("usage")
                                    && let Ok(u) = serde_json::from_value::<WsResponsesUsage>(
                                        usage_val.clone(),
                                    )
                                {
                                    usage = u.to_usage();
                                }
                            }

                            // Create RedactedThinking from streaming reasoning if needed
                            if !reasoning_state.completed_items.is_empty()
                                && !reasoning_state.received_completed_reasoning
                            {
                                content_blocks.push(ContentBlock::RedactedThinking {
                                    id: reasoning_state
                                        .reasoning_block_id
                                        .clone()
                                        .unwrap_or_else(|| "reasoning_stream".to_string()),
                                    summary: reasoning_state.completed_items.clone(),
                                    data: String::new(),
                                    start_time: None,
                                    end_time: None,
                                });
                            }

                            if let Some(cb) = streaming_callback {
                                cb(&StreamingChunk::StreamingComplete)?;
                            }

                            break; // Done with this response
                        }
                        "response.failed" => {
                            let error_msg = event
                                .response
                                .as_ref()
                                .and_then(|r| r.get("error"))
                                .and_then(|e| e.get("message"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("Unknown error");
                            let error_code = event
                                .response
                                .as_ref()
                                .and_then(|r| r.get("error"))
                                .and_then(|e| e.get("code"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("unknown");

                            match error_code {
                                "rate_limit_exceeded" => {
                                    bail!("{}", ApiError::RateLimit(error_msg.to_string()));
                                }
                                "context_length_exceeded" => {
                                    bail!("Context length exceeded: {}", error_msg);
                                }
                                "insufficient_quota" => {
                                    bail!("Insufficient quota: {}", error_msg);
                                }
                                _ => {
                                    bail!("Response failed ({}): {}", error_code, error_msg);
                                }
                            }
                        }
                        "response.incomplete" => {
                            let reason = event
                                .response
                                .as_ref()
                                .and_then(|r| r.get("incomplete_details"))
                                .and_then(|d| d.get("reason"))
                                .and_then(|r| r.as_str())
                                .unwrap_or("unknown");
                            bail!("Response incomplete: {}", reason);
                        }
                        _ => {
                            // Unknown event type — ignore (forward compatibility)
                            debug!("Ignoring unknown WS event type: {}", event.event_type);
                        }
                    }
                }

                ReaderFrame::Close(frame) => {
                    let (code, reason) = frame
                        .as_ref()
                        .map(|f| (f.code.into(), f.reason.as_ref()))
                        .unwrap_or((0u16, "no reason"));
                    warn!(
                        "WebSocket closed by server: code={}, reason='{}'",
                        code, reason
                    );
                    bail!(
                        "WebSocket connection closed by server (code={}, reason='{}')",
                        code,
                        reason
                    );
                }

                ReaderFrame::Error(msg) => {
                    bail!("WebSocket connection error: {}", msg);
                }
            }
        }

        Ok((
            LLMResponse {
                content: content_blocks,
                usage,
                rate_limit_info: None,
            },
            response_id,
        ))
    }
}

// ============================================================================
// Free function: output conversion (avoids borrow conflicts in process_ws_stream)
// ============================================================================

/// Convert API response output items to internal ContentBlocks.
fn convert_output_items(output: Vec<WsResponseOutputItem>) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    for item in output {
        match item {
            WsResponseOutputItem::Message { content, .. } => {
                for c in content {
                    match c {
                        WsResponseOutputContent::OutputText { text } => {
                            blocks.push(ContentBlock::Text {
                                text,
                                start_time: None,
                                end_time: None,
                            });
                        }
                    }
                }
            }
            WsResponseOutputItem::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            } => {
                if let Some(enc) = encrypted_content
                    && !enc.is_empty()
                {
                    let summary_items: Vec<ReasoningSummaryItem> = summary
                        .into_iter()
                        .map(|s| match s {
                            WsReasoningSummary::SummaryText { text } => {
                                ReasoningSummaryItem::SummaryText { text }
                            }
                        })
                        .collect();
                    blocks.push(ContentBlock::RedactedThinking {
                        id,
                        summary: summary_items,
                        data: enc,
                        start_time: None,
                        end_time: None,
                    });
                    continue;
                }
                let visible_text: String = content
                    .into_iter()
                    .map(|c| match c {
                        WsReasoningContent::ReasoningText { text } => text,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !visible_text.is_empty() {
                    blocks.push(ContentBlock::Thinking {
                        thinking: visible_text,
                        signature: String::new(),
                        start_time: None,
                        end_time: None,
                    });
                }
            }
            WsResponseOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let input: serde_json::Value =
                    serde_json::from_str(&arguments).unwrap_or(serde_json::Value::Null);
                blocks.push(ContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input,
                    thought_signature: None,
                    start_time: None,
                    end_time: None,
                });
            }
        }
    }
    blocks
}

// ============================================================================
// LLMProvider implementation
// ============================================================================

#[async_trait]
impl LLMProvider for OpenAIResponsesWsClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Retry loop (max 3 attempts) to handle transient WebSocket issues
        let max_retries = 3u32;
        let mut attempts = 0u32;

        loop {
            match self
                .send_ws_request(request.clone(), streaming_callback)
                .await
            {
                Ok(response) => return Ok(response),
                Err(e) => {
                    attempts += 1;
                    let err_msg = e.to_string();
                    let is_connection_error = err_msg.contains("WebSocket connection")
                        || err_msg.contains("connection limit")
                        || err_msg.contains("idle timeout")
                        || err_msg.contains("closed by server")
                        || err_msg.contains("reader task ended")
                        || err_msg.contains("Failed to send WebSocket");

                    if is_connection_error && attempts < max_retries {
                        warn!(
                            "WebSocket error (attempt {}/{}), reconnecting: {}",
                            attempts, max_retries, e
                        );
                        self.drop_connection();
                        // Brief delay before retry
                        tokio::time::sleep(Duration::from_millis(500 * attempts as u64)).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
}

// ============================================================================
// Default WebSocket request customizer
// ============================================================================

/// Default request customizer for WebSocket Responses API.
///
/// Uses `OpenAI-Beta: responses_websockets=2026-02-06` to request
/// WebSocket support from the API.
struct WsDefaultRequestCustomizer;

impl RequestCustomizer for WsDefaultRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![(
            "OpenAI-Beta".to_string(),
            "responses_websockets=2026-02-06".to_string(),
        )]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/responses")
    }
}

// ============================================================================
// Codex auth WebSocket request customizer
// ============================================================================

/// Request customizer for WebSocket Responses API with Codex (ChatGPT) auth.
pub struct CodexWsRequestCustomizer;

impl RequestCustomizer for CodexWsRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        // ChatGPT backend doesn't need the beta header — the WebSocket
        // endpoint is the native transport.
        vec![]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/responses")
    }
}

// ============================================================================
// Helper types (duplicated from openai_responses.rs to stay self-contained)
// ============================================================================

#[derive(Debug, Clone)]
struct FunctionCallInfo {
    name: String,
    call_id: String,
}

#[derive(Debug, Default)]
struct ReasoningState {
    current_item_id: Option<String>,
    current_item_content: String,
    completed_items: Vec<ReasoningSummaryItem>,
    reasoning_block_id: Option<String>,
    received_completed_reasoning: bool,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_url_conversion() {
        let client = OpenAIResponsesWsClient::new(
            "sk-test".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );
        let url = client.ws_url().unwrap();
        assert!(url.starts_with("wss://"));
        assert!(url.contains("api.openai.com/v1/responses"));
    }

    #[test]
    fn test_ws_url_conversion_http() {
        let client = OpenAIResponsesWsClient::new(
            "sk-test".to_string(),
            "gpt-5".to_string(),
            "http://localhost:8080".to_string(),
        );
        let url = client.ws_url().unwrap();
        assert!(url.starts_with("ws://"));
        assert!(url.contains("localhost:8080/responses"));
    }

    #[test]
    fn test_ws_request_serialization() {
        let req = WsRequest::ResponseCreate(WsResponseCreateRequest {
            model: "gpt-5".to_string(),
            instructions: Some("You are helpful.".to_string()),
            previous_response_id: None,
            input: vec![WsInputItem::Message {
                role: "user".to_string(),
                content: vec![WsContentItem::input_text("Hello".to_string())],
            }],
            tools: None,
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
            prompt_cache_key: None,
            text: None,
        });

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "response.create");
        assert_eq!(json["model"], "gpt-5");
        assert_eq!(json["instructions"], "You are helpful.");
        assert_eq!(json["input"][0]["type"], "message");
        assert_eq!(json["input"][0]["role"], "user");
    }

    #[test]
    fn test_ws_request_incremental() {
        let req = WsRequest::ResponseCreate(WsResponseCreateRequest {
            model: "gpt-5".to_string(),
            instructions: None,
            previous_response_id: Some("resp_abc123".to_string()),
            input: vec![WsInputItem::Message {
                role: "user".to_string(),
                content: vec![WsContentItem::input_text("Follow-up".to_string())],
            }],
            tools: None,
            tool_choice: None,
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
            prompt_cache_key: None,
            text: None,
        });

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "response.create");
        assert_eq!(json["previous_response_id"], "resp_abc123");
    }

    #[test]
    fn test_model_capabilities() {
        let gpt5 = ModelCapabilities::for_model("gpt-5");
        assert!(gpt5.supports_reasoning);
        assert!(gpt5.supports_verbosity);

        let o3 = ModelCapabilities::for_model("o3-mini");
        assert!(o3.supports_reasoning);
        assert!(!o3.supports_verbosity);

        let gpt4o = ModelCapabilities::for_model("gpt-4o");
        assert!(!gpt4o.supports_reasoning);
        assert!(!gpt4o.supports_verbosity);
    }

    #[test]
    fn test_delta_computation() {
        let mut client = OpenAIResponsesWsClient::new(
            "sk-test".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        // No previous state => no delta
        let input = vec![WsInputItem::Message {
            role: "user".to_string(),
            content: vec![WsContentItem::input_text("Hello".to_string())],
        }];
        assert!(client.compute_delta(&input).is_none());

        // Set up previous state
        client.last_response_id = Some("resp_1".to_string());
        client.last_input_items = input.clone();

        // Extend with a new message
        let mut extended = input.clone();
        extended.push(WsInputItem::Message {
            role: "assistant".to_string(),
            content: vec![WsContentItem::OutputText {
                text: "Hi!".to_string(),
                phase: Some("final_answer".to_string()),
            }],
        });
        extended.push(WsInputItem::Message {
            role: "user".to_string(),
            content: vec![WsContentItem::input_text("How are you?".to_string())],
        });

        let result = client.compute_delta(&extended);
        assert!(result.is_some());
        let (prev_id, delta) = result.unwrap();
        assert_eq!(prev_id, "resp_1");
        assert_eq!(delta.len(), 2); // assistant + user messages
    }

    #[test]
    fn test_ws_event_parsing() {
        // response.completed event
        let json = r#"{
            "type": "response.completed",
            "response": {
                "id": "resp_test123",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "total_tokens": 150,
                    "input_tokens_details": {
                        "cached_tokens": 80
                    }
                }
            }
        }"#;

        let event: WsStreamEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "response.completed");
        assert!(event.response.is_some());
    }

    #[test]
    fn test_ws_wrapped_error_parsing() {
        let json = r#"{
            "type": "error",
            "status": 429,
            "error": {
                "type": "rate_limit_exceeded",
                "message": "Rate limit reached, try again in 5s"
            },
            "headers": {
                "x-codex-primary-used-percent": "100.0"
            }
        }"#;

        let event: WsStreamEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "error");
        assert_eq!(event.status, Some(429));
        assert!(event.error.is_some());
    }

    #[test]
    fn test_output_conversion() {
        let output = vec![
            WsResponseOutputItem::Message {
                id: Some("msg_1".to_string()),
                role: "assistant".to_string(),
                content: vec![WsResponseOutputContent::OutputText {
                    text: "Hello, world!".to_string(),
                }],
            },
            WsResponseOutputItem::FunctionCall {
                id: Some("fc_1".to_string()),
                call_id: "call_abc".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"city": "Berlin"}"#.to_string(),
            },
        ];

        let blocks = convert_output_items(output);
        assert_eq!(blocks.len(), 2);

        match &blocks[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "Hello, world!"),
            _ => panic!("Expected Text block"),
        }

        match &blocks[1] {
            ContentBlock::ToolUse { name, .. } => assert_eq!(name, "get_weather"),
            _ => panic!("Expected ToolUse block"),
        }
    }

    // -------------------------------------------------------------------------
    // phase field tests (WebSocket)
    // -------------------------------------------------------------------------

    /// A pure-text assistant message (no tool use) should produce an OutputText
    /// content item with `phase = "final_answer"`.
    #[test]
    fn test_ws_output_text_phase_final_answer_for_text_only_message() {
        let client = OpenAIResponsesWsClient::new(
            "sk-test".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message::new_assistant("Hello from assistant")];
        let converted = client.convert_messages_with_cache(messages, false);
        assert_eq!(converted.len(), 1);

        match &converted[0] {
            WsInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    WsContentItem::OutputText { text, phase } => {
                        assert_eq!(text, "Hello from assistant");
                        assert_eq!(phase.as_deref(), Some("final_answer"));
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }
    }

    /// An assistant message that also contains a ToolUse block should produce
    /// OutputText items with `phase = "commentary"`.
    #[test]
    fn test_ws_output_text_phase_commentary_when_tool_use_present() {
        let client = OpenAIResponsesWsClient::new(
            "sk-test".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message::new_assistant_content(vec![
            ContentBlock::new_text("Let me look that up."),
            ContentBlock::new_tool_use("call_1", "search", serde_json::json!({"query": "weather"})),
        ])];

        let converted = client.convert_messages_with_cache(messages, false);
        // Should produce: Message(OutputText), FunctionCall
        assert_eq!(converted.len(), 2);

        match &converted[0] {
            WsInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    WsContentItem::OutputText { text, phase } => {
                        assert_eq!(text, "Let me look that up.");
                        assert_eq!(phase.as_deref(), Some("commentary"));
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }
    }

    /// User messages must never get a `phase` field (it only applies to
    /// assistant OutputText items).
    #[test]
    fn test_ws_output_text_phase_not_set_on_user_messages() {
        let client = OpenAIResponsesWsClient::new(
            "sk-test".to_string(),
            "gpt-5".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages = vec![Message::new_user("Hello")];
        let converted = client.convert_messages_with_cache(messages, false);
        assert_eq!(converted.len(), 1);

        match &converted[0] {
            WsInputItem::Message { content, .. } => match &content[0] {
                WsContentItem::InputText { .. } => { /* correct */ }
                WsContentItem::OutputText { .. } => {
                    panic!("User message should use InputText, not OutputText")
                }
                _ => panic!("Expected InputText"),
            },
            _ => panic!("Expected Message"),
        }
    }

    /// The `phase` field must be serialized correctly.
    #[test]
    fn test_ws_output_text_phase_serialization() {
        let final_item = WsContentItem::OutputText {
            text: "done".to_string(),
            phase: Some("final_answer".to_string()),
        };
        let json = serde_json::to_value(&final_item).unwrap();
        assert_eq!(json["type"], "output_text");
        assert_eq!(json["text"], "done");
        assert_eq!(json["phase"], "final_answer");

        let commentary_item = WsContentItem::OutputText {
            text: "thinking".to_string(),
            phase: Some("commentary".to_string()),
        };
        let json = serde_json::to_value(&commentary_item).unwrap();
        assert_eq!(json["phase"], "commentary");

        let input_item = WsContentItem::input_text("hello".to_string());
        let json = serde_json::to_value(&input_item).unwrap();
        assert!(json.get("phase").is_none());
    }

    fn ws_items_with_breakpoint(items: &[WsInputItem]) -> Vec<usize> {
        items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| match item {
                WsInputItem::Message { content, .. } => content
                    .iter()
                    .any(|c| {
                        matches!(
                            c,
                            WsContentItem::InputText {
                                prompt_cache_breakpoint: Some(_),
                                ..
                            } | WsContentItem::InputImage {
                                prompt_cache_breakpoint: Some(_),
                                ..
                            }
                        )
                    })
                    .then_some(idx),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_explicit_cache_breakpoints_on_user_messages() {
        let client = OpenAIResponsesWsClient::new(
            "test_key".to_string(),
            "gpt-5.6".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        // 10 user messages → markers at message indices 4 and 9 (1:1 items)
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::new_user(format!("Message {i}")))
            .collect();
        let items = client.convert_messages_with_cache(messages, true);
        assert_eq!(ws_items_with_breakpoint(&items), vec![4, 9]);

        // Without explicit caching no markers are placed
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::new_user(format!("Message {i}")))
            .collect();
        let items = client.convert_messages_with_cache(messages, false);
        assert!(ws_items_with_breakpoint(&items).is_empty());
    }

    /// Moving cache markers must invalidate the incremental-input delta so
    /// the full input (with markers at their new positions) is resent.
    #[test]
    fn test_moved_cache_marker_invalidates_delta() {
        let mut client = OpenAIResponsesWsClient::new(
            "test_key".to_string(),
            "gpt-5.6".to_string(),
            "https://api.openai.com/v1".to_string(),
        );

        let messages: Vec<Message> = (0..10)
            .map(|i| Message::new_user(format!("Message {i}")))
            .collect();
        let prev_items = client.convert_messages_with_cache(messages.clone(), true);
        client.last_response_id = Some("resp_1".to_string());
        client.last_input_items = prev_items;

        // Growing to 12 messages keeps markers at 4 and 9 → strict extension
        let mut extended = messages.clone();
        extended.push(Message::new_user("Message 10"));
        extended.push(Message::new_user("Message 11"));
        let items = client.convert_messages_with_cache(extended, true);
        let delta = client.compute_delta(&items);
        assert!(delta.is_some());
        assert_eq!(delta.unwrap().1.len(), 2);

        // Growing to 15 messages moves markers to 9 and 14 → full resend
        let mut extended = messages;
        for i in 10..15 {
            extended.push(Message::new_user(format!("Message {i}")));
        }
        let items = client.convert_messages_with_cache(extended, true);
        assert_eq!(ws_items_with_breakpoint(&items), vec![9, 14]);
        assert!(client.compute_delta(&items).is_none());
    }
}
