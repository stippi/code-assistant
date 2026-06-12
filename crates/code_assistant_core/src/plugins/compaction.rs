//! Compaction policy: compact once the context window fills up past a
//! fixed threshold.

use agent_core::hooks::{CompactionPolicy, ContextSnapshot};
use crate::plugins::AgentAppState;
use anyhow::Result;
use std::any::Any;
use tracing::debug;

pub struct TokenRatioCompaction {
    threshold: f32,
    prompt: &'static str,
}

impl TokenRatioCompaction {
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold,
            prompt: include_str!("../../resources/compaction_prompt.md"),
        }
    }
}

impl CompactionPolicy for TokenRatioCompaction {
    fn context_limit(&self, extensions: &(dyn Any + Send)) -> Result<Option<u32>> {
        let state = AgentAppState::of_ref(extensions);

        let model_name = match state.model_config.as_ref() {
            Some(config) => config.model_name.clone(),
            None => return Ok(None),
        };

        let limit = if let Some(limit) = state.context_limit_override {
            limit
        } else {
            let config_system = llm::provider_config::ConfigurationSystem::load()?;
            config_system
                .get_model(&model_name)
                .map(|model| model.context_token_limit)
                .ok_or_else(|| anyhow::anyhow!("Model not found in models.json: {model_name}"))?
        };

        Ok(if limit == 0 { None } else { Some(limit) })
    }

    fn should_compact(&self, snapshot: &ContextSnapshot) -> bool {
        if let Some(ratio) = snapshot.usage_ratio {
            if ratio >= self.threshold {
                debug!(
                    "Context usage {:.1}% >= threshold {:.0}% — triggering compaction",
                    ratio * 100.0,
                    self.threshold * 100.0
                );
                return true;
            }
        }
        false
    }

    fn compaction_prompt(&self) -> &str {
        self.prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_at_threshold() {
        let policy = TokenRatioCompaction::new(0.8);
        assert!(!policy.should_compact(&ContextSnapshot { usage_ratio: None }));
        assert!(!policy.should_compact(&ContextSnapshot {
            usage_ratio: Some(0.79)
        }));
        assert!(policy.should_compact(&ContextSnapshot {
            usage_ratio: Some(0.8)
        }));
    }
}
