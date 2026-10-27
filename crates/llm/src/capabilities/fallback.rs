//! Reasoning support for models the catalog does not know, guessed from the
//! API family and the model ID.

use super::{Effort, ReasoningSpec, ReasoningStyle};
use crate::provider_config::LlmApiClientKind;

const LOW_TO_HIGH: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];

pub(super) fn reasoning(api: LlmApiClientKind, model_id: &str) -> Option<ReasoningSpec> {
    let id = model_id.to_ascii_lowercase();
    let id = id.rsplit('/').next().unwrap_or(&id);
    match api {
        // Everything on the Responses API reasons, except the GPT-4 family.
        LlmApiClientKind::OpenAiResponses | LlmApiClientKind::OpenAiResponsesWs => {
            (!id.starts_with("gpt-4")).then(|| ReasoningSpec {
                style: ReasoningStyle::OpenAiResponses,
                levels: LOW_TO_HIGH.to_vec(),
                default: Effort::Medium,
            })
        }
        // Chat Completions: only the known reasoning families accept
        // `reasoning_effort`; other models reject it.
        LlmApiClientKind::OpenAi
        | LlmApiClientKind::OpenRouter
        | LlmApiClientKind::Groq
        | LlmApiClientKind::Cerebras => ["o3", "o4", "gpt-oss"]
            .iter()
            .any(|family| id.starts_with(family))
            .then(|| ReasoningSpec {
                style: ReasoningStyle::ChatCompletionsEffort,
                levels: LOW_TO_HIGH.to_vec(),
                default: Effort::Low,
            }),
        // Anthropic and the rest: no reasoning fields unless the catalog (or
        // the user) says otherwise — a wrong guess is a 400.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_models_reason_except_gpt4() {
        let spec = reasoning(LlmApiClientKind::OpenAiResponsesWs, "gpt-7-preview").unwrap();
        assert_eq!(spec.style, ReasoningStyle::OpenAiResponses);
        assert_eq!(spec.default, Effort::Medium);
        assert!(reasoning(LlmApiClientKind::OpenAiResponses, "gpt-4o").is_none());
    }

    #[test]
    fn chat_completions_reasoning_families() {
        let spec = reasoning(LlmApiClientKind::OpenRouter, "openai/o4-mini").unwrap();
        assert_eq!(spec.style, ReasoningStyle::ChatCompletionsEffort);
        assert!(reasoning(LlmApiClientKind::Groq, "llama-4-scout").is_none());
    }

    #[test]
    fn anthropic_does_not_guess() {
        assert!(reasoning(LlmApiClientKind::Anthropic, "claude-opus-9").is_none());
    }
}
