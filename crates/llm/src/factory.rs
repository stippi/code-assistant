use crate::auth::TokenManager;
use crate::config::{AiCoreConfig, DeploymentConfig};
use crate::{
    AiCoreClient, AnthropicClient, GroqClient, LLMProvider, MistralAiClient, OllamaClient,
    OpenAIClient, OpenRouterClient, VertexClient,
};
use anyhow::{Context, Result};
use clap::ValueEnum;
use std::path::PathBuf;

#[derive(ValueEnum, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum LLMProviderType {
    AiCore,
    Anthropic,
    Groq,
    MistralAI,
    Ollama,
    OpenAI,
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

pub async fn create_llm_client(config: LLMClientConfig) -> Result<Box<dyn LLMProvider>> {
    // If playback is specified, use the recording player regardless of provider
    if let Some(path) = config.playback_path {
        use crate::anthropic_playback::RecordingPlayer;
        let player = RecordingPlayer::from_file(path)?;

        if player.session_count() == 0 {
            return Err(anyhow::anyhow!("Recording file contains no sessions"));
        }

        let mut provider = player.create_provider()?;

        // Configure timing simulation based on command line flag
        if config.fast_playback {
            provider.set_simulate_timing(false);
        }

        return Ok(Box::new(provider));
    }

    // Otherwise continue with normal provider setup
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

            if let Some(path) = config.record_path {
                Ok(Box::new(AiCoreClient::new_with_recorder(
                    token_manager,
                    api_url,
                    path,
                )))
            } else {
                Ok(Box::new(AiCoreClient::new(token_manager, api_url)))
            }
        }

        LLMProviderType::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?;
            let model_name = config
                .model
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());
            let base_url = config
                .base_url
                .unwrap_or(AnthropicClient::default_base_url());

            if let Some(path) = config.record_path {
                Ok(Box::new(AnthropicClient::new_with_recorder(
                    api_key, model_name, base_url, path,
                )))
            } else {
                Ok(Box::new(AnthropicClient::new(
                    api_key, model_name, base_url,
                )))
            }
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
