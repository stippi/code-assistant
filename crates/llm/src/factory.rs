use crate::auth::TokenManager;
use crate::config::{AiCoreConfig, DeploymentConfig};
use crate::provider_config::{ConfigurationSystem, ModelConfig, ProviderConfig};
use crate::{
    recording::PlaybackState, AiCoreClient, AnthropicClient, CerebrasClient, GroqClient,
    LLMProvider, MistralAiClient, OllamaClient, OpenAIClient, OpenAIResponsesClient,
    OpenRouterClient, VertexClient,
};
use anyhow::{Context, Result};
use clap::ValueEnum;

use std::path::PathBuf;

#[derive(ValueEnum, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum LLMProviderType {
    AiCore,
    Anthropic,
    Cerebras,
    Groq,
    MistralAI,
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
) -> Result<Box<dyn LLMProvider>> {
    let config_system = ConfigurationSystem::load()?;
    let (model_config, provider_config) = config_system.get_model_with_provider(model_name)?;

    create_llm_client_from_configs(model_config, provider_config, playback_path, fast_playback)
        .await
}

/// Create an LLM client from model and provider configurations
pub async fn create_llm_client_from_configs(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    playback_path: Option<PathBuf>,
    fast_playback: bool,
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
        "mistral-ai" => LLMProviderType::MistralAI,
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

    // Extract recording path from model config if present
    let record_path = model_config
        .config
        .get("record_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    match provider_type {
        LLMProviderType::AiCore => {
            create_ai_core_client(model_config, provider_config, record_path).await
        }
        LLMProviderType::Anthropic => {
            create_anthropic_client(model_config, provider_config, record_path, playback_state)
                .await
        }
        LLMProviderType::Cerebras => create_cerebras_client(model_config, provider_config).await,
        LLMProviderType::Groq => create_groq_client(model_config, provider_config).await,
        LLMProviderType::MistralAI => create_mistral_client(model_config, provider_config).await,
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

pub async fn create_llm_client(config: LLMClientConfig) -> Result<Box<dyn LLMProvider>> {
    // Build optional playback state once
    let playback_state = if let Some(path) = &config.playback_path {
        let state = PlaybackState::from_file(path, config.fast_playback)?;
        if state.session_count() == 0 {
            return Err(anyhow::anyhow!("Recording file contains no sessions"));
        }
        Some(state)
    } else {
        None
    };

    // Create normal providers but inject playback/recording where supported
    match config.provider {
        LLMProviderType::AiCore => {
            // Try new config file first, fallback to keyring
            let config_path = config
                .aicore_config
                .unwrap_or_else(AiCoreConfig::get_default_config_path);

            let aicore_config = match AiCoreConfig::load_from_file(&config_path) {
                Ok(config) => config,
                Err(e) => {
                    // Output sample config file when loading fails
                    eprintln!("Failed to load AI Core config from {config_path:?}: {e}");
                    eprintln!("\nPlease create the config file with the following structure:");
                    eprintln!("```json");
                    eprintln!("{{");
                    eprintln!("  \"auth\": {{");
                    eprintln!("    \"client_id\": \"<your service key client id>\",");
                    eprintln!("    \"client_secret\": \"<your service key client secret>\",");
                    eprintln!("    \"token_url\": \"https://<your service key url>/oauth/token\",");
                    eprintln!(
                        "    \"api_base_url\": \"https://<your service key api URL>/v2/inference\""
                    );
                    eprintln!("  }},");
                    eprintln!("  \"models\": {{");
                    eprintln!("    \"claude-sonnet-4\": \"<your deployment id for the model>\"");
                    eprintln!("  }}");
                    eprintln!("}}");
                    eprintln!("```");
                    eprintln!("\nDefault config file location: {config_path:?}");

                    return Err(e.context(format!(
                        "Failed to load AI Core config from {config_path:?}"
                    )));
                }
            };

            // Get matching deployment for given model ID
            let model_name = config
                .model
                .unwrap_or_else(|| "claude-sonnet-4".to_string());
            let deployment_uuid = aicore_config
                .get_deployment_for_model(&model_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No deployment found for model '{}' in config file. Available models: {:?}",
                        model_name,
                        aicore_config.models.keys().collect::<Vec<_>>()
                    )
                })?;

            // Convert AiCoreAuthConfig to DeploymentConfig for TokenManager
            let deployment_config = DeploymentConfig {
                client_id: aicore_config.auth.client_id.clone(),
                client_secret: aicore_config.auth.client_secret.clone(),
                token_url: aicore_config.auth.token_url.clone(),
                api_base_url: aicore_config.auth.api_base_url.clone(),
            };

            let token_manager = TokenManager::new(&deployment_config)
                .await
                .context("Failed to initialize token manager")?;

            // Extend API URL with deployment ID
            let base_api_url = config
                .base_url
                .unwrap_or(aicore_config.auth.api_base_url.clone());
            let api_url = format!(
                "{}/deployments/{}",
                base_api_url.trim_end_matches('/'),
                deployment_uuid
            );

            let client = if let Some(path) = config.record_path {
                AiCoreClient::new_with_recorder(token_manager, api_url, path)
            } else {
                AiCoreClient::new(token_manager, api_url)
            };

            // TODO: add playback integration to AiCoreClient similar to others
            Ok(Box::new(client))
        }

        LLMProviderType::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?;
            let model_name = config
                .model
                .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
            let base_url = config
                .base_url
                .unwrap_or(AnthropicClient::default_base_url());

            let mut client = if let Some(path) = config.record_path {
                AnthropicClient::new_with_recorder(api_key, model_name, base_url, path)
            } else {
                AnthropicClient::new(api_key, model_name, base_url)
            };

            // Inject playback if provided
            if let Some(state) = playback_state {
                client = client.with_playback(state);
            }

            Ok(Box::new(client))
        }

        LLMProviderType::Cerebras => {
            let api_key = std::env::var("CEREBRAS_API_KEY")
                .context("CEREBRAS_API_KEY environment variable not set")?;
            let model_name = config.model.unwrap_or_else(|| "gpt-oss-120b".to_string());
            let base_url = config
                .base_url
                .unwrap_or(CerebrasClient::default_base_url());

            Ok(Box::new(CerebrasClient::new(api_key, model_name, base_url)))
        }

        LLMProviderType::Groq => {
            let api_key = std::env::var("GROQ_API_KEY")
                .context("GROQ_API_KEY environment variable not set")?;
            let model_name = config
                .model
                .unwrap_or_else(|| "moonshotai/kimi-k2-instruct".to_string());
            let base_url = config.base_url.unwrap_or(GroqClient::default_base_url());

            Ok(Box::new(GroqClient::new(api_key, model_name, base_url)))
        }

        LLMProviderType::MistralAI => {
            let api_key = std::env::var("MISTRALAI_API_KEY")
                .context("MISTRALAI_API_KEY environment variable not set")?;
            let model_name = config
                .model
                .unwrap_or_else(|| "devstral-medium-2507".to_string());
            let base_url = config
                .base_url
                .unwrap_or(MistralAiClient::default_base_url());

            Ok(Box::new(MistralAiClient::new(
                api_key, model_name, base_url,
            )))
        }

        LLMProviderType::OpenAI => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY environment variable not set")?;
            let model_name = config.model.unwrap_or_else(|| "gpt-4.1".to_string());
            let base_url = config.base_url.unwrap_or(OpenAIClient::default_base_url());

            Ok(Box::new(OpenAIClient::new(api_key, model_name, base_url)))
        }

        LLMProviderType::OpenAIResponses => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY environment variable not set")?;
            let model_name = config.model.unwrap_or_else(|| "gpt-5".to_string());
            let base_url = config
                .base_url
                .unwrap_or(OpenAIResponsesClient::default_base_url());

            let mut client = OpenAIResponsesClient::new(api_key, model_name, base_url);

            // Inject playback state into OpenAI Responses provider (implemented below)
            if let Some(state) = playback_state {
                client = client.with_playback(state);
            }
            // Attach recorder if requested (implemented below)
            if let Some(path) = config.record_path {
                client = client.with_recorder(path);
            }

            Ok(Box::new(client))
        }

        LLMProviderType::Vertex => {
            let api_key = std::env::var("GOOGLE_API_KEY")
                .context("GOOGLE_API_KEY environment variable not set")?;
            let model_name = config
                .model
                .unwrap_or_else(|| "gemini-2.5-pro-preview-06-05".to_string());
            let base_url = config.base_url.unwrap_or(VertexClient::default_base_url());

            if let Some(path) = config.record_path {
                Ok(Box::new(VertexClient::new_with_recorder(
                    api_key, model_name, base_url, path,
                )))
            } else {
                Ok(Box::new(VertexClient::new(api_key, model_name, base_url)))
            }
        }

        LLMProviderType::Ollama => {
            let base_url = config.base_url.unwrap_or(OllamaClient::default_base_url());

            Ok(Box::new(OllamaClient::new(
                config
                    .model
                    .context("Model name is required for Ollama provider")?,
                base_url,
                config.num_ctx,
            )))
        }

        LLMProviderType::OpenRouter => {
            let api_key = std::env::var("OPENROUTER_API_KEY")
                .context("OPENROUTER_API_KEY environment variable not set")?;
            let model = config
                .model
                .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());
            let base_url = config
                .base_url
                .unwrap_or(OpenRouterClient::default_base_url());

            Ok(Box::new(OpenRouterClient::new(api_key, model, base_url)))
        }
    }
}
// Helper functions for creating clients with the new configuration system

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

    let deployment_uuid = models
        .get(&model_config.id)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No deployment found for model '{}' in AI Core config",
                model_config.id
            )
        })?;

    let deployment_config = DeploymentConfig {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        token_url: token_url.to_string(),
        api_base_url: api_base_url.to_string(),
    };

    let token_manager = TokenManager::new(&deployment_config)
        .await
        .context("Failed to initialize token manager")?;

    let api_url = format!(
        "{}/deployments/{}",
        api_base_url.trim_end_matches('/'),
        deployment_uuid
    );

    let client = if let Some(path) = record_path {
        AiCoreClient::new_with_recorder(token_manager, api_url, path)
    } else {
        AiCoreClient::new(token_manager, api_url)
    };

    Ok(Box::new(client))
}

async fn create_anthropic_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    record_path: Option<PathBuf>,
    playback_state: Option<PlaybackState>,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in Anthropic provider config"))?;

    let default_base_url = AnthropicClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    let mut client = if let Some(path) = record_path {
        AnthropicClient::new_with_recorder(
            api_key.to_string(),
            model_config.id.clone(),
            base_url.to_string(),
            path,
        )
    } else {
        AnthropicClient::new(
            api_key.to_string(),
            model_config.id.clone(),
            base_url.to_string(),
        )
    };

    if let Some(state) = playback_state {
        client = client.with_playback(state);
    }

    Ok(Box::new(client))
}

async fn create_cerebras_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in Cerebras provider config"))?;

    let default_base_url = CerebrasClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    Ok(Box::new(CerebrasClient::new(
        api_key.to_string(),
        model_config.id.clone(),
        base_url.to_string(),
    )))
}

async fn create_groq_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in Groq provider config"))?;

    let default_base_url = GroqClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    Ok(Box::new(GroqClient::new(
        api_key.to_string(),
        model_config.id.clone(),
        base_url.to_string(),
    )))
}

async fn create_mistral_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in Mistral provider config"))?;

    let default_base_url = MistralAiClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    Ok(Box::new(MistralAiClient::new(
        api_key.to_string(),
        model_config.id.clone(),
        base_url.to_string(),
    )))
}

async fn create_openai_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in OpenAI provider config"))?;

    let default_base_url = OpenAIClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    Ok(Box::new(OpenAIClient::new(
        api_key.to_string(),
        model_config.id.clone(),
        base_url.to_string(),
    )))
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

    if let Some(path) = record_path {
        Ok(Box::new(VertexClient::new_with_recorder(
            api_key.to_string(),
            model_config.id.clone(),
            base_url.to_string(),
            path,
        )))
    } else {
        Ok(Box::new(VertexClient::new(
            api_key.to_string(),
            model_config.id.clone(),
            base_url.to_string(),
        )))
    }
}

async fn create_ollama_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let default_base_url = OllamaClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    let num_ctx = model_config
        .config
        .get("num_ctx")
        .and_then(|v| v.as_u64())
        .unwrap_or(8192) as usize;

    Ok(Box::new(OllamaClient::new(
        model_config.id.clone(),
        base_url.to_string(),
        num_ctx,
    )))
}

async fn create_openrouter_client(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> Result<Box<dyn LLMProvider>> {
    let config = &provider_config.config;

    let api_key = config
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("api_key not found in OpenRouter provider config"))?;

    let default_base_url = OpenRouterClient::default_base_url();
    let base_url = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_base_url);

    Ok(Box::new(OpenRouterClient::new(
        api_key.to_string(),
        model_config.id.clone(),
        base_url.to_string(),
    )))
}
