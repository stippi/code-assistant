//! Asking a provider which models it offers.
//!
//! Each provider API lists its models differently; [`list_models`] picks the
//! endpoint for a configured provider and normalizes the answer into
//! [`DiscoveredModel`]s. What a listing reveals beyond the ID (context
//! window, output limit, reasoning) depends on the API; the
//! [`crate::capabilities`] catalog fills in the rest.
//!
//! Results are kept in [`DiscoveryCache`] so the settings and the model
//! picker never wait on the network.

mod cache;
mod parse;

pub use cache::{DiscoveryCache, ProviderDiscovery};

use crate::capabilities::ReasoningSpec;
use crate::provider_config::ProviderConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A model a provider offers, as far as its listing tells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningSpec>,
}

impl DiscoveredModel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_window: None,
            max_output: None,
            reasoning: None,
        }
    }
}

/// List the models a configured provider offers.
pub async fn list_models(provider: &ProviderConfig) -> Result<Vec<DiscoveredModel>> {
    let config = &provider.config;
    let client = crate::utils::http_client();
    let mut models = match provider.provider.as_str() {
        "anthropic" | "minimax" => {
            let default_url = match provider.provider.as_str() {
                "minimax" => crate::MinimaxClient::default_base_url(),
                _ => crate::AnthropicClient::default_base_url(),
            };
            let base_url = base_url(config, &default_url);
            let api_key = api_key(config)?;
            let mut models = Vec::new();
            let mut after_id: Option<String> = None;
            loop {
                let mut request = client
                    .get(format!("{base_url}/models"))
                    .query(&[("limit", "1000")])
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01");
                if let Some(after) = &after_id {
                    request = request.query(&[("after_id", after)]);
                }
                let page = get_json(request).await?;
                models.extend(parse::anthropic(&page));
                match (page["has_more"].as_bool(), page["last_id"].as_str()) {
                    (Some(true), Some(last)) => after_id = Some(last.to_string()),
                    _ => break,
                }
            }
            models
        }
        "openai"
        | "openai-responses"
        | "openai-responses-ws"
        | "cerebras"
        | "groq"
        | "mistral-ai"
        | "moonshot"
        | "z-ai"
        | "openrouter" => {
            if config.get("codex_auth").is_some() {
                bail!("This provider does not list its models; add model IDs by hand");
            }
            let default_url = default_openai_compatible_url(&provider.provider);
            let base_url = base_url(config, &default_url);
            let request = client
                .get(format!("{base_url}/models"))
                .bearer_auth(api_key(config)?);
            parse::openai_compatible(&get_json(request).await?)
        }
        "vertex" => {
            let base_url = base_url(config, &crate::VertexClient::default_base_url());
            let api_key = api_key(config)?;
            let bearer = config.get("bearer_auth").and_then(Value::as_bool) == Some(true);
            let mut models = Vec::new();
            let mut page_token: Option<String> = None;
            loop {
                let mut request = client
                    .get(format!("{base_url}/models"))
                    .query(&[("pageSize", "1000")]);
                request = if bearer {
                    request.bearer_auth(&api_key)
                } else {
                    request.query(&[("key", &api_key)])
                };
                if let Some(token) = &page_token {
                    request = request.query(&[("pageToken", token)]);
                }
                let page = get_json(request).await?;
                models.extend(parse::gemini(&page));
                match page["nextPageToken"].as_str() {
                    Some(token) if !token.is_empty() => page_token = Some(token.to_string()),
                    _ => break,
                }
            }
            models
        }
        "ollama" => {
            let base_url = base_url(config, &crate::OllamaClient::default_base_url());
            let tags = get_json(client.get(format!("{base_url}/api/tags"))).await?;
            let mut models = Vec::new();
            for mut model in parse::ollama_tags(&tags) {
                let show = client
                    .post(format!("{base_url}/api/show"))
                    .json(&serde_json::json!({ "model": model.id }));
                if let Ok(details) = get_json(show).await {
                    parse::ollama_show(&details, &mut model);
                }
                models.push(model);
            }
            models
        }
        // AI Core serves the deployments configured on the provider.
        "ai-core" => config
            .get("models")
            .and_then(Value::as_object)
            .map(|deployments| deployments.keys().map(DiscoveredModel::new).collect())
            .unwrap_or_default(),
        other => bail!("Unknown provider type: {other}"),
    };
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

fn api_key(config: &Value) -> Result<String> {
    config
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("api_key missing in the provider config"))
}

fn base_url(config: &Value, default: &str) -> String {
    config
        .get("base_url")
        .and_then(Value::as_str)
        .unwrap_or(default)
        .trim_end_matches('/')
        .to_string()
}

fn default_openai_compatible_url(provider_type: &str) -> String {
    match provider_type {
        "cerebras" => crate::CerebrasClient::default_base_url(),
        "groq" => crate::GroqClient::default_base_url(),
        "mistral-ai" => crate::MistralAiClient::default_base_url(),
        "moonshot" => crate::MoonshotClient::default_base_url(),
        "z-ai" => crate::ZaiClient::default_base_url(),
        "openrouter" => crate::OpenRouterClient::default_base_url(),
        _ => crate::OpenAIClient::default_base_url(),
    }
}

async fn get_json(request: reqwest::RequestBuilder) -> Result<Value> {
    let response = request
        .send()
        .await
        .context("model listing request failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body: String = body.chars().take(300).collect();
        bail!("model listing returned {status}: {body}");
    }
    response
        .json()
        .await
        .context("model listing returned invalid JSON")
}
