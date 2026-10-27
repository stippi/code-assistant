//! Normalizing the model listings of the provider APIs.

use super::DiscoveredModel;
use crate::capabilities::{Effort, ReasoningSpec, ReasoningStyle};
use serde_json::Value;

/// Anthropic `GET /v1/models`: IDs, limits and a capability tree with the
/// thinking modes and effort levels.
pub(super) fn anthropic(page: &Value) -> Vec<DiscoveredModel> {
    entries(page, "data")
        .filter_map(|entry| {
            let mut model = DiscoveredModel::new(entry["id"].as_str()?);
            model.display_name = string(entry, "display_name");
            model.context_window = number(entry, "max_input_tokens");
            model.max_output = number(entry, "max_tokens");
            model.reasoning = anthropic_reasoning(&entry["capabilities"]);
            Some(model)
        })
        .collect()
}

fn anthropic_reasoning(capabilities: &Value) -> Option<ReasoningSpec> {
    let supported = |path: &[&str]| {
        path.iter()
            .fold(capabilities, |node, key| &node[*key])
            .get("supported")
            .and_then(Value::as_bool)
            == Some(true)
    };
    if supported(&["thinking", "types", "adaptive"]) {
        let levels: Vec<Effort> = [
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::XHigh,
            Effort::Max,
        ]
        .into_iter()
        .filter(|level| supported(&["effort", level.as_str()]))
        .collect();
        let default = if levels.contains(&Effort::High) {
            Effort::High
        } else {
            *levels.last()?
        };
        // Whether thinking can be switched off is not part of the listing;
        // the catalog knows it for the models that allow it.
        return Some(ReasoningSpec {
            style: ReasoningStyle::AnthropicAdaptive,
            levels,
            default,
        });
    }
    supported(&["thinking", "types", "enabled"]).then(|| ReasoningSpec {
        style: ReasoningStyle::AnthropicBudget,
        levels: vec![Effort::Low, Effort::Medium, Effort::High],
        default: Effort::High,
    })
}

/// OpenAI-style `GET /models` (OpenAI, Groq, Cerebras, Mistral, Moonshot,
/// Z.ai, OpenRouter). Mostly IDs; some add the context window.
pub(super) fn openai_compatible(page: &Value) -> Vec<DiscoveredModel> {
    entries(page, "data")
        .filter_map(|entry| {
            let mut model = DiscoveredModel::new(entry["id"].as_str()?);
            // OpenRouter
            model.display_name = string(entry, "name");
            model.context_window =
                number(entry, "context_window").or_else(|| number(entry, "context_length"));
            model.max_output = number(&entry["top_provider"], "max_completion_tokens");
            Some(model)
        })
        .collect()
}

/// Gemini `GET /models`: names carry a `models/` prefix; only models that
/// generate content are of use.
pub(super) fn gemini(page: &Value) -> Vec<DiscoveredModel> {
    entries(page, "models")
        .filter(|entry| {
            entry["supportedGenerationMethods"]
                .as_array()
                .is_none_or(|methods| methods.iter().any(|m| m == "generateContent"))
        })
        .filter_map(|entry| {
            let name = entry["name"].as_str()?;
            let mut model = DiscoveredModel::new(name.strip_prefix("models/").unwrap_or(name));
            model.display_name = string(entry, "displayName");
            model.context_window = number(entry, "inputTokenLimit");
            model.max_output = number(entry, "outputTokenLimit");
            // `thinking_level` exists from Gemini 3 on; older models take a
            // budget instead, which we leave to their defaults.
            if entry["thinking"].as_bool() == Some(true) && model.id.starts_with("gemini-3") {
                model.reasoning = Some(ReasoningSpec {
                    style: ReasoningStyle::Gemini,
                    levels: vec![Effort::Low, Effort::High],
                    default: Effort::High,
                });
            }
            Some(model)
        })
        .collect()
}

/// Ollama `GET /api/tags`: the locally installed models.
pub(super) fn ollama_tags(tags: &Value) -> Vec<DiscoveredModel> {
    entries(tags, "models")
        .filter_map(|entry| {
            let id = entry["model"].as_str().or_else(|| entry["name"].as_str())?;
            Some(DiscoveredModel::new(id))
        })
        .collect()
}

/// Ollama `POST /api/show`: the context length sits under an
/// architecture-specific key (`qwen3.context_length`, ...).
pub(super) fn ollama_show(details: &Value, model: &mut DiscoveredModel) {
    model.context_window = details["model_info"].as_object().and_then(|info| {
        info.iter()
            .find(|(key, _)| key.ends_with(".context_length"))
            .and_then(|(_, value)| value.as_u64())
            .and_then(|n| u32::try_from(n).ok())
    });
}

fn entries<'a>(page: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    page[key].as_array().into_iter().flatten()
}

fn string(entry: &Value, key: &str) -> Option<String> {
    entry[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn number(entry: &Value, key: &str) -> Option<u32> {
    entry[key].as_u64().and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_listing_with_capabilities() {
        let page = json!({
            "data": [{
                "id": "claude-opus-4-8",
                "display_name": "Claude Opus 4.8",
                "max_input_tokens": 1000000,
                "max_tokens": 128000,
                "capabilities": {
                    "thinking": { "supported": true, "types": {
                        "enabled": { "supported": false },
                        "adaptive": { "supported": true }
                    }},
                    "effort": {
                        "supported": true,
                        "low": { "supported": true }, "medium": { "supported": true },
                        "high": { "supported": true }, "xhigh": { "supported": true },
                        "max": { "supported": true }
                    }
                }
            }, {
                "id": "claude-haiku-4-5",
                "capabilities": { "thinking": { "types": { "enabled": { "supported": true } } } }
            }, {
                "id": "proxy-model-without-capabilities"
            }],
            "has_more": false
        });
        let models = anthropic(&page);
        assert_eq!(models.len(), 3);

        let opus = &models[0];
        assert_eq!(opus.display_name.as_deref(), Some("Claude Opus 4.8"));
        assert_eq!(opus.context_window, Some(1_000_000));
        let reasoning = opus.reasoning.as_ref().unwrap();
        assert_eq!(reasoning.style, ReasoningStyle::AnthropicAdaptive);
        assert_eq!(reasoning.levels.len(), 5);
        assert_eq!(reasoning.default, Effort::High);

        assert_eq!(
            models[1].reasoning.as_ref().unwrap().style,
            ReasoningStyle::AnthropicBudget
        );
        assert_eq!(models[2].reasoning, None);
    }

    #[test]
    fn openai_compatible_listings() {
        let groq = json!({ "data": [{ "id": "qwen-3-coder-480b", "context_window": 256000 }] });
        assert_eq!(openai_compatible(&groq)[0].context_window, Some(256_000));

        let openrouter = json!({ "data": [{
            "id": "openai/gpt-5.6", "name": "OpenAI: GPT-5.6", "context_length": 400000,
            "top_provider": { "max_completion_tokens": 128000 }
        }]});
        let model = &openai_compatible(&openrouter)[0];
        assert_eq!(model.display_name.as_deref(), Some("OpenAI: GPT-5.6"));
        assert_eq!(model.max_output, Some(128_000));
    }

    #[test]
    fn gemini_listing_keeps_content_models() {
        let page = json!({ "models": [
            { "name": "models/gemini-3.5-flash", "displayName": "Gemini 3.5 Flash",
              "inputTokenLimit": 1048576, "outputTokenLimit": 65536, "thinking": true,
              "supportedGenerationMethods": ["generateContent", "countTokens"] },
            { "name": "models/text-embedding-005", "supportedGenerationMethods": ["embedContent"] }
        ]});
        let models = gemini(&page);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-3.5-flash");
        assert_eq!(
            models[0].reasoning.as_ref().unwrap().style,
            ReasoningStyle::Gemini
        );
    }

    #[test]
    fn ollama_tags_and_details() {
        let tags = json!({ "models": [{ "name": "qwen3.8:27b-mlx", "model": "qwen3.8:27b-mlx" }] });
        let mut model = ollama_tags(&tags).remove(0);
        ollama_show(
            &json!({ "model_info": { "general.architecture": "qwen3", "qwen3.context_length": 131072 } }),
            &mut model,
        );
        assert_eq!(model.id, "qwen3.8:27b-mlx");
        assert_eq!(model.context_window, Some(131_072));
    }
}
