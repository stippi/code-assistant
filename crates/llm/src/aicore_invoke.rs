use crate::{
    anthropic::{AnthropicClient, AuthProvider, MessageConverter, RequestCustomizer},
    types::*,
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use super::auth::TokenManager;

/// AiCore authentication provider using TokenManager
pub struct AiCoreAuthProvider {
    token_manager: Arc<TokenManager>,
}

impl AiCoreAuthProvider {
    pub fn new(token_manager: Arc<TokenManager>) -> Self {
        Self { token_manager }
    }
}

#[async_trait]
impl AuthProvider for AiCoreAuthProvider {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        let token = self.token_manager.get_valid_token().await?;
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {}", token),
        )])
    }
}

/// AiCore request customizer
pub struct AiCoreRequestCustomizer;

impl RequestCustomizer for AiCoreRequestCustomizer {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()> {
        if let Value::Object(ref mut map) = request {
            // Remove stream and model fields after URL routing is done
            map.remove("stream");
            map.remove("model");
            // Add anthropic_version for AiCore
            map.insert(
                "anthropic_version".to_string(),
                Value::String("bedrock-2023-05-31".to_string()),
            );
        }
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![
            ("AI-Resource-Group".to_string(), "default".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "anthropic-beta".to_string(),
                "output-128k-2025-02-19".to_string(),
            ),
        ]
    }

    fn customize_url(&self, base_url: &str, streaming: bool) -> String {
        if streaming {
            format!("{}/invoke-with-response-stream", base_url)
        } else {
            format!("{}/invoke", base_url)
        }
    }
}

/// AiCore message converter - converts messages to AiCore format without caching
pub struct AiCoreMessageConverter;

/// AiCore message structure
#[derive(Debug, Serialize)]
struct AiCoreMessage {
    role: String,
    content: Vec<AiCoreContentBlock>,
}

/// AiCore content block structure
#[derive(Debug, Serialize)]
struct AiCoreContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(flatten)]
    content: AiCoreBlockContent,
}

/// Content variants for AiCore content blocks
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AiCoreBlockContent {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<AiCoreToolResultContent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Tool result content for AiCore
#[derive(Debug, Serialize)]
struct AiCoreToolResultContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(flatten)]
    data: AiCoreToolResultData,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AiCoreToolResultData {
    Text { text: String },
}

impl MessageConverter for AiCoreMessageConverter {
    fn convert_messages(&mut self, messages: Vec<Message>) -> Result<Vec<serde_json::Value>> {
        let aicore_messages = self.convert_messages_to_aicore(messages);
        Ok(vec![serde_json::to_value(aicore_messages)?])
    }
}

impl AiCoreMessageConverter {
    pub fn new() -> Self {
        Self
    }

    /// Convert LLM messages to AiCore format (removes internal fields like request_id)
    fn convert_messages_to_aicore(&self, messages: Vec<Message>) -> Vec<AiCoreMessage> {
        messages
            .into_iter()
            .map(|msg| AiCoreMessage {
                role: match msg.role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                },
                content: self.convert_content_to_aicore(msg.content),
            })
            .collect()
    }

    /// Convert LLM message content to AiCore format
    fn convert_content_to_aicore(&self, content: MessageContent) -> Vec<AiCoreContentBlock> {
        match content {
            MessageContent::Text(text) => {
                vec![AiCoreContentBlock {
                    block_type: "text".to_string(),
                    content: AiCoreBlockContent::Text { text },
                }]
            }
            MessageContent::Structured(blocks) => blocks
                .into_iter()
                .map(|block| self.convert_content_block_to_aicore(block))
                .collect(),
        }
    }

    /// Convert a single content block to AiCore format
    fn convert_content_block_to_aicore(&self, block: ContentBlock) -> AiCoreContentBlock {
        match block {
            ContentBlock::Text { text } => AiCoreContentBlock {
                block_type: "text".to_string(),
                content: AiCoreBlockContent::Text { text },
            },
            ContentBlock::ToolUse { id, name, input } => AiCoreContentBlock {
                block_type: "tool_use".to_string(),
                content: AiCoreBlockContent::ToolUse { id, name, input },
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let tool_content = Some(vec![AiCoreToolResultContent {
                    content_type: "text".to_string(),
                    data: AiCoreToolResultData::Text { text: content },
                }]);
                AiCoreContentBlock {
                    block_type: "tool_result".to_string(),
                    content: AiCoreBlockContent::ToolResult {
                        tool_use_id,
                        content: tool_content,
                        is_error,
                    },
                }
            }
            ContentBlock::Thinking {
                thinking,
                signature,
            } => AiCoreContentBlock {
                block_type: "thinking".to_string(),
                content: AiCoreBlockContent::Thinking {
                    thinking,
                    signature,
                },
            },
            ContentBlock::RedactedThinking { data } => AiCoreContentBlock {
                block_type: "redacted_thinking".to_string(),
                content: AiCoreBlockContent::RedactedThinking { data },
            },
        }
    }
}

pub struct AiCoreClient {
    anthropic_client: AnthropicClient,
}

impl AiCoreClient {
    fn create_anthropic_client(
        token_manager: Arc<TokenManager>,
        base_url: String,
    ) -> AnthropicClient {
        let auth_provider = Box::new(AiCoreAuthProvider::new(token_manager));
        let request_customizer = Box::new(AiCoreRequestCustomizer);
        let message_converter = Box::new(AiCoreMessageConverter::new());

        AnthropicClient::with_customization(
            "ignored".to_string(), // Default model, can be overridden
            base_url,
            auth_provider,
            request_customizer,
            message_converter,
        )
    }

    pub fn new(token_manager: Arc<TokenManager>, base_url: String) -> Self {
        let anthropic_client = Self::create_anthropic_client(token_manager, base_url);
        Self { anthropic_client }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        token_manager: Arc<TokenManager>,
        base_url: String,
        recording_path: P,
    ) -> Self {
        let anthropic_client =
            Self::create_anthropic_client(token_manager, base_url).with_recorder(recording_path);

        Self { anthropic_client }
    }
}

#[async_trait]
impl LLMProvider for AiCoreClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to the wrapped AnthropicClient
        self.anthropic_client
            .send_message(request, streaming_callback)
            .await
    }
}
