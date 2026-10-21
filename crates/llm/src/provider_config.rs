use crate::capabilities::{Effort, ModelCapabilities};
use crate::discovery::DiscoveryCache;
use crate::model_selection::ModelSelection;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::info;

/// Process-wide configuration directory override, set once at startup by an
/// embedding application (see [`ConfigurationSystem::set_config_dir`]).
static CONFIG_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Configuration for a single provider instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Human-readable label for this provider configuration
    pub label: String,
    /// Provider type (maps to LLMProviderType)
    pub provider: String,
    /// Provider-specific configuration
    pub config: serde_json::Value,
}

/// Configuration for all providers (provider_id -> ProviderConfig)
pub type ProvidersConfig = HashMap<String, ProviderConfig>;

/// The provider API/client family used to serialize conversation history.
///
/// A session can safely switch between models only when both models use the
/// same API client family. Different families use different representations for
/// tool calls and encrypted/redacted reasoning blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmApiClientKind {
    Anthropic,
    OpenAi,
    OpenAiResponses,
    OpenAiResponsesWs,
    Vertex,
    Ollama,
    OpenRouter,
    Cerebras,
    Groq,
    Minimax,
    MistralAi,
    Moonshot,
    Zai,
}

impl std::fmt::Display for LlmApiClientKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic => write!(f, "Anthropic API"),
            Self::OpenAi => write!(f, "OpenAI Chat Completions API"),
            Self::OpenAiResponses => write!(f, "OpenAI Responses API"),
            Self::OpenAiResponsesWs => write!(f, "OpenAI Responses WebSocket API"),
            Self::Vertex => write!(f, "Vertex API"),
            Self::Ollama => write!(f, "Ollama API"),
            Self::OpenRouter => write!(f, "OpenRouter API"),
            Self::Cerebras => write!(f, "Cerebras API"),
            Self::Groq => write!(f, "Groq API"),
            Self::Minimax => write!(f, "Minimax API"),
            Self::MistralAi => write!(f, "Mistral AI API"),
            Self::Moonshot => write!(f, "Moonshot API"),
            Self::Zai => write!(f, "Z.ai API"),
        }
    }
}

/// Format used by a model for the file-editing tool.
///
/// Some models (notably GPT-5.x) occasionally produce malformed tool-call
/// arguments where junk text leaks into a string parameter. The "simple"
/// `edit` tool takes raw `old_text`/`new_text` strings, so corrupted output
/// gets written verbatim to disk. The "diff" form (`replace_in_file`) wraps
/// content in `<<<<<<< SEARCH ... >>>>>>> REPLACE` markers, which acts as a
/// structural guard: malformed output usually fails diff parsing instead of
/// silently corrupting source files.
///
/// The choice is locked into a session at creation time (the tool-call
/// history would be inconsistent otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    /// `edit` tool with simple `old_text`/`new_text` string parameters.
    #[default]
    Simple,
    /// `replace_in_file` tool with `<<<<<<< SEARCH ... >>>>>>> REPLACE` blocks.
    Diff,
}

impl EditFormat {
    /// Returns true if this format uses the diff-style `replace_in_file` tool.
    pub fn is_diff(self) -> bool {
        matches!(self, EditFormat::Diff)
    }
}

/// A model offered in the model picker: a model ID of a configured
/// provider, plus whatever the user overrides. Everything else comes from
/// the model's capabilities ([`ConfigurationSystem::model_capabilities`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider ID (a key of `providers.json`)
    pub provider: String,
    /// Model ID within the provider
    pub id: String,
    /// Display name; defaults to the catalog's or the provider's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Catalog model this one behaves like, for IDs the catalog does not
    /// know (e.g. a proxy's name for a known model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behaves_like: Option<String>,
    /// Context window override (token count)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_token_limit: Option<u32>,
    /// File-editing tool format override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_format: Option<EditFormat>,
    /// Effort for sessions that did not choose one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<Effort>,
    /// Extra request fields, merged into every request (JSON Merge Patch)
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub config: serde_json::Value,
}

impl ModelConfig {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            name: None,
            behaves_like: None,
            context_token_limit: None,
            edit_format: None,
            default_effort: None,
            config: serde_json::Value::Null,
        }
    }

    /// The stable reference to this model: `provider/id`.
    pub fn key(&self) -> String {
        Self::key_for(&self.provider, &self.id)
    }

    pub fn key_for(provider: &str, id: &str) -> String {
        format!("{provider}/{id}")
    }
}

/// The models offered in the picker, by key (`provider/id`).
pub type ModelsConfig = HashMap<String, ModelConfig>;

/// Combined configuration system
#[derive(Debug, Clone, Default)]
pub struct ConfigurationSystem {
    pub providers: ProvidersConfig,
    /// The models offered in the picker, by key (`provider/id`).
    pub models: ModelsConfig,
    /// The providers' last model listings.
    pub discovery: DiscoveryCache,
}

impl ConfigurationSystem {
    /// Pin the configuration directory for this process.
    ///
    /// Applications embedding the agent stack under their own home directory
    /// (e.g. pal with `$PAL_HOME`) call this once at startup so that every
    /// implicit [`ConfigurationSystem::load`] — session service, sub-agents,
    /// compaction, model pickers — reads `providers.json` and the model selection from
    /// that directory. Like `CODE_ASSISTANT_CONFIG_DIR` the override is
    /// exclusive (no other location is searched), and it takes precedence
    /// over the environment variable, which belongs to code-assistant
    /// itself. The first call wins; returns `false` if it was already set.
    pub fn set_config_dir(dir: impl Into<PathBuf>) -> bool {
        CONFIG_DIR_OVERRIDE.set(dir.into()).is_ok()
    }

    /// Load configuration from the default locations
    pub fn load() -> Result<Self> {
        let providers = Self::load_providers_config(None)?;
        let selection = ModelSelection::load_or_import()?;
        Self::from_parts(providers, selection, DiscoveryCache::load())
    }

    /// Assemble a configuration from its loaded parts.
    pub fn from_parts(
        providers: ProvidersConfig,
        selection: ModelSelection,
        discovery: DiscoveryCache,
    ) -> Result<Self> {
        let models: ModelsConfig = selection
            .models
            .into_iter()
            .map(|model| (model.key(), model))
            .collect();

        // Validate that all models reference valid providers
        Self::validate_model_provider_references(&models, &providers)?;

        Ok(Self {
            providers,
            models,
            discovery,
        })
    }

    /// Path of a file in the configuration directory (where it exists, or
    /// where it would be created).
    pub fn config_file_path(file_name: &str) -> PathBuf {
        Self::determine_config_path(file_name, None).0
    }

    /// Get the resolved path to providers.json (first found or fallback).
    pub fn providers_config_path() -> PathBuf {
        Self::determine_config_path("providers.json", None).0
    }

    /// Load providers configuration from file
    pub fn load_providers_config(custom_path: Option<PathBuf>) -> Result<ProvidersConfig> {
        let custom_path_ref = custom_path.as_ref();
        let (config_path, searched_paths) =
            Self::determine_config_path("providers.json", custom_path_ref);

        if !config_path.exists() {
            return Err(Self::missing_config_error(
                "Providers",
                &config_path,
                &searched_paths,
                "providers.example.json",
                custom_path_ref.is_some(),
            ));
        }

        let content = std::fs::read_to_string(&config_path).with_context(|| {
            format!("Failed to read providers config: {}", config_path.display())
        })?;

        let config: ProvidersConfig = serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse providers config: {}",
                config_path.display()
            )
        })?;

        // Substitute environment variables
        let config = Self::substitute_env_vars_in_providers(config)?;

        Ok(config)
    }

    /// Get the default path for providers configuration
    pub fn default_providers_path() -> PathBuf {
        let (path, _) = Self::determine_config_path("providers.json", None);
        path
    }

    /// The key of a model referred to by key or by name (sessions created
    /// before models had keys refer to them by their display name).
    pub fn resolve_key(&self, model_ref: &str) -> Option<String> {
        if self.models.contains_key(model_ref) {
            return Some(model_ref.to_string());
        }
        let mut keys: Vec<&String> = self.models.keys().collect();
        keys.sort();
        keys.iter()
            .find(|key| self.models[**key].name.as_deref() == Some(model_ref))
            .or_else(|| keys.iter().find(|key| self.display_name(key) == model_ref))
            .map(|key| key.to_string())
    }

    /// Get a model by key or name
    pub fn get_model(&self, model_ref: &str) -> Option<&ModelConfig> {
        self.models.get(&self.resolve_key(model_ref)?)
    }

    /// The name the model is shown with: the user's name for it, else the
    /// catalog's or provider's, else its ID — with the provider's label
    /// when another offered model has the same name.
    pub fn display_name(&self, key: &str) -> String {
        let Some(model) = self.models.get(key) else {
            return key.to_string();
        };
        if let Some(name) = &model.name {
            return name.clone();
        }
        let base = self.base_display_name(model);
        let ambiguous = self
            .models
            .iter()
            .any(|(other_key, other)| other_key != key && self.base_display_name(other) == base);
        match self.providers.get(&model.provider) {
            Some(provider) if ambiguous => format!("{base} · {}", provider.label),
            _ => base,
        }
    }

    fn base_display_name(&self, model: &ModelConfig) -> String {
        if let Some(name) = &model.name {
            return name.clone();
        }
        crate::capabilities::catalog::lookup(model.behaves_like.as_deref().unwrap_or(&model.id))
            .map(|entry| entry.name.clone())
            .or_else(|| {
                self.discovery
                    .model(&model.provider, &model.id)
                    .and_then(|discovered| discovered.display_name.clone())
            })
            .unwrap_or_else(|| model.id.clone())
    }

    /// Get a provider configuration by provider ID
    pub fn get_provider(&self, provider_id: &str) -> Option<&ProviderConfig> {
        self.providers.get(provider_id)
    }

    /// Get the full configuration for a model (model + provider)
    pub fn get_model_with_provider(
        &self,
        model_name: &str,
    ) -> Result<(&ModelConfig, &ProviderConfig)> {
        let model = self
            .get_model(model_name)
            .ok_or_else(|| anyhow::anyhow!("Model not found: {model_name}"))?;

        let provider = self.get_provider(&model.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "Provider not found for model {}: {}",
                model_name,
                model.provider
            )
        })?;

        Ok((model, provider))
    }

    /// The keys of the offered models, ordered by display name
    pub fn list_models(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.models.keys().cloned().collect();
        keys.sort_by_cached_key(|key| (self.display_name(key).to_lowercase(), key.clone()));
        keys
    }

    /// List all available provider IDs
    pub fn list_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// What an offered model can do (context, output limit, reasoning):
    /// the catalog and the provider's listing, with the user's overrides.
    pub fn model_capabilities(&self, model_ref: &str) -> Result<ModelCapabilities> {
        let (model, provider) = self.get_model_with_provider(model_ref)?;
        let api = Self::api_client_kind_for_configs(model, provider)?;
        let mut capabilities = ModelCapabilities::resolve(
            api,
            model.behaves_like.as_deref().unwrap_or(&model.id),
            self.discovery.model(&model.provider, &model.id),
        );
        if let Some(limit) = model.context_token_limit {
            capabilities.context_window = Some(limit);
        }
        if let Some(format) = model.edit_format {
            capabilities.edit_format = Some(format);
        }
        if let (Some(effort), Some(reasoning)) =
            (model.default_effort, capabilities.reasoning.as_mut())
        {
            reasoning.default = effort
                .closest_in(&reasoning.levels)
                .unwrap_or(reasoning.default);
        }
        capabilities.display_name = Some(self.display_name(&model.key()));
        Ok(capabilities)
    }

    /// The context window of an offered model, if known.
    pub fn context_limit(&self, model_ref: &str) -> Option<u32> {
        self.model_capabilities(model_ref).ok()?.context_window
    }

    /// Resolve the API/client family used by a configured model.
    pub fn model_api_client_kind(&self, model_name: &str) -> Result<LlmApiClientKind> {
        let (model, provider) = self.get_model_with_provider(model_name)?;
        Self::api_client_kind_for_configs(model, provider)
    }

    /// Resolve the API/client family used by a model/provider pair.
    pub fn api_client_kind_for_configs(
        model: &ModelConfig,
        provider: &ProviderConfig,
    ) -> Result<LlmApiClientKind> {
        match provider.provider.as_str() {
            "ai-core" => Self::aicore_api_client_kind(model, provider),
            "anthropic" => Ok(LlmApiClientKind::Anthropic),
            "openai" => Ok(LlmApiClientKind::OpenAi),
            "openai-responses" => Ok(LlmApiClientKind::OpenAiResponses),
            "openai-responses-ws" => Ok(LlmApiClientKind::OpenAiResponsesWs),
            "vertex" => Ok(LlmApiClientKind::Vertex),
            "ollama" => Ok(LlmApiClientKind::Ollama),
            "openrouter" => Ok(LlmApiClientKind::OpenRouter),
            "cerebras" => Ok(LlmApiClientKind::Cerebras),
            "groq" => Ok(LlmApiClientKind::Groq),
            "minimax" => Ok(LlmApiClientKind::Minimax),
            "mistral-ai" => Ok(LlmApiClientKind::MistralAi),
            "moonshot" => Ok(LlmApiClientKind::Moonshot),
            "z-ai" => Ok(LlmApiClientKind::Zai),
            other => Err(anyhow::anyhow!("Unknown provider type: {other}")),
        }
    }

    fn aicore_api_client_kind(
        model: &ModelConfig,
        provider: &ProviderConfig,
    ) -> Result<LlmApiClientKind> {
        let deployment = provider
            .config
            .get("models")
            .and_then(|v| v.as_object())
            .and_then(|models| models.get(&model.id))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No deployment found for model '{}' in AI Core provider config",
                    model.id
                )
            })?;

        let api_type = if deployment.is_string() {
            "anthropic"
        } else {
            deployment
                .as_object()
                .and_then(|obj| obj.get("api_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("anthropic")
        };

        match api_type {
            "anthropic" => Ok(LlmApiClientKind::Anthropic),
            "openai" => Ok(LlmApiClientKind::OpenAi),
            "openai-responses" => Ok(LlmApiClientKind::OpenAiResponses),
            "vertex" => Ok(LlmApiClientKind::Vertex),
            other => Err(anyhow::anyhow!(
                "Unknown AI Core API type: '{}'. Expected one of: anthropic, openai, openai-responses, vertex",
                other
            )),
        }
    }

    /// Save providers configuration back to disk.
    ///
    /// This reads the raw JSON from the file (preserving `${ENV_VAR}` patterns
    /// that have not been substituted), merges in the provided changes, and
    /// writes the result back.
    pub fn save_providers_config(
        custom_path: Option<&Path>,
        mutate: impl FnOnce(&mut serde_json::Value) -> Result<()>,
    ) -> Result<()> {
        let path = if let Some(p) = custom_path {
            p.to_path_buf()
        } else {
            Self::default_providers_path()
        };

        // Read the raw JSON (with ${ENV_VAR} patterns still intact)
        let content = if path.exists() {
            std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read providers config: {}", path.display()))?
        } else {
            "{}".to_string()
        };

        let mut raw: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse providers config: {}", path.display()))?;

        mutate(&mut raw)?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&raw)?;
        std::fs::write(&path, &json)?;

        // Set file permissions to 0600 on Unix (may contain secrets)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }

        info!("Saved providers config to {}", path.display());
        Ok(())
    }

    /// Substitute environment variables in provider configurations
    fn substitute_env_vars_in_providers(mut config: ProvidersConfig) -> Result<ProvidersConfig> {
        for (provider_id, provider_config) in &mut config {
            provider_config.config = Self::substitute_env_vars_in_value(
                provider_config.config.clone(),
            )
            .with_context(|| format!("Failed to substitute env vars in provider: {provider_id}"))?;
        }
        Ok(config)
    }

    /// Recursively substitute environment variables in JSON values
    fn substitute_env_vars_in_value(value: serde_json::Value) -> Result<serde_json::Value> {
        match value {
            serde_json::Value::String(s) => Ok(serde_json::Value::String(
                Self::substitute_env_vars_in_string(&s)?,
            )),
            serde_json::Value::Object(mut map) => {
                for (_key, val) in &mut map {
                    *val = Self::substitute_env_vars_in_value(val.clone())?;
                }
                Ok(serde_json::Value::Object(map))
            }
            serde_json::Value::Array(mut arr) => {
                for item in &mut arr {
                    *item = Self::substitute_env_vars_in_value(item.clone())?;
                }
                Ok(serde_json::Value::Array(arr))
            }
            other => Ok(other),
        }
    }

    /// Substitute environment variables in a string (${VAR_NAME} format)
    fn substitute_env_vars_in_string(input: &str) -> Result<String> {
        let mut result = input.to_string();

        // Find all ${VAR_NAME} patterns
        while let Some(start) = result.find("${") {
            let end = result[start..].find('}').ok_or_else(|| {
                anyhow::anyhow!("Unclosed environment variable substitution: {input}")
            })?;
            let end = start + end;

            let var_name = &result[start + 2..end];
            let var_value = std::env::var(var_name)
                .with_context(|| format!("Environment variable not set: {var_name}"))?;

            result.replace_range(start..=end, &var_value);
        }

        Ok(result)
    }

    /// Validate that all models reference valid providers
    fn validate_model_provider_references(
        models: &ModelsConfig,
        providers: &ProvidersConfig,
    ) -> Result<()> {
        for (model_name, model_config) in models {
            if !providers.contains_key(&model_config.provider) {
                return Err(anyhow::anyhow!(
                    "Model '{}' references unknown provider: {}",
                    model_name,
                    model_config.provider
                ));
            }
        }
        Ok(())
    }

    /// Determine the configuration path to use along with all searched locations
    fn determine_config_path(
        filename: &str,
        custom_path: Option<&PathBuf>,
    ) -> (PathBuf, Vec<PathBuf>) {
        if let Some(path) = custom_path {
            return (path.clone(), vec![path.clone()]);
        }

        let mut searched_paths = Vec::new();
        for base_dir in Self::config_directories() {
            let candidate = base_dir.join(filename);
            if !searched_paths.iter().any(|existing| existing == &candidate) {
                let exists = candidate.exists();
                searched_paths.push(candidate.clone());
                if exists {
                    return (candidate, searched_paths);
                }
            }
        }

        let fallback = searched_paths
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from(filename));
        (fallback, searched_paths)
    }

    /// Compute all directories that may contain configuration files, ordered by priority
    fn config_directories() -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        // Both overrides are exclusive — don't search anywhere else. The
        // programmatic one wins: it is set by the embedding application,
        // while the environment variable configures code-assistant.
        if let Some(dir) = CONFIG_DIR_OVERRIDE.get() {
            dirs.push(dir.clone());
            return dirs;
        }
        if let Ok(custom_dir) = std::env::var("CODE_ASSISTANT_CONFIG_DIR") {
            dirs.push(PathBuf::from(custom_dir));
            return dirs;
        }

        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            Self::push_unique_dir(&mut dirs, PathBuf::from(xdg_config).join("code-assistant"));
        }
        if let Some(home_dir) = dirs::home_dir() {
            Self::push_unique_dir(&mut dirs, home_dir.join(".config").join("code-assistant"));
        }
        if let Some(system_config) = dirs::config_dir() {
            Self::push_unique_dir(&mut dirs, system_config.join("code-assistant"));
        }
        if let Ok(current_dir) = std::env::current_dir() {
            Self::push_unique_dir(&mut dirs, current_dir.join("code-assistant"));
        }

        if dirs.is_empty() {
            dirs.push(PathBuf::from("code-assistant"));
        }

        dirs
    }

    /// Insert a path into the list while keeping only unique entries
    fn push_unique_dir(dirs: &mut Vec<PathBuf>, candidate: PathBuf) {
        if !dirs.iter().any(|existing| existing == &candidate) {
            dirs.push(candidate);
        }
    }

    /// Build a helpful missing configuration error message
    fn missing_config_error(
        config_label: &str,
        resolved_path: &Path,
        searched_paths: &[PathBuf],
        example_file: &str,
        custom: bool,
    ) -> anyhow::Error {
        if custom {
            return anyhow::anyhow!(
                "{config_label} configuration file not found: {}\nPlease ensure the path is correct.",
                resolved_path.display()
            );
        }

        let searched_display = if searched_paths.is_empty() {
            "  (no search paths available)".to_string()
        } else {
            searched_paths
                .iter()
                .map(|path| format!("  {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n")
        };

        anyhow::anyhow!(
            "{config_label} configuration file not found.\n\
            Searched locations:\n{searched}\n\n\
            Please copy {example_file} to {target} and configure it.",
            searched = searched_display,
            example_file = example_file,
            target = resolved_path.display(),
        )
    }

    /// The edit format an offered model uses (`Simple` when unknown).
    pub fn edit_format_for(&self, model_ref: &str) -> EditFormat {
        self.model_capabilities(model_ref)
            .ok()
            .and_then(|capabilities| capabilities.edit_format)
            .unwrap_or_default()
    }
}

/// Resolve the `EditFormat` for a model, loading the
/// configuration on demand. Returns `EditFormat::Simple` on any error
/// (missing config file, unknown model, parse failure) — callers that
/// require the model to exist should use `ConfigurationSystem::get_model`
/// directly.
pub fn edit_format_for_model(model_name: &str) -> EditFormat {
    match ConfigurationSystem::load() {
        Ok(config) => config.edit_format_for(model_name),
        Err(_) => EditFormat::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_var_substitution() {
        temp_env::with_var("TEST_VAR", Some("test_value"), || {
            let input = "prefix_${TEST_VAR}_suffix";
            let result = ConfigurationSystem::substitute_env_vars_in_string(input).unwrap();
            assert_eq!(result, "prefix_test_value_suffix");
        });
    }

    #[test]
    fn test_env_var_substitution_missing() {
        let input = "prefix_${NONEXISTENT_VAR}_suffix";
        let result = ConfigurationSystem::substitute_env_vars_in_string(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("NONEXISTENT_VAR"));
    }

    #[test]
    fn test_env_var_substitution_unclosed() {
        let input = "prefix_${UNCLOSED_VAR_suffix";
        let result = ConfigurationSystem::substitute_env_vars_in_string(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unclosed"));
    }

    #[test]
    fn test_json_value_substitution() {
        temp_env::with_vars(
            [
                ("TEST_API_KEY", Some("secret_key")),
                ("TEST_URL", Some("https://api.example.com")),
            ],
            || {
                let input = serde_json::json!({
                    "api_key": "${TEST_API_KEY}",
                    "base_url": "${TEST_URL}",
                    "nested": {
                        "value": "${TEST_API_KEY}"
                    },
                    "array": ["${TEST_URL}", "static_value"]
                });

                let result = ConfigurationSystem::substitute_env_vars_in_value(input).unwrap();

                assert_eq!(result["api_key"], "secret_key");
                assert_eq!(result["base_url"], "https://api.example.com");
                assert_eq!(result["nested"]["value"], "secret_key");
                assert_eq!(result["array"][0], "https://api.example.com");
                assert_eq!(result["array"][1], "static_value");
            },
        );
    }

    #[test]
    fn test_config_dir_override_is_exclusive_and_beats_env() {
        // Process-global by design; this is the only test in the binary that
        // touches the override or the search list, so no cross-test bleed.
        assert!(ConfigurationSystem::set_config_dir("/pal/home"));
        assert!(
            !ConfigurationSystem::set_config_dir("/elsewhere"),
            "first call wins"
        );

        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some("/env/dir"), || {
            let dirs = ConfigurationSystem::config_directories();
            assert_eq!(dirs, vec![PathBuf::from("/pal/home")]);
        });
    }

    fn provider(provider_type: &str, label: &str) -> ProviderConfig {
        ProviderConfig {
            label: label.into(),
            provider: provider_type.into(),
            config: serde_json::json!({}),
        }
    }

    fn config(models: Vec<ModelConfig>) -> ConfigurationSystem {
        let providers = [
            ("anthropic".to_string(), provider("anthropic", "Anthropic")),
            ("hai".to_string(), provider("anthropic", "HAI Proxy")),
        ]
        .into_iter()
        .collect();
        ConfigurationSystem::from_parts(
            providers,
            ModelSelection { models },
            DiscoveryCache::default(),
        )
        .unwrap()
    }

    #[test]
    fn models_are_found_by_key_or_name() {
        let legacy = ModelConfig {
            name: Some("Claude Opus 4.8 (SAP, HAI)".into()),
            ..ModelConfig::new("hai", "anthropic--claude-4.8-opus")
        };
        let config = config(vec![ModelConfig::new("anthropic", "claude-opus-5"), legacy]);

        assert_eq!(
            config.resolve_key("anthropic/claude-opus-5").as_deref(),
            Some("anthropic/claude-opus-5")
        );
        assert_eq!(
            config.resolve_key("Claude Opus 4.8 (SAP, HAI)").as_deref(),
            Some("hai/anthropic--claude-4.8-opus")
        );
        assert_eq!(
            config.resolve_key("Claude Opus 5").as_deref(),
            Some("anthropic/claude-opus-5"),
            "the catalog name"
        );
        assert!(config.get_model("GPT-9").is_none());
    }

    #[test]
    fn same_model_on_two_providers_shows_the_provider() {
        let config = config(vec![
            ModelConfig::new("anthropic", "claude-opus-4-8"),
            ModelConfig::new("hai", "anthropic--claude-4.8-opus"),
        ]);
        assert_eq!(
            config.display_name("anthropic/claude-opus-4-8"),
            "Claude Opus 4.8 · Anthropic"
        );
        assert_eq!(
            config.display_name("hai/anthropic--claude-4.8-opus"),
            "Claude Opus 4.8 · HAI Proxy"
        );
    }

    #[test]
    fn overrides_win_over_the_catalog() {
        let config = config(vec![ModelConfig {
            context_token_limit: Some(200_000),
            edit_format: Some(EditFormat::Diff),
            default_effort: Some(Effort::Minimal),
            ..ModelConfig::new("anthropic", "claude-opus-5")
        }]);
        let capabilities = config
            .model_capabilities("anthropic/claude-opus-5")
            .unwrap();
        assert_eq!(capabilities.context_window, Some(200_000));
        assert_eq!(
            config.edit_format_for("anthropic/claude-opus-5"),
            EditFormat::Diff
        );
        // Minimal is not an Opus level: the closest one becomes the default.
        assert_eq!(capabilities.reasoning.unwrap().default, Effort::Low);
    }

    #[test]
    fn behaves_like_borrows_a_catalog_entry() {
        let config = config(vec![ModelConfig {
            behaves_like: Some("claude-sonnet-5".into()),
            ..ModelConfig::new("hai", "anthropic--claude-5-sonnet")
        }]);
        let capabilities = config
            .model_capabilities("hai/anthropic--claude-5-sonnet")
            .unwrap();
        assert_eq!(
            capabilities.display_name.as_deref(),
            Some("Claude Sonnet 5")
        );
        assert!(capabilities.reasoning.is_some());
    }

    #[test]
    fn models_of_unknown_providers_are_rejected() {
        let result = ConfigurationSystem::from_parts(
            ProvidersConfig::new(),
            ModelSelection {
                models: vec![ModelConfig::new("missing", "model")],
            },
            DiscoveryCache::default(),
        );
        assert!(result.is_err());
    }
}
