use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

/// Configuration for a single model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider ID that this model uses
    pub provider: String,
    /// Model ID within the provider
    pub id: String,
    /// Model-specific configuration
    pub config: serde_json::Value,
}

/// Configuration for all models (model_display_name -> ModelConfig)
pub type ModelsConfig = HashMap<String, ModelConfig>;

/// Combined configuration system
#[derive(Debug, Clone)]
pub struct ConfigurationSystem {
    pub providers: ProvidersConfig,
    pub models: ModelsConfig,
}

impl ConfigurationSystem {
    /// Load configuration from the default locations
    pub fn load() -> Result<Self> {
        let providers = Self::load_providers_config(None)?;
        let models = Self::load_models_config(None)?;

        // Validate that all models reference valid providers
        Self::validate_model_provider_references(&models, &providers)?;

        Ok(Self { providers, models })
    }

    /// Load configuration from custom file paths
    pub fn load_from_paths(
        providers_path: Option<PathBuf>,
        models_path: Option<PathBuf>,
    ) -> Result<Self> {
        let providers = Self::load_providers_config(providers_path)?;
        let models = Self::load_models_config(models_path)?;

        // Validate that all models reference valid providers
        Self::validate_model_provider_references(&models, &providers)?;

        Ok(Self { providers, models })
    }

    /// Load providers configuration from file
    pub fn load_providers_config(custom_path: Option<PathBuf>) -> Result<ProvidersConfig> {
        let config_path = custom_path.unwrap_or_else(Self::default_providers_path);

        if !config_path.exists() {
            return Err(anyhow::anyhow!(
                "Providers configuration file not found: {}\n\
                Please copy providers.example.json to providers.json and configure your API keys.",
                config_path.display()
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

    /// Load models configuration from file
    pub fn load_models_config(custom_path: Option<PathBuf>) -> Result<ModelsConfig> {
        let config_path = custom_path.unwrap_or_else(Self::default_models_path);

        if !config_path.exists() {
            return Err(anyhow::anyhow!(
                "Models configuration file not found: {}\n\
                Please copy models.example.json to models.json and configure your preferred models.",
                config_path.display()
            ));
        }

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read models config: {}", config_path.display()))?;

        let config: ModelsConfig = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse models config: {}", config_path.display()))?;

        Ok(config)
    }

    /// Get the default path for providers configuration
    pub fn default_providers_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join("code-assistant")
            .join("providers.json")
    }

    /// Get the default path for models configuration
    pub fn default_models_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join("code-assistant")
            .join("models.json")
    }

    /// Get a model configuration by display name
    pub fn get_model(&self, model_name: &str) -> Option<&ModelConfig> {
        self.models.get(model_name)
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
            .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_name))?;

        let provider = self.get_provider(&model.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "Provider not found for model {}: {}",
                model_name,
                model.provider
            )
        })?;

        Ok((model, provider))
    }

    /// List all available model names
    pub fn list_models(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }

    /// List all available provider IDs
    pub fn list_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_env_var_substitution() {
        // Set a test environment variable
        env::set_var("TEST_VAR", "test_value");

        let input = "prefix_${TEST_VAR}_suffix";
        let result = ConfigurationSystem::substitute_env_vars_in_string(input).unwrap();
        assert_eq!(result, "prefix_test_value_suffix");

        // Clean up
        env::remove_var("TEST_VAR");
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
        // Set test environment variables
        env::set_var("TEST_API_KEY", "secret_key");
        env::set_var("TEST_URL", "https://api.example.com");

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

        // Clean up
        env::remove_var("TEST_API_KEY");
        env::remove_var("TEST_URL");
    }
}
