//! Provider suggestions for onboarding.
//!
//! When no providers/models are configured, the settings screen shows
//! suggestion cards that users can click to quickly set up common providers.
//! Some suggestions are context-sensitive (e.g. HAI Proxy only shown for SAP users).

#![allow(dead_code)] // Fields are part of the public API, used by UI code

use code_assistant_core::session::default_model::DefaultModelConfig;
use llm::model_selection::ModelSelection;
use llm::provider_config::ModelConfig;
use serde_json::{Map, Value};

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
    /// Which fields the user needs to fill in before applying. Their
    /// values go into the config of every provider below.
    pub required_fields: Vec<SuggestionField>,
    /// The provider entries to write to providers.json. The first model of
    /// the first provider becomes the default model if there is none yet.
    pub providers: Vec<ProviderTemplate>,
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
    /// The provider's key in providers.json.
    pub key: &'static str,
    pub label: &'static str,
    pub provider_type: &'static str,
    /// Static config fields (base_url, etc.). Does NOT include user-provided fields.
    pub static_config: Map<String, Value>,
    /// The provider's models to offer in the picker right away.
    pub models: Vec<ModelTemplate>,
}

/// A model a suggestion offers, with the request config it needs through
/// this provider (on top of what the capability catalog knows).
#[derive(Clone, Debug)]
pub struct ModelTemplate {
    pub id: &'static str,
    /// The model's `config` (a JSON Merge Patch for its requests).
    pub config: Value,
}

/// Models that need no config of their own.
fn models(ids: &[&'static str]) -> Vec<ModelTemplate> {
    ids.iter()
        .map(|&id| ModelTemplate {
            id,
            config: Value::Null,
        })
        .collect()
}

/// A config object from `(key, value)` pairs.
fn config_of<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn base_url(url: &str) -> Map<String, Value> {
    config_of([("base_url", Value::from(url))])
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
        let output = process_env::std_command("git")
            .args(["config", "--global", "user.email"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let email = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if email.is_empty() { None } else { Some(email) }
    }

    /// Get the system username (whoami).
    fn get_system_username() -> Option<String> {
        let output = process_env::std_command("whoami").output().ok()?;

        if !output.status.success() {
            return None;
        }

        let user = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if user.is_empty() { None } else { Some(user) }
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
    // One provider per API the proxy offers, all with the same key. The
    // `hai-proxy` key predates the others; configs and sessions refer to it.
    let provider = |key, provider_type, static_config, models| ProviderTemplate {
        key,
        label: "HAI Proxy",
        provider_type,
        static_config,
        models,
    };

    ProviderSuggestion {
        id: "suggestion-hai-proxy",
        title: "SAP Hyperspace AI Proxy",
        description: "Use Claude, GPT and Gemini via SAP's HAI Proxy running locally. Requires the Hyperspace AI Proxy tool to be installed and running.",
        icon: "icons/ai_sap.svg",
        required_fields: vec![SuggestionField {
            key: "api_key",
            label: "API Key",
            placeholder: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
            is_secret: false,
            help_text: Some("Find your machine-specific API key in the HAI Proxy settings."),
        }],
        providers: vec![
            provider(
                "hai-proxy",
                "anthropic",
                base_url("http://localhost:6655/anthropic/v1"),
                models(&["anthropic--claude-4.8-opus", "anthropic--claude-4.6-sonnet"]),
            ),
            provider(
                "hai-proxy-openai",
                "openai-responses",
                base_url("http://localhost:6655/openai/v1"),
                models(&["gpt-6.1-sol"]),
            ),
            provider(
                "hai-proxy-vertex",
                "vertex",
                config_of([
                    (
                        "base_url",
                        Value::from("http://localhost:6655/gemini/v1beta"),
                    ),
                    ("bearer_auth", Value::Bool(true)),
                ]),
                // The proxy rejects streamed function-call arguments.
                vec![ModelTemplate {
                    id: "gemini-3.8-flash",
                    config: serde_json::json!({ "tool_config": { "function_calling_config": {
                        "mode": "AUTO", "stream_function_call_arguments": null
                    } } }),
                }],
            ),
        ],
        context_requirement: ContextRequirement::SapUser,
    }
}

fn anthropic_suggestion() -> ProviderSuggestion {
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
        providers: vec![ProviderTemplate {
            key: "anthropic",
            label: "Anthropic",
            provider_type: "anthropic",
            static_config: base_url("https://api.anthropic.com/v1"),
            models: models(&["claude-opus-5-5", "claude-sonnet-5-5"]),
        }],
        context_requirement: ContextRequirement::None,
    }
}

fn openai_suggestion() -> ProviderSuggestion {
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
        providers: vec![ProviderTemplate {
            key: "openai",
            label: "OpenAI",
            provider_type: "openai-responses",
            static_config: base_url("https://api.openai.com/v1"),
            models: models(&["gpt-5.6"]),
        }],
        context_requirement: ContextRequirement::None,
    }
}

fn chatgpt_subscription_suggestion() -> ProviderSuggestion {
    ProviderSuggestion {
        id: "suggestion-chatgpt",
        title: "ChatGPT Subscription",
        description: "Use your existing ChatGPT Plus/Pro subscription. Requires browser login via OAuth.",
        icon: "icons/ai_open_ai.svg",
        required_fields: vec![], // No fields needed - uses OAuth flow
        providers: vec![ProviderTemplate {
            key: "openai-chatgpt",
            label: "ChatGPT Subscription",
            provider_type: "openai-responses-ws",
            static_config: config_of([("codex_auth", Value::Bool(true))]),
            models: models(&["gpt-6.1-sol"]),
        }],
        context_requirement: ContextRequirement::None,
    }
}

// ---------------------------------------------------------------------------
// Apply suggestion: writes provider + models to config files
// ---------------------------------------------------------------------------

impl ProviderSuggestion {
    /// What applying the suggestion creates, for the card.
    pub fn summary(&self) -> String {
        let models: usize = self.providers.iter().map(|p| p.models.len()).sum();
        let models = format!("{models} model{}", if models == 1 { "" } else { "s" });
        match self.providers.as_slice() {
            [provider] => format!(
                "This will create the provider \"{}\" and {models}.",
                provider.label
            ),
            providers => format!(
                "This will create {} providers and {models}.",
                providers.len()
            ),
        }
    }

    /// Add the suggestion's providers to `providers` (the providers.json
    /// object), offer their models in `selection` and, if there is no
    /// default model yet, make the first one the default.
    ///
    /// `user_fields` maps field key -> user-provided value (e.g. "api_key" ->
    /// "sk-...") and goes into every provider's config.
    pub fn apply_to(
        &self,
        user_fields: &Map<String, Value>,
        providers: &mut Map<String, Value>,
        selection: &mut ModelSelection,
        default_model: &mut DefaultModelConfig,
    ) {
        for template in &self.providers {
            let mut config = template.static_config.clone();
            config.extend(user_fields.clone());

            let mut entry = Map::new();
            entry.insert("label".to_string(), Value::from(template.label));
            entry.insert("provider".to_string(), Value::from(template.provider_type));
            entry.insert("config".to_string(), Value::Object(config));
            providers.insert(template.key.to_string(), Value::Object(entry));

            for model in &template.models {
                selection.set_enabled(template.key, model.id, true);
                if !model.config.is_null()
                    && let Some(entry) =
                        selection.get_mut(&ModelConfig::key_for(template.key, model.id))
                {
                    entry.config = model.config.clone();
                }
            }
        }

        if default_model.model.is_none()
            && let Some(first) = self.providers.first()
            && let Some(model) = first.models.first()
        {
            default_model.model = Some(ModelConfig::key_for(first.key, model.id));
        }
    }
}

/// Apply a suggestion by writing its provider and model configs to disk.
pub fn apply_suggestion(
    suggestion: &ProviderSuggestion,
    user_fields: &Map<String, Value>,
) -> anyhow::Result<()> {
    let providers_path = llm::provider_config::ConfigurationSystem::providers_config_path();
    let mut providers: Map<String, Value> = if providers_path.exists() {
        let content = std::fs::read_to_string(&providers_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Map::new()
    };
    let mut selection = ModelSelection::load_or_import()?;
    let mut default_model = DefaultModelConfig::load();
    let had_default = default_model.model.is_some();

    suggestion.apply_to(
        user_fields,
        &mut providers,
        &mut selection,
        &mut default_model,
    );

    if let Some(parent) = providers_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&providers_path, serde_json::to_string_pretty(&providers)?)?;
    selection.save()?;
    if !had_default && default_model.model.is_some() {
        default_model.save()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::model_selection::ModelSelection;
    use serde_json::json;

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

    fn api_key_fields() -> Map<String, Value> {
        let mut fields = Map::new();
        fields.insert("api_key".into(), json!("machine-key"));
        fields
    }

    #[test]
    fn hai_proxy_creates_a_provider_per_api() {
        let mut providers = Map::new();
        let mut selection = ModelSelection::default();
        let mut default_model = DefaultModelConfig::default();

        hai_proxy_suggestion().apply_to(
            &api_key_fields(),
            &mut providers,
            &mut selection,
            &mut default_model,
        );

        assert_eq!(
            Value::Object(providers),
            json!({
                "hai-proxy": {
                    "label": "HAI Proxy",
                    "provider": "anthropic",
                    "config": {
                        "api_key": "machine-key",
                        "base_url": "http://localhost:6655/anthropic/v1"
                    }
                },
                "hai-proxy-openai": {
                    "label": "HAI Proxy",
                    "provider": "openai-responses",
                    "config": {
                        "api_key": "machine-key",
                        "base_url": "http://localhost:6655/openai/v1"
                    }
                },
                "hai-proxy-vertex": {
                    "label": "HAI Proxy",
                    "provider": "vertex",
                    "config": {
                        "api_key": "machine-key",
                        "base_url": "http://localhost:6655/gemini/v1beta",
                        "bearer_auth": true
                    }
                }
            })
        );
        let offered: Vec<String> = selection.models.iter().map(|m| m.key()).collect();
        // The HAI Proxy rejects Gemini's streamed function-call arguments.
        assert_eq!(
            selection
                .get("hai-proxy-vertex/gemini-3.8-flash")
                .unwrap()
                .config,
            json!({ "tool_config": { "function_calling_config": {
                "mode": "AUTO", "stream_function_call_arguments": null
            } } })
        );
        assert_eq!(
            selection
                .get("hai-proxy/anthropic--claude-4.8-opus")
                .unwrap()
                .config,
            Value::Null
        );
        assert_eq!(
            offered,
            [
                "hai-proxy/anthropic--claude-4.8-opus",
                "hai-proxy/anthropic--claude-4.6-sonnet",
                "hai-proxy-openai/gpt-6.1-sol",
                "hai-proxy-vertex/gemini-3.8-flash",
            ]
        );
        assert_eq!(
            default_model.model.as_deref(),
            Some("hai-proxy/anthropic--claude-4.8-opus")
        );
    }

    #[test]
    fn applying_keeps_other_providers_and_the_default_model() {
        let mut providers = Map::new();
        providers.insert("anthropic".into(), json!({ "provider": "anthropic" }));
        let mut selection = ModelSelection::default();
        let mut default_model = DefaultModelConfig {
            model: Some("anthropic/claude-opus-5-5".into()),
        };

        hai_proxy_suggestion().apply_to(
            &api_key_fields(),
            &mut providers,
            &mut selection,
            &mut default_model,
        );

        assert!(providers.contains_key("anthropic"));
        assert_eq!(
            default_model.model.as_deref(),
            Some("anthropic/claude-opus-5-5")
        );
    }

    #[test]
    fn summary_names_a_single_provider_and_counts_several() {
        assert_eq!(
            anthropic_suggestion().summary(),
            "This will create the provider \"Anthropic\" and 2 models."
        );
        assert_eq!(
            openai_suggestion().summary(),
            "This will create the provider \"OpenAI\" and 1 model."
        );
        assert_eq!(
            hai_proxy_suggestion().summary(),
            "This will create 3 providers and 4 models."
        );
    }
}
