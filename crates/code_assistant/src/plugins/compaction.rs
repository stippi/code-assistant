//! Compaction policy: compact once the context window fills up past a
//! fixed threshold.

use crate::agent::hooks::{CompactionPolicy, ContextSnapshot};
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
