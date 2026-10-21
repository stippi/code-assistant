//! What a model can do, and how to ask for it.
//!
//! The single place that knows a model's context window, output limit,
//! preferred edit format and reasoning support. Clients send neutral
//! requests; the factory merges [`ModelCapabilities::request_overlay`] into
//! them, which translates the session's [`Effort`] into the provider's
//! fields.
//!
//! Sources, most specific first: the built-in [`catalog`], then
//! [`fallback`] rules per API family for models the catalog does not know.

pub mod catalog;
mod effort;
mod fallback;
mod reasoning;

pub use effort::Effort;
pub use reasoning::{ReasoningSpec, ReasoningStyle};

use crate::provider_config::{EditFormat, LlmApiClientKind};
use serde_json::{Value, json};

/// Output limit requested from Anthropic models. Larger limits need
/// streaming, which non-streaming side requests (compaction) do not use.
const ANTHROPIC_MAX_TOKENS_CAP: u32 = 64000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub display_name: Option<String>,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub edit_format: Option<EditFormat>,
    pub reasoning: Option<ReasoningSpec>,
}

impl ModelCapabilities {
    /// Capabilities of `model_id` served through an API of kind `api`.
    pub fn resolve(api: LlmApiClientKind, model_id: &str) -> Self {
        match catalog::lookup(model_id) {
            Some(entry) => Self {
                display_name: Some(entry.name.clone()),
                context_window: entry.context_window,
                max_output: entry.max_output,
                edit_format: entry.edit_format,
                reasoning: entry.reasoning.clone(),
            },
            None => Self {
                reasoning: fallback::reasoning(api, model_id),
                ..Self::default()
            },
        }
    }

    /// Request fields for this model at `effort` (`None`: the model's
    /// default), as a JSON Merge Patch for the request body.
    pub fn request_overlay(&self, api: LlmApiClientKind, effort: Option<Effort>) -> Value {
        let mut overlay = self
            .reasoning
            .as_ref()
            .map(|spec| spec.request_patch(effort))
            .unwrap_or_else(|| json!({}));

        if api == LlmApiClientKind::Anthropic {
            let max_tokens = match (self.max_output, &self.reasoning) {
                (Some(limit), _) => Some(limit.min(ANTHROPIC_MAX_TOKENS_CAP)),
                (None, Some(_)) => Some(ANTHROPIC_MAX_TOKENS_CAP),
                (None, None) => None,
            };
            if let Some(max_tokens) = max_tokens {
                overlay["max_tokens"] = json!(max_tokens);
            }
        }
        overlay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_models_get_their_reasoning_and_output_limit() {
        let caps = ModelCapabilities::resolve(LlmApiClientKind::Anthropic, "claude-opus-5");
        assert_eq!(caps.display_name.as_deref(), Some("Claude Opus 5"));
        assert_eq!(caps.context_window, Some(1_000_000));
        assert_eq!(
            caps.request_overlay(LlmApiClientKind::Anthropic, Some(Effort::Medium)),
            json!({
                "thinking": { "type": "adaptive", "display": "summarized" },
                "output_config": { "effort": "medium" },
                "max_tokens": 64000
            })
        );
    }

    #[test]
    fn proxy_aliases_resolve_to_the_catalog_model() {
        let caps =
            ModelCapabilities::resolve(LlmApiClientKind::Anthropic, "anthropic--claude-4.6-sonnet");
        assert_eq!(caps.display_name.as_deref(), Some("Claude Sonnet 4.6"));
        let overlay = caps.request_overlay(LlmApiClientKind::Anthropic, None);
        assert_eq!(overlay["thinking"]["type"], "adaptive");
    }

    #[test]
    fn unknown_anthropic_model_gets_a_neutral_request() {
        let caps = ModelCapabilities::resolve(LlmApiClientKind::Anthropic, "claude-unknown-9");
        assert_eq!(caps, ModelCapabilities::default());
        assert_eq!(
            caps.request_overlay(LlmApiClientKind::Anthropic, Some(Effort::High)),
            json!({})
        );
    }

    #[test]
    fn max_tokens_is_only_set_for_anthropic() {
        let caps = ModelCapabilities::resolve(LlmApiClientKind::Vertex, "gemini-3.5-flash");
        assert_eq!(
            caps.request_overlay(LlmApiClientKind::Vertex, Some(Effort::Minimal)),
            json!({ "generation_config": { "thinking_config": { "thinking_level": "minimal" } } })
        );
    }
}
