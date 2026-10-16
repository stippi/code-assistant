//! Configuration for tools that require external API keys or settings.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Configuration for tools that require external services.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// API key for Perplexity service (enables perplexity_ask tool)
    #[serde(default)]
    pub perplexity_api_key: Option<String>,
    /// Global settings of the built-in tool groups, by group id (see
    /// [`crate::tools::builtin_selection`]). A group without an entry is on,
    /// and on in new sessions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub builtin_tools: BTreeMap<String, BuiltinToolGroupSettings>,
}

/// Global settings of one built-in tool group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinToolGroupSettings {
    /// Off hides the group's tools from every session.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// New sessions start with the group off (unless their project
    /// remembers otherwise); a session can still switch it on.
    #[serde(default)]
    pub off_in_new_sessions: bool,
}

impl Default for BuiltinToolGroupSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            off_in_new_sessions: false,
        }
    }
}

fn default_true() -> bool {
    true
}

impl ToolsConfig {
    /// Load the tools configuration from disk.
    /// Returns Ok with default config if the file doesn't exist.
    pub fn load() -> Result<Self> {
        // Substitute environment variables in API keys
        Self::substitute_env_vars(Self::load_raw()?)
    }

    /// Load the tools configuration without substituting environment
    /// variables, as the settings screen edits and saves it.
    pub fn load_raw() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read tools config: {}", config_path.display()))?;

        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse tools config: {}", config_path.display()))
    }

    /// Persist a configuration loaded with [`Self::load_raw`].
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;
        crate::utils::file_utils::atomic_write_json(&config_path, self)
            .with_context(|| format!("Failed to write tools config: {}", config_path.display()))
    }

    /// The global settings of a built-in tool group.
    pub fn builtin_group(&self, id: &str) -> BuiltinToolGroupSettings {
        self.builtin_tools.get(id).cloned().unwrap_or_default()
    }

    /// Get the path to the tools configuration file.
    pub fn config_path() -> Result<PathBuf> {
        Ok(crate::config_dir::config_dir().join("tools.json"))
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
    pub(crate) fn substitute_env_var_in_string(input: &str) -> Result<String> {
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

    #[test]
    fn test_default_config() {
        let config = ToolsConfig::default();
        assert!(config.perplexity_api_key.is_none());
        assert!(!config.has_perplexity_api_key());
    }

    #[test]
    fn test_env_var_substitution() {
        temp_env::with_var("TEST_PERPLEXITY_KEY", Some("pplx-test-key"), || {
            let input = "${TEST_PERPLEXITY_KEY}";
            let result = ToolsConfig::substitute_env_var_in_string(input).unwrap();
            assert_eq!(result, "pplx-test-key");
        });
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
