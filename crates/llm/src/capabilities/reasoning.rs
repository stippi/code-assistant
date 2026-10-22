use super::Effort;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How a provider API expresses a reasoning effort in the request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningStyle {
    /// Anthropic `thinking: {type: "adaptive"}` + `output_config.effort`
    /// (Claude 4.6 and later).
    AnthropicAdaptive,
    /// Anthropic `thinking: {type: "enabled", budget_tokens: N}`, the budget
    /// derived from the level (models before Claude 4.6).
    AnthropicBudget,
    /// OpenAI Responses API `reasoning: {effort, summary}`.
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    /// Chat Completions `reasoning_effort` (OpenAI, Groq, Cerebras, Moonshot, ...).
    ChatCompletionsEffort,
    /// Gemini `generation_config.thinking_config.thinking_level`.
    Gemini,
}

impl ReasoningStyle {
    /// The request fields that put a model of this style at `effort`, as a
    /// JSON Merge Patch for the request body.
    pub fn request_patch(self, effort: Effort) -> Value {
        match self {
            ReasoningStyle::AnthropicAdaptive => match effort {
                Effort::Off => json!({ "thinking": { "type": "disabled" } }),
                level => json!({
                    "thinking": { "type": "adaptive", "display": "summarized" },
                    "output_config": { "effort": level.as_str() },
                }),
            },
            ReasoningStyle::AnthropicBudget => match effort {
                Effort::Off => json!({ "thinking": { "type": "disabled" } }),
                level => json!({
                    "thinking": { "type": "enabled", "budget_tokens": thinking_budget(level) },
                }),
            },
            ReasoningStyle::OpenAiResponses => match effort {
                Effort::Off => json!({ "reasoning": { "effort": "none" } }),
                level => json!({ "reasoning": { "effort": level.as_str(), "summary": "auto" } }),
            },
            ReasoningStyle::ChatCompletionsEffort => match effort {
                Effort::Off => json!({ "reasoning_effort": "none" }),
                level => json!({ "reasoning_effort": level.as_str() }),
            },
            ReasoningStyle::Gemini => match effort {
                Effort::Off => json!({
                    "generation_config": { "thinking_config": { "thinking_budget": 0 } }
                }),
                level => json!({
                    "generation_config": { "thinking_config": { "thinking_level": level.as_str() } }
                }),
            },
        }
    }
}

/// Thinking budget for the budget-based Anthropic models; always below the
/// 64k `max_tokens` the client requests for them.
fn thinking_budget(effort: Effort) -> u32 {
    match effort {
        Effort::Off | Effort::Minimal => 1024,
        Effort::Low => 4096,
        Effort::Medium => 8192,
        Effort::High => 16384,
        Effort::XHigh => 32768,
        Effort::Max => 48000,
    }
}

/// The reasoning a model supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningSpec {
    pub style: ReasoningStyle,
    /// Supported levels, ascending; contains `Off` when reasoning can be
    /// switched off.
    pub levels: Vec<Effort>,
    /// Level used when the session did not choose one.
    pub default: Effort,
}

impl ReasoningSpec {
    /// The level a request runs at: the requested one (or the default),
    /// moved to the closest level this model supports.
    pub fn effective_effort(&self, requested: Option<Effort>) -> Effort {
        requested
            .unwrap_or(self.default)
            .closest_in(&self.levels)
            .unwrap_or(self.default)
    }

    pub fn request_patch(&self, requested: Option<Effort>) -> Value {
        self.style.request_patch(self.effective_effort(requested))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(style: ReasoningStyle, levels: &[Effort], default: Effort) -> ReasoningSpec {
        ReasoningSpec {
            style,
            levels: levels.to_vec(),
            default,
        }
    }

    #[test]
    fn anthropic_adaptive_sets_thinking_and_effort() {
        let opus = spec(
            ReasoningStyle::AnthropicAdaptive,
            &[
                Effort::Off,
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::XHigh,
                Effort::Max,
            ],
            Effort::High,
        );
        assert_eq!(
            opus.request_patch(Some(Effort::XHigh)),
            json!({
                "thinking": { "type": "adaptive", "display": "summarized" },
                "output_config": { "effort": "xhigh" }
            })
        );
        assert_eq!(
            opus.request_patch(Some(Effort::Off)),
            json!({ "thinking": { "type": "disabled" } })
        );
        assert_eq!(
            opus.request_patch(None)["output_config"]["effort"],
            json!("high")
        );
    }

    #[test]
    fn anthropic_budget_derives_budget_tokens() {
        let haiku = spec(
            ReasoningStyle::AnthropicBudget,
            &[Effort::Off, Effort::Low, Effort::Medium, Effort::High],
            Effort::Medium,
        );
        assert_eq!(
            haiku.request_patch(Some(Effort::Max)),
            json!({ "thinking": { "type": "enabled", "budget_tokens": 16384 } })
        );
    }

    #[test]
    fn openai_styles() {
        let gpt = spec(
            ReasoningStyle::OpenAiResponses,
            &[Effort::Low, Effort::Medium, Effort::High],
            Effort::Medium,
        );
        assert_eq!(
            gpt.request_patch(Some(Effort::High)),
            json!({ "reasoning": { "effort": "high", "summary": "auto" } })
        );
        assert_eq!(
            ReasoningStyle::ChatCompletionsEffort.request_patch(Effort::Low),
            json!({ "reasoning_effort": "low" })
        );
    }

    #[test]
    fn gemini_sets_thinking_level() {
        assert_eq!(
            ReasoningStyle::Gemini.request_patch(Effort::Low),
            json!({ "generation_config": { "thinking_config": { "thinking_level": "low" } } })
        );
    }

    #[test]
    fn unsupported_request_moves_to_closest_level() {
        let fable = spec(
            ReasoningStyle::AnthropicAdaptive,
            &[
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::XHigh,
                Effort::Max,
            ],
            Effort::High,
        );
        assert_eq!(fable.effective_effort(Some(Effort::Off)), Effort::Low);
        assert_eq!(fable.effective_effort(None), Effort::High);
    }
}
