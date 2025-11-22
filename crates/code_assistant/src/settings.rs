use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Global agent settings loaded from ~/.config/code-assistant/settings.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentSettings {
    pub parallel_api_key: Option<String>,
}

static SETTINGS: OnceLock<AgentSettings> = OnceLock::new();

/// Get the loaded settings, initializing them lazily on first use.
pub fn get_settings() -> &'static AgentSettings {
    SETTINGS.get_or_init(|| match load_settings_from_disk() {
        Ok(settings) => settings,
        Err(err) => {
            tracing::warn!("Failed to load settings: {err}");
            AgentSettings::default()
        }
    })
}

/// Convenience helper used by tools that care about the Parallel API key.
pub fn parallel_api_key() -> Option<String> {
    get_settings().parallel_api_key.clone()
}

fn load_settings_from_disk() -> Result<AgentSettings> {
    let config_dir = crate::config::config_dir()?;
    let settings_path = config_dir.join("settings.json");

    if !settings_path.exists() {
        return Ok(AgentSettings::default());
    }

    let contents = std::fs::read_to_string(&settings_path).map_err(|err| {
        tracing::warn!(
            "Failed to read settings from {}: {err}",
            settings_path.display()
        );
        err
    })?;

    let mut settings: AgentSettings = serde_json::from_str(&contents).map_err(|err| {
        tracing::warn!(
            "Failed to parse settings from {}: {err}",
            settings_path.display()
        );
        err
    })?;

    // Support env var substitution similar to providers.json by allowing ${VAR}
    if let Some(api_key) = &mut settings.parallel_api_key {
        if let Some(resolved) = substitute_env_vars(api_key) {
            *api_key = resolved;
        }
    }

    Ok(settings)
}

fn substitute_env_vars(input: &str) -> Option<String> {
    let mut result = input.to_string();
    let mut changed = false;
    while let Some(start) = result.find("${") {
        let end = result[start..].find('}')?;
        let end = start + end;
        let var_name = &result[start + 2..end];
        let var_value = std::env::var(var_name).ok()?;
        result.replace_range(start..=end, &var_value);
        changed = true;
    }

    if changed {
        Some(result)
    } else {
        None
    }
}
