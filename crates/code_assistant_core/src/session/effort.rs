//! The reasoning effort a session can choose for its current model.

use llm::capabilities::{Effort, ModelCapabilities};
use llm::provider_config::ConfigurationSystem;

/// The effort levels offered for a session's model, and the one its
/// requests run at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffortChoice {
    /// Levels the model supports, ascending; empty when it does not reason
    /// (frontends then hide the selector).
    pub levels: Vec<Effort>,
    /// The level requests run at: the session's choice moved to a supported
    /// level, or the model's default. `None` without levels.
    pub selected: Option<Effort>,
}

impl EffortChoice {
    pub fn new(capabilities: &ModelCapabilities, requested: Option<Effort>) -> Self {
        match &capabilities.reasoning {
            Some(spec) => Self {
                levels: spec.levels.clone(),
                selected: Some(spec.effective_effort(requested)),
            },
            None => Self::default(),
        }
    }

    /// The choice for a model of the current configuration; empty when the
    /// model is unknown.
    pub fn for_model(model_name: &str, requested: Option<Effort>) -> Self {
        ConfigurationSystem::load()
            .and_then(|config| config.model_capabilities(model_name))
            .map(|capabilities| Self::new(&capabilities, requested))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::provider_config::LlmApiClientKind;

    #[test]
    fn offers_the_model_levels_and_moves_the_request_onto_them() {
        let fable = ModelCapabilities::resolve(LlmApiClientKind::Anthropic, "claude-fable-5", None);
        let choice = EffortChoice::new(&fable, Some(Effort::Off));
        assert_eq!(choice.levels.first(), Some(&Effort::Low));
        assert_eq!(choice.selected, Some(Effort::Low));

        assert_eq!(EffortChoice::new(&fable, None).selected, Some(Effort::High));
    }

    #[test]
    fn models_without_reasoning_offer_nothing() {
        let plain = ModelCapabilities::resolve(LlmApiClientKind::Groq, "llama-4-scout", None);
        assert_eq!(
            EffortChoice::new(&plain, Some(Effort::High)),
            EffortChoice::default()
        );
    }
}
