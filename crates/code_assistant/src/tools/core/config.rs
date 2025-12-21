//! Configuration for tools that require external API keys or settings.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Configuration for tools that require external services.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// API key for Perplexity service (enables perplexity_ask tool)
    #[serde(default)]
    pub perplexity_api_key: Option<String>,
}

impl ToolsConfig {
    /// Get the global singleton instance of the tools configuration.
    /// Returns a default (empty) config if no configuration file exists.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<ToolsConfig> = OnceLock::new();
        INSTANCE.get_or_init(|| Self::load().unwrap_or_default())
    }

    /// Load the tools configuration from disk.
    /// Returns Ok with default config if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read tools config: {}", config_path.display()))?;

        let config: ToolsConfig = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse tools config: {}", config_path.display()))?;

        // Substitute environment variables in API keys
        Ok(Self::substitute_env_vars(config)?)
    }

    /// Get the path to the tools configuration file.
    pub fn config_path() -> Result<PathBuf> {
        // Use the same config directory as other code-assistant configs
        let config_dir = Self::config_directory()?;
        Ok(config_dir.join("tools.json"))
    }

    /// Get the configuration directory.
    fn config_directory() -> Result<PathBuf> {
        // Check for custom config directory first
        if let Ok(custom_dir) = std::env::var("CODE_ASSISTANT_CONFIG_DIR") {
            return Ok(PathBuf::from(custom_dir));
        }

        // Check XDG_CONFIG_HOME
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg_config).join("code-assistant"));
        }

        // Fall back to ~/.config/code-assistant
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        Ok(home.join(".config").join("code-assistant"))
    }

    /// Substitute environment variables in configuration values.
    /// Supports ${VAR_NAME} syntax.
    fn substitute_env_vars(mut config: Self) -> Result<Self> {
        if let Some(ref key) = config.perplexity_api_key {
            config.perplexity_api_key = Some(Self::substitute_env_var_in_string(key)?);
        }
        Ok(config)
    }

    /// Substitute environment variables in a string.
    fn substitute_env_var_in_string(input: &str) -> Result<String> {
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

    /// Check if Perplexity API key is configured.
    pub fn has_perplexity_api_key(&self) -> bool {
        self.perplexity_api_key
            .as_ref()
            .map(|k| !k.is_empty())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_default_config() {
        let config = ToolsConfig::default();
        assert!(config.perplexity_api_key.is_none());
        assert!(!config.has_perplexity_api_key());
    }

    #[test]
    fn test_env_var_substitution() {
        env::set_var("TEST_PERPLEXITY_KEY", "pplx-test-key");

        let input = "${TEST_PERPLEXITY_KEY}";
        let result = ToolsConfig::substitute_env_var_in_string(input).unwrap();
        assert_eq!(result, "pplx-test-key");

        env::remove_var("TEST_PERPLEXITY_KEY");
    }

    #[test]
    fn test_has_perplexity_api_key() {
        let mut config = ToolsConfig::default();
        assert!(!config.has_perplexity_api_key());

        config.perplexity_api_key = Some(String::new());
        assert!(!config.has_perplexity_api_key());

        config.perplexity_api_key = Some("pplx-xxx".to_string());
        assert!(config.has_perplexity_api_key());
    }
}
