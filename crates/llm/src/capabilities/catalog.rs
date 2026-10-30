//! The built-in list of models whose capabilities we know.
//!
//! Kept deliberately small: the models we actually use, plus aliases for the
//! IDs proxies give them. Discovery and user overrides cover the rest.

use super::ReasoningSpec;
use crate::provider_config::EditFormat;
use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub edit_format: Option<EditFormat>,
    pub reasoning: Option<ReasoningSpec>,
}

static CATALOG: LazyLock<Vec<CatalogEntry>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("catalog.json")).expect("built-in model catalog is valid")
});

/// Find the entry for a model ID (or one of its aliases). Date-stamped IDs
/// (`claude-sonnet-4-5-20250929`, `claude-opus-4-5@20251101`) match their
/// undated entry.
pub fn lookup(model_id: &str) -> Option<&'static CatalogEntry> {
    let undated = strip_date_suffix(model_id);
    CATALOG.iter().find(|entry| {
        std::iter::once(&entry.id)
            .chain(&entry.aliases)
            .any(|id| id == model_id || id == undated)
    })
}

fn strip_date_suffix(model_id: &str) -> &str {
    let Some(pos) = model_id.rfind(['-', '@']) else {
        return model_id;
    };
    let suffix = &model_id[pos + 1..];
    if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) {
        &model_id[..pos]
    } else {
        model_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{Effort, ReasoningStyle};

    #[test]
    fn catalog_parses_and_every_default_is_a_supported_level() {
        assert!(!CATALOG.is_empty());
        for entry in CATALOG.iter() {
            if let Some(reasoning) = &entry.reasoning {
                assert!(
                    reasoning.levels.contains(&reasoning.default),
                    "{}: default {:?} not in levels",
                    entry.id,
                    reasoning.default
                );
                assert!(
                    reasoning.levels.windows(2).all(|w| w[0] < w[1]),
                    "{}: levels must be ascending",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn looks_up_ids_aliases_and_dated_ids() {
        let opus = lookup("claude-opus-4-8").unwrap();
        assert_eq!(opus.name, "Claude Opus 4.8");
        let reasoning = opus.reasoning.as_ref().unwrap();
        assert_eq!(reasoning.style, ReasoningStyle::AnthropicAdaptive);
        assert!(reasoning.levels.contains(&Effort::XHigh));

        assert_eq!(
            lookup("anthropic--claude-4.8-opus").unwrap().id,
            "claude-opus-4-8"
        );
        assert_eq!(
            lookup("claude-sonnet-4-5-20250929").unwrap().id,
            "claude-sonnet-4-5"
        );
        assert_eq!(
            lookup("claude-haiku-4-5@20251001").unwrap().id,
            "claude-haiku-4-5"
        );
        assert!(lookup("claude-opus-4").is_none());
    }

    #[test]
    fn version_suffixes_are_not_dates() {
        assert_eq!(strip_date_suffix("gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(strip_date_suffix("qwen3.8:27b-mlx"), "qwen3.8:27b-mlx");
    }

    #[test]
    fn looks_up_luna_models_with_responses_capabilities() {
        for id in ["gpt-5.6-luna", "gpt-6-luna"] {
            let entry = lookup(id).unwrap();
            assert_eq!(
                entry.context_window,
                Some(1_050_000),
                "{id}: context window"
            );
            assert_eq!(entry.max_output, Some(128_000), "{id}: max output");
            assert_eq!(
                entry.edit_format,
                Some(crate::provider_config::EditFormat::Diff),
                "{id}: edit format"
            );
            let reasoning = entry.reasoning.as_ref().unwrap();
            assert_eq!(
                reasoning.style,
                ReasoningStyle::OpenAiResponses,
                "{id}: reasoning style"
            );
            assert_eq!(
                reasoning.levels,
                [Effort::Low, Effort::Medium, Effort::High, Effort::XHigh],
                "{id}: supported reasoning levels"
            );
            assert_eq!(reasoning.default, Effort::High, "{id}: default effort");
        }
    }
}
