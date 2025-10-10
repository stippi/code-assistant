use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

    /// Load models configuration from file
    pub fn load_models_config(custom_path: Option<PathBuf>) -> Result<ModelsConfig> {
        let custom_path_ref = custom_path.as_ref();
        let (config_path, searched_paths) =
            Self::determine_config_path("models.json", custom_path_ref);

        if !config_path.exists() {
            return Err(Self::missing_config_error(
                "Models",
                &config_path,
                &searched_paths,
                "models.example.json",
                custom_path_ref.is_some(),
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
        let (path, _) = Self::determine_config_path("providers.json", None);
        path
    }

    /// Get the default path for models configuration
    pub fn default_models_path() -> PathBuf {
        let (path, _) = Self::determine_config_path("models.json", None);
        path
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

        if let Ok(custom_dir) = std::env::var("CODE_ASSISTANT_CONFIG_DIR") {
            Self::push_unique_dir(&mut dirs, PathBuf::from(custom_dir));
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
