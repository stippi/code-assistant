//! Provider suggestions for onboarding.
//!
//! When no providers/models are configured, the settings screen shows
//! suggestion cards that users can click to quickly set up common providers.
//! Some suggestions are context-sensitive (e.g. HAI Proxy only shown for SAP users).

#![allow(dead_code)] // Fields are part of the public API, used by UI code

use serde_json::{json, Map, Value};
use std::process::Command;

/// A suggested provider + model configuration that can be applied with minimal user input.
#[derive(Clone, Debug)]
pub struct ProviderSuggestion {
    /// Unique ID for this suggestion (used as element ID).
    pub id: &'static str,
    /// Display name shown in the suggestion card.
    pub title: &'static str,
    /// Short description of what this provider offers.
    pub description: &'static str,
    /// Icon path for the provider.
    pub icon: &'static str,
    /// Which fields the user needs to fill in before applying.
    pub required_fields: Vec<SuggestionField>,
    /// The provider entry to write to providers.json (template with placeholders).
    pub provider_key: &'static str,
    pub provider_config: ProviderTemplate,
    /// Model entries to write to models.json.
    pub models: Vec<ModelTemplate>,
    /// Whether this suggestion requires a specific user context (e.g. SAP user).
    pub context_requirement: ContextRequirement,
}

/// A field that the user must fill in for a suggestion.
#[derive(Clone, Debug)]
pub struct SuggestionField {
    pub key: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
    /// If true, display as password/masked input.
    pub is_secret: bool,
    /// Help text shown below the field.
    pub help_text: Option<&'static str>,
}

/// Template for a provider entry in providers.json.
#[derive(Clone, Debug)]
pub struct ProviderTemplate {
    pub label: &'static str,
    pub provider_type: &'static str,
    /// Static config fields (base_url, etc.). Does NOT include user-provided fields.
    pub static_config: Map<String, Value>,
}

/// Template for a model entry in models.json.
#[derive(Clone, Debug)]
pub struct ModelTemplate {
    pub display_name: &'static str,
    pub model_id: &'static str,
    pub context_token_limit: u32,
    pub config: Value,
}

/// What environment context is needed to show this suggestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextRequirement {
    /// Always show this suggestion.
    None,
    /// Only show for detected SAP users.
    SapUser,
}

// ---------------------------------------------------------------------------
// User environment detection
// ---------------------------------------------------------------------------

/// Detected user environment info.
#[derive(Clone, Debug)]
pub struct UserEnvironment {
    /// The user's global git email, if configured.
    pub git_email: Option<String>,
    /// Whether the user appears to be an SAP employee.
    pub is_sap_user: bool,
}

impl UserEnvironment {
    /// Detect the current user's environment by checking git config and system user.
    pub fn detect() -> Self {
        let git_email = Self::get_git_global_email();
        let username = Self::get_system_username();

        let is_sap_user = git_email
            .as_ref()
            .map(|e| e.ends_with("@sap.com"))
            .unwrap_or(false)
            || username
                .as_ref()
                .map(|u| Self::is_sap_username(u))
                .unwrap_or(false);

        Self {
            git_email,
            is_sap_user,
        }
    }

    /// Get the global git user.email config value.
    fn get_git_global_email() -> Option<String> {
        let output = Command::new("git")
            .args(["config", "--global", "user.email"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let email = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if email.is_empty() {
            None
        } else {
            Some(email)
        }
    }

    /// Get the system username (whoami).
    fn get_system_username() -> Option<String> {
        let output = Command::new("whoami").output().ok()?;

        if !output.status.success() {
            return None;
        }

        let user = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if user.is_empty() {
            None
        } else {
            Some(user)
        }
    }

    /// Check if a username matches SAP's I-user or D-user pattern (e.g. I531928, D012345).
    fn is_sap_username(username: &str) -> bool {
        let upper = username.to_uppercase();
        if upper.len() >= 7 {
            let first = upper.chars().next().unwrap_or(' ');
            (first == 'I' || first == 'D') && upper[1..].chars().all(|c| c.is_ascii_digit())
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Suggestion definitions
// ---------------------------------------------------------------------------

/// Get all provider suggestions applicable to the current user.
pub fn get_suggestions(env: &UserEnvironment) -> Vec<ProviderSuggestion> {
    let mut suggestions = Vec::new();

    // SAP HAI Proxy (only for SAP users)
    if env.is_sap_user {
        suggestions.push(hai_proxy_suggestion());
    }

    // Generic suggestions for everyone
    suggestions.push(anthropic_suggestion());
    suggestions.push(openai_suggestion());
    suggestions.push(chatgpt_subscription_suggestion());

    suggestions
}

fn hai_proxy_suggestion() -> ProviderSuggestion {
    let mut static_config = Map::new();
    static_config.insert(
        "base_url".to_string(),
        Value::String("http://localhost:6655/anthropic/v1".to_string()),
    );

    ProviderSuggestion {
        id: "suggestion-hai-proxy",
        title: "SAP Hyperspace AI Proxy",
        description: "Use Claude via SAP's HAI Proxy running locally. Requires the Hyperspace AI Proxy tool to be installed and running.",
        icon: "icons/ai_sap.svg",
        required_fields: vec![SuggestionField {
            key: "api_key",
            label: "API Key",
            placeholder: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
            is_secret: false,
            help_text: Some("Find your machine-specific API key in the HAI Proxy settings."),
        }],
        provider_key: "hai-proxy",
        provider_config: ProviderTemplate {
            label: "HAI Proxy",
            provider_type: "anthropic",
            static_config,
        },
        models: vec![
            ModelTemplate {
                display_name: "Claude Sonnet 4.6 (HAI Proxy)",
                model_id: "anthropic--claude-4.6-sonnet",
                context_token_limit: 200000,
                config: json!({
                    "max_tokens": 64000,
                    "temperature": 1.0,
                    "thinking": {
                        "type": "enabled",
                        "budget_tokens": 16384
                    }
                }),
            },
            ModelTemplate {
                display_name: "Claude Opus 4.6 (HAI Proxy)",
                model_id: "anthropic--claude-4.6-opus",
                context_token_limit: 200000,
                config: json!({
                    "max_tokens": 64000,
                    "temperature": 1.0,
                    "thinking": {
                        "type": "enabled",
                        "budget_tokens": 16384
                    }
                }),
            },
        ],
        context_requirement: ContextRequirement::SapUser,
    }
}

fn anthropic_suggestion() -> ProviderSuggestion {
    let mut static_config = Map::new();
    static_config.insert(
        "base_url".to_string(),
        Value::String("https://api.anthropic.com/v1".to_string()),
    );

    ProviderSuggestion {
        id: "suggestion-anthropic",
        title: "Anthropic",
        description: "Direct access to Claude models via Anthropic's API platform.",
        icon: "icons/ai_anthropic.svg",
        required_fields: vec![SuggestionField {
            key: "api_key",
            label: "API Key",
            placeholder: "sk-ant-...",
            is_secret: true,
            help_text: Some("Get your API key at console.anthropic.com"),
        }],
        provider_key: "anthropic",
        provider_config: ProviderTemplate {
            label: "Anthropic",
            provider_type: "anthropic",
            static_config,
        },
        models: vec![ModelTemplate {
            display_name: "Claude Sonnet 4.6 (Anthropic)",
            model_id: "claude-sonnet-4-6-20250610",
            context_token_limit: 200000,
            config: json!({
                "max_tokens": 64000,
                "temperature": 1.0,
                "thinking": {
                    "type": "enabled",
                    "budget_tokens": 16384
                }
            }),
        }],
        context_requirement: ContextRequirement::None,
    }
}

fn openai_suggestion() -> ProviderSuggestion {
    let mut static_config = Map::new();
    static_config.insert(
        "base_url".to_string(),
        Value::String("https://api.openai.com/v1".to_string()),
    );

    ProviderSuggestion {
        id: "suggestion-openai",
        title: "OpenAI",
        description: "Access GPT and reasoning models via OpenAI's API platform.",
        icon: "icons/ai_open_ai.svg",
        required_fields: vec![SuggestionField {
            key: "api_key",
            label: "API Key",
            placeholder: "sk-...",
            is_secret: true,
            help_text: Some("Get your API key at platform.openai.com"),
        }],
        provider_key: "openai",
        provider_config: ProviderTemplate {
            label: "OpenAI",
            provider_type: "openai-responses",
            static_config,
        },
        models: vec![ModelTemplate {
            display_name: "GPT-4.1 (OpenAI)",
            model_id: "gpt-4.1",
            context_token_limit: 1047576,
            config: json!({
                "reasoning": {
                    "effort": "medium",
                    "summary": "auto"
                }
            }),
        }],
        context_requirement: ContextRequirement::None,
    }
}

fn chatgpt_subscription_suggestion() -> ProviderSuggestion {
    ProviderSuggestion {
        id: "suggestion-chatgpt",
        title: "ChatGPT Subscription",
        description:
            "Use your existing ChatGPT Plus/Pro subscription. Requires browser login via OAuth.",
        icon: "icons/ai_open_ai.svg",
        required_fields: vec![], // No fields needed - uses OAuth flow
        provider_key: "openai-chatgpt",
        provider_config: ProviderTemplate {
            label: "ChatGPT Subscription",
            provider_type: "openai-responses-ws",
            static_config: {
                let mut m = Map::new();
                m.insert("codex_auth".to_string(), Value::Bool(true));
                m
            },
        },
        models: vec![ModelTemplate {
            display_name: "GPT-4.1 (ChatGPT)",
            model_id: "gpt-4.1",
            context_token_limit: 1047576,
            config: json!({
                "reasoning": {
                    "effort": "medium",
                    "summary": "auto"
                }
            }),
        }],
        context_requirement: ContextRequirement::None,
    }
}

// ---------------------------------------------------------------------------
// Apply suggestion: writes provider + models to config files
// ---------------------------------------------------------------------------

/// Apply a suggestion by writing its provider and model configs to disk.
///
/// `user_fields` is a map of field key -> user-provided value (e.g. "api_key" -> "sk-...").
pub fn apply_suggestion(
    suggestion: &ProviderSuggestion,
    user_fields: &Map<String, Value>,
) -> anyhow::Result<()> {
    // Build provider config object
    let mut config = suggestion.provider_config.static_config.clone();
    for (key, value) in user_fields {
        config.insert(key.clone(), value.clone());
    }

    let mut provider_entry = Map::new();
    provider_entry.insert(
        "label".to_string(),
        Value::String(suggestion.provider_config.label.to_string()),
    );
    provider_entry.insert(
        "provider".to_string(),
        Value::String(suggestion.provider_config.provider_type.to_string()),
    );
    provider_entry.insert("config".to_string(), Value::Object(config));

    // Write to providers.json
    let providers_path = llm::provider_config::ConfigurationSystem::providers_config_path();
    let mut providers_map: Map<String, Value> = if providers_path.exists() {
        let content = std::fs::read_to_string(&providers_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Map::new()
    };

    providers_map.insert(
        suggestion.provider_key.to_string(),
        Value::Object(provider_entry),
    );

    // Ensure parent directory exists
    if let Some(parent) = providers_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let providers_json = serde_json::to_string_pretty(&providers_map)?;
    std::fs::write(&providers_path, providers_json)?;

    // Write models to models.json
    let models_path = llm::provider_config::ConfigurationSystem::models_config_path();
    let mut models_map: Map<String, Value> = if models_path.exists() {
        let content = std::fs::read_to_string(&models_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Map::new()
    };

    for model in &suggestion.models {
        let mut model_entry = Map::new();
        model_entry.insert(
            "provider".to_string(),
            Value::String(suggestion.provider_key.to_string()),
        );
        model_entry.insert("id".to_string(), Value::String(model.model_id.to_string()));
        model_entry.insert(
            "context_token_limit".to_string(),
            Value::Number(model.context_token_limit.into()),
        );
        model_entry.insert("config".to_string(), model.config.clone());

        models_map.insert(model.display_name.to_string(), Value::Object(model_entry));
    }

    if let Some(parent) = models_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let models_json = serde_json::to_string_pretty(&models_map)?;
    std::fs::write(&models_path, models_json)?;

    // Set the first model as default if no default is configured yet
    let mut settings = crate::ui::gpui::settings::UiSettings::load();
    if settings.default_model.is_none() {
        if let Some(first_model) = suggestion.models.first() {
            settings.default_model = Some(first_model.display_name.to_string());
            settings.save();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sap_username_detection() {
        assert!(UserEnvironment::is_sap_username("I531928"));
        assert!(UserEnvironment::is_sap_username("D012345"));
        assert!(UserEnvironment::is_sap_username("i531928")); // case insensitive
        assert!(!UserEnvironment::is_sap_username("john"));
        assert!(!UserEnvironment::is_sap_username("I1234")); // too short
        assert!(!UserEnvironment::is_sap_username("X531928")); // wrong prefix
    }

    #[test]
    fn test_suggestions_for_sap_user() {
        let env = UserEnvironment {
            git_email: Some("test@sap.com".to_string()),
            is_sap_user: true,
        };
        let suggestions = get_suggestions(&env);
        assert!(suggestions.iter().any(|s| s.id == "suggestion-hai-proxy"));
        assert!(suggestions.iter().any(|s| s.id == "suggestion-anthropic"));
        assert!(suggestions.iter().any(|s| s.id == "suggestion-openai"));
        assert!(suggestions.iter().any(|s| s.id == "suggestion-chatgpt"));
    }

    #[test]
    fn test_suggestions_for_non_sap_user() {
        let env = UserEnvironment {
            git_email: Some("dev@example.com".to_string()),
            is_sap_user: false,
        };
        let suggestions = get_suggestions(&env);
        assert!(!suggestions.iter().any(|s| s.id == "suggestion-hai-proxy"));
        assert!(suggestions.iter().any(|s| s.id == "suggestion-anthropic"));
    }
}
