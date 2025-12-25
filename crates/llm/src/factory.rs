use crate::aicore::{AiCoreAnthropicClient, AiCoreApiType, AiCoreOpenAIClient, AiCoreVertexClient};
use crate::auth::TokenManager;
use crate::provider_config::{ConfigurationSystem, ModelConfig, ProviderConfig};
use crate::{
    recording::PlaybackState, AnthropicClient, CerebrasClient, GroqClient, LLMProvider,
    MinimaxClient, MistralAiClient, MoonshotClient, OllamaClient, OpenAIClient,
    OpenAIResponsesClient, OpenRouterClient, VertexClient, ZaiClient,
};
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde_json::Value;
use std::path::PathBuf;

// ============================================================================
// Helper Functions for Factory
// ============================================================================

/// Trait for providers that support custom configuration
trait WithCustomConfig: Sized {
    fn with_custom_config(self, custom_config: Value) -> Self;
}

/// Apply custom model configuration to a client if present
fn apply_custom_config<T: WithCustomConfig>(client: T, model_config: &ModelConfig) -> T {
    if !model_config.config.is_null()
        && model_config
            .config
            .as_object()
            .is_some_and(|o| !o.is_empty())
    {
        client.with_custom_config(model_config.config.clone())
    } else {
        client
    }
}

/// Extract API key from provider config
fn get_api_key(config: &Value, provider_name: &str) -> Result<String> {
    config
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in {provider_name} provider config"))
}

/// Extract base URL from provider config with default fallback
fn get_base_url(config: &Value, default_url: &str) -> String {
    config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(default_url)
        .to_string()
}

// Implement WithCustomConfig trait for all providers that support it
impl WithCustomConfig for AnthropicClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for OpenAIClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for CerebrasClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for GroqClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for OpenRouterClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for OllamaClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for MinimaxClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for MistralAiClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for MoonshotClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for ZaiClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for OpenAIResponsesClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for VertexClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for AiCoreAnthropicClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for AiCoreOpenAIClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

impl WithCustomConfig for AiCoreVertexClient {
    fn with_custom_config(self, custom_config: Value) -> Self {
        self.with_custom_config(custom_config)
    }
}

// ============================================================================
// Macro for Simple Provider Factory Functions
// ============================================================================

/// Macro to generate factory functions for providers with standard api_key + base_url pattern
macro_rules! simple_provider_factory {
    ($func_name:ident, $client_type:ty, $provider_name:expr) => {
        async fn $func_name(
            model_config: &ModelConfig,
            provider_config: &ProviderConfig,
        ) -> Result<Box<dyn LLMProvider>> {
            let api_key = get_api_key(&provider_config.config, $provider_name)?;
            let base_url =
                get_base_url(&provider_config.config, &<$client_type>::default_base_url());

            let client = <$client_type>::new(api_key, model_config.id.clone(), base_url);
            let client = apply_custom_config(client, model_config);
            Ok(Box::new(client))
        }
    };
}

// Use the macro to generate factory functions for simple providers
simple_provider_factory!(create_cerebras_client, CerebrasClient, "Cerebras");
simple_provider_factory!(create_groq_client, GroqClient, "Groq");
simple_provider_factory!(create_minimax_client, MinimaxClient, "Minimax");
simple_provider_factory!(create_mistral_client, MistralAiClient, "MistralAI");
simple_provider_factory!(create_moonshot_client, MoonshotClient, "Moonshot");
simple_provider_factory!(create_zai_client, ZaiClient, "Z.ai");
simple_provider_factory!(create_openai_client, OpenAIClient, "OpenAI");
simple_provider_factory!(create_openrouter_client, OpenRouterClient, "OpenRouter");

// ============================================================================
// Provider Types and Configuration
// ============================================================================

#[derive(ValueEnum, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum LLMProviderType {
    AiCore,
    Anthropic,
    Cerebras,
    Groq,
    Minimax,
    MistralAI,
    Moonshot,
    Zai,
    Ollama,
    OpenAI,
    OpenAIResponses,
    OpenRouter,
    Vertex,
}

/// Configuration for creating an LLM client
#[derive(Debug, Clone)]
pub struct LLMClientConfig {
    pub provider: LLMProviderType,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub aicore_config: Option<PathBuf>,
    pub num_ctx: usize,
    pub record_path: Option<PathBuf>,
    pub playback_path: Option<PathBuf>,
    pub fast_playback: bool,
}

/// Create an LLM client using the new model-based configuration system
pub async fn create_llm_client_from_model(
    model_name: &str,
    playback_path: Option<PathBuf>,
    fast_playback: bool,
    record_path: Option<PathBuf>,
) -> Result<Box<dyn LLMProvider>> {
    let config_system = ConfigurationSystem::load()?;
    let (model_config, provider_config) = config_system.get_model_with_provider(model_name)?;

    create_llm_client_from_configs(
        model_config,
        provider_config,
        playback_path,
        fast_playback,
        record_path,
    )
    .await
}

/// Create an LLM client from model and provider configurations
pub async fn create_llm_client_from_configs(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    playback_path: Option<PathBuf>,
    fast_playback: bool,
    record_path_override: Option<PathBuf>,
) -> Result<Box<dyn LLMProvider>> {
    // Build optional playback state once
    let playback_state = if let Some(path) = &playback_path {
        let state = PlaybackState::from_file(path, fast_playback)?;
        if state.session_count() == 0 {
            return Err(anyhow::anyhow!("Recording file contains no sessions"));
        }
        Some(state)
    } else {
        None
    };

    // Parse provider type
    let provider_type = match provider_config.provider.as_str() {
        "ai-core" => LLMProviderType::AiCore,
        "anthropic" => LLMProviderType::Anthropic,
        "cerebras" => LLMProviderType::Cerebras,
        "groq" => LLMProviderType::Groq,
        "minimax" => LLMProviderType::Minimax,
        "mistral-ai" => LLMProviderType::MistralAI,
        "moonshot" => LLMProviderType::Moonshot,
        "z-ai" => LLMProviderType::Zai,
        "ollama" => LLMProviderType::Ollama,
        "openai" => LLMProviderType::OpenAI,
        "openai-responses" => LLMProviderType::OpenAIResponses,
        "openrouter" => LLMProviderType::OpenRouter,
        "vertex" => LLMProviderType::Vertex,
        _ => {
            return Err(anyhow::anyhow!(
                "Unknown provider type: {}",
                provider_config.provider
            ))
        }
    };

    // Extract recording path from model config (allowing runtime override)
    let record_path = record_path_override.or_else(|| {
        model_config
            .config
            .get("record_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    });

    match provider_type {
        LLMProviderType::AiCore => {
            create_ai_core_client(model_config, provider_config, record_path.clone()).await
        }
        LLMProviderType::Anthropic => {
            create_anthropic_client(model_config, provider_config, record_path, playback_state)
                .await
        }
        LLMProviderType::Cerebras => create_cerebras_client(model_config, provider_config).await,
        LLMProviderType::Groq => create_groq_client(model_config, provider_config).await,
        LLMProviderType::Minimax => create_minimax_client(model_config, provider_config).await,
        LLMProviderType::MistralAI => create_mistral_client(model_config, provider_config).await,
        LLMProviderType::Moonshot => create_moonshot_client(model_config, provider_config).await,
        LLMProviderType::Zai => create_zai_client(model_config, provider_config).await,
        LLMProviderType::OpenAI => create_openai_client(model_config, provider_config).await,
        LLMProviderType::OpenAIResponses => {
            create_openai_responses_client(
                model_config,
                provider_config,
                playback_state,
                record_path,
            )
            .await
        }
        LLMProviderType::Vertex => {
            create_vertex_client(model_config, provider_config, record_path).await
        }
        LLMProviderType::Ollama => create_ollama_client(model_config, provider_config).await,
        LLMProviderType::OpenRouter => {
            create_openrouter_client(model_config, provider_config).await
        }
    }
}

/// AI Core model deployment configuration
///
/// The `models` field in the AI Core provider config can be specified in two formats:
///
/// 1. Simple format (backwards compatible) - just the deployment UUID:
///    ```json
///    "models": {
///      "claude-3.5-sonnet": "deployment-uuid-here"
///    }
///    ```
///    This defaults to Anthropic API type.
///
/// 2. Extended format - object with `deployment` and `api_type`:
///    ```json
///    "models": {
///      "claude-3.5-sonnet": {
///        "deployment": "deployment-uuid-here",
///        "api_type": "anthropic"
///      },
///      "gpt-4o": {
///        "deployment": "another-deployment-uuid",
///        "api_type": "openai"
///      },
///      "gemini-pro": {
///        "deployment": "vertex-deployment-uuid",
///        "api_type": "vertex"
///      }
///    }
///    ```
#[derive(Debug)]
struct AiCoreDeployment {
    deployment_uuid: String,
    api_type: AiCoreApiType,
}

fn parse_aicore_deployment(model_id: &str, value: &Value) -> Result<AiCoreDeployment> {
    // Try simple string format first (backwards compatible)
    if let Some(uuid) = value.as_str() {
        return Ok(AiCoreDeployment {
            deployment_uuid: uuid.to_string(),
            api_type: AiCoreApiType::default(), // Defaults to Anthropic
        });
    }

    // Try extended object format
    if let Some(obj) = value.as_object() {
        let deployment_uuid = obj
            .get("deployment")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing 'deployment' field in AI Core config for model '{}'",
                    model_id
                )
            })?
            .to_string();

        let api_type = if let Some(api_type_str) = obj.get("api_type").and_then(|v| v.as_str()) {
            api_type_str.parse().with_context(|| {
                format!(
                    "Invalid api_type for model '{}'. Valid values are: anthropic, openai, vertex",
                    model_id
                )
            })?
        } else {
            AiCoreApiType::default()
        };

        return Ok(AiCoreDeployment {
            deployment_uuid,
            api_type,
        });
    }

    Err(anyhow::anyhow!(
        "Invalid deployment configuration for model '{}'. Expected string (deployment UUID) or object with 'deployment' and optional 'api_type' fields",
        model_id
    ))
}

async fn create_ai_core_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    record_path: Option<PathBuf>,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let client_id = config
        .get("client_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("client_id not found in AI Core provider config"))?;

    let client_secret = config
        .get("client_secret")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("client_secret not found in AI Core provider config"))?;

    let token_url = config
        .get("token_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("token_url not found in AI Core provider config"))?;

    let api_base_url = config
        .get("api_base_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_base_url not found in AI Core provider config"))?;

    let models = config
        .get("models")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("models not found in AI Core provider config"))?;

    let deployment_value = models.get(&model_config.id).ok_or_else(|| {
        anyhow::anyhow!(
            "No deployment found for model '{}' in AI Core config",
            model_config.id
        )
    })?;

    let deployment = parse_aicore_deployment(&model_config.id, deployment_value)?;

    let token_manager = TokenManager::new(
        client_id.to_string(),
        client_secret.to_string(),
        token_url.to_string(),
    )
    .await
    .context("Failed to initialize token manager")?;

    let api_url = format!(
        "{}/deployments/{}",
        api_base_url.trim_end_matches('/'),
        deployment.deployment_uuid
    );

    // Create the appropriate client based on API type
    match deployment.api_type {
        AiCoreApiType::Anthropic => {
            let client = if let Some(path) = record_path {
                AiCoreAnthropicClient::new_with_recorder(token_manager, api_url, path)
            } else {
                AiCoreAnthropicClient::new(token_manager, api_url)
            };
            let client = apply_custom_config(client, model_config);
            Ok(Box::new(client))
        }
        AiCoreApiType::OpenAI => {
            let client = if let Some(path) = record_path {
                AiCoreOpenAIClient::new_with_recorder(
                    token_manager,
                    api_url,
                    model_config.id.clone(),
                    path,
                )
            } else {
                AiCoreOpenAIClient::new(token_manager, api_url, model_config.id.clone())
            };
            let client = apply_custom_config(client, model_config);
            Ok(Box::new(client))
        }
        AiCoreApiType::Vertex => {
            let client = if let Some(path) = record_path {
                AiCoreVertexClient::new_with_recorder(
                    token_manager,
                    api_url,
                    model_config.id.clone(),
                    path,
                )
            } else {
                AiCoreVertexClient::new(token_manager, api_url, model_config.id.clone())
            };
            let client = apply_custom_config(client, model_config);
            Ok(Box::new(client))
        }
    }
}

async fn create_anthropic_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    record_path: Option<PathBuf>,
    playback_state: Option<PlaybackState>,
) -> Result<Box<dyn LLMProvider>> {
    let api_key = get_api_key(&provider_config.config, "Anthropic")?;
    let base_url = get_base_url(
        &provider_config.config,
        &AnthropicClient::default_base_url(),
    );

    let mut client = if let Some(path) = record_path {
        AnthropicClient::new_with_recorder(api_key, model_config.id.clone(), base_url, path)
    } else {
        AnthropicClient::new(api_key, model_config.id.clone(), base_url)
    };

    if let Some(state) = playback_state {
        client = client.with_playback(state);
    }

    let client = apply_custom_config(client, model_config);
    Ok(Box::new(client))
}

async fn create_openai_responses_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    playback_state: Option<PlaybackState>,
    record_path: Option<PathBuf>,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in OpenAI provider config"))?;

    let default_base_url = OpenAIResponsesClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    let mut client = OpenAIResponsesClient::new(
        api_key.to_string(),
        model_config.id.clone(),
        base_url.to_string(),
    );

    if let Some(state) = playback_state {
        client = client.with_playback(state);
    }

    if let Some(path) = record_path {
        client = client.with_recorder(path);
    }

    let client = apply_custom_config(client, model_config);
    Ok(Box::new(client))
}

async fn create_vertex_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    record_path: Option<PathBuf>,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in Vertex provider config"))?;

    let default_base_url = VertexClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    let client = if let Some(path) = record_path {
        VertexClient::new_with_recorder(
            api_key.to_string(),
            model_config.id.clone(),
            base_url.to_string(),
            path,
        )
    } else {
        VertexClient::new(
            api_key.to_string(),
            model_config.id.clone(),
            base_url.to_string(),
        )
    };

    let client = apply_custom_config(client, model_config);
    Ok(Box::new(client))
}

async fn create_ollama_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let base_url = get_base_url(&provider_config.config, &OllamaClient::default_base_url());

    let client = OllamaClient::new(model_config.id.clone(), base_url);
    let client = apply_custom_config(client, model_config);
    Ok(Box::new(client))
}
