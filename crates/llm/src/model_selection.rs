//! The models offered in the model picker, with the user's per-model
//! overrides (`model-selection.json`).
//!
//! Everything a model needs beyond its provider and ID comes from the
//! capability catalog and the provider's model listing; an entry only holds
//! what the user changed. The file replaces `models.json`, whose entries are
//! imported while it is missing (the old file stays untouched, older builds
//! keep reading it).

use crate::capabilities::{Effort, catalog};
use crate::config_merge::merge_patch;
use crate::provider_config::{ConfigurationSystem, ModelConfig};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::debug;

const FILE_NAME: &str = "model-selection.json";
const LEGACY_FILE_NAME: &str = "models.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSelection {
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

impl ModelSelection {
    pub fn path() -> PathBuf {
        ConfigurationSystem::config_file_path(FILE_NAME)
    }

    /// The selection from the config directory. Without one, the entries of
    /// a legacy `models.json` are imported — in memory only: loading never
    /// writes, the selection file appears with the first change saved.
    pub fn load_or_import() -> Result<Self> {
        let path = Self::path();
        if let Some(selection) = Self::load_from(&path)? {
            return Ok(selection);
        }
        let legacy_path = ConfigurationSystem::config_file_path(LEGACY_FILE_NAME);
        if !legacy_path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&legacy_path)
            .with_context(|| format!("Failed to read {}", legacy_path.display()))?;
        let legacy: HashMap<String, ModelConfig> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", legacy_path.display()))?;
        let selection = Self::import_legacy(legacy);
        debug!(
            "Imported {} models from {}",
            selection.models.len(),
            legacy_path.display()
        );
        Ok(selection)
    }

    /// `None` when the file does not exist.
    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let selection = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(Some(selection))
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("Failed to write {}", path.display()))
    }

    pub fn get(&self, key: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|model| model.key() == key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut ModelConfig> {
        self.models.iter_mut().find(|model| model.key() == key)
    }

    /// Offer `model_id` of `provider` in the picker, or stop offering it
    /// (dropping its overrides).
    pub fn set_enabled(&mut self, provider: &str, model_id: &str, enabled: bool) {
        let key = ModelConfig::key_for(provider, model_id);
        let present = self.get(&key).is_some();
        if enabled && !present {
            self.models.push(ModelConfig::new(provider, model_id));
        } else if !enabled && present {
            self.models.retain(|model| model.key() != key);
        }
    }

    /// Turn `models.json` entries (display name → model) into selection
    /// entries: the display name stays (sessions refer to models by it),
    /// the reasoning settings become the model's default effort (the
    /// capability catalog now writes them), and limits the catalog already
    /// knows are dropped.
    pub fn import_legacy(legacy: HashMap<String, ModelConfig>) -> Self {
        let mut models: Vec<ModelConfig> = legacy
            .into_iter()
            .map(|(name, mut model)| {
                let known = catalog::lookup(&model.id);
                model.name = Some(name);
                model.default_effort = legacy_effort(&model.config);
                model.config =
                    keep_replaced_tool_config(strip_reasoning(std::mem::take(&mut model.config)));
                if model.context_token_limit.is_some()
                    && model.context_token_limit == known.and_then(|e| e.context_window)
                {
                    model.context_token_limit = None;
                }
                if model.edit_format.unwrap_or_default()
                    == known.and_then(|e| e.edit_format).unwrap_or_default()
                {
                    model.edit_format = None;
                }
                model
            })
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Self { models }
    }
}

/// `models.json` configs replaced top-level request fields wholesale; now
/// they merge. A `tool_config` replaced the Gemini client's experimental
/// `stream_function_call_arguments` flag — the documented way to opt out of
/// it — so the import removes that flag explicitly.
fn keep_replaced_tool_config(mut config: Value) -> Value {
    if let Some(calling) = config
        .get_mut("tool_config")
        .and_then(|tool_config| tool_config.get_mut("function_calling_config"))
        .and_then(Value::as_object_mut)
    {
        calling
            .entry("stream_function_call_arguments")
            .or_insert(Value::Null);
    }
    config
}

/// The effort a legacy model config asked for, in any provider's dialect.
fn legacy_effort(config: &Value) -> Option<Effort> {
    [
        &config["output_config"]["effort"],
        &config["reasoning"]["effort"],
        &config["reasoning_effort"],
        &config["generation_config"]["thinking_config"]["thinking_level"],
    ]
    .into_iter()
    .find_map(|value| value.as_str().and_then(Effort::parse))
}

/// Remove the request fields the capability catalog now sets from the
/// session's effort.
fn strip_reasoning(config: Value) -> Value {
    if !config.is_object() {
        return Value::Null;
    }
    let config = merge_patch(
        config,
        serde_json::json!({
            "thinking": null,
            "max_tokens": null,
            "output_config": { "effort": null },
            "reasoning": { "effort": null },
            "reasoning_effort": null,
            "generation_config": { "thinking_config": null },
        }),
    );
    // Drop the objects that only held reasoning fields.
    match config {
        Value::Object(fields) => {
            let fields: serde_json::Map<String, Value> = fields
                .into_iter()
                .filter(|(_, value)| value.as_object().is_none_or(|o| !o.is_empty()))
                .collect();
            if fields.is_empty() {
                Value::Null
            } else {
                Value::Object(fields)
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_config::EditFormat;
    use serde_json::json;

    fn legacy(json: Value) -> HashMap<String, ModelConfig> {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn import_moves_reasoning_into_the_default_effort() {
        let selection = ModelSelection::import_legacy(legacy(json!({
            "Claude Opus 5": {
                "provider": "anthropic",
                "id": "claude-opus-5",
                "context_token_limit": 1000000,
                "edit_format": "diff",
                "config": {
                    "max_tokens": 64000,
                    "thinking": { "type": "adaptive", "display": "summarized" },
                    "output_config": { "effort": "medium" }
                }
            },
            "GPT-5.6 Sol (SAP, HAI)": {
                "provider": "hai-proxy-openai",
                "id": "gpt-5.6-sol",
                "context_token_limit": 275000,
                "edit_format": "diff",
                "config": {
                    "temperature": 1.0,
                    "reasoning": { "context": "all_turns", "effort": "high", "summary": "concise" }
                }
            }
        })));

        let opus = selection.get("anthropic/claude-opus-5").unwrap();
        assert_eq!(opus.name.as_deref(), Some("Claude Opus 5"));
        assert_eq!(opus.default_effort, Some(Effort::Medium));
        assert_eq!(opus.config, Value::Null, "only reasoning fields were set");
        assert_eq!(opus.context_token_limit, None, "the catalog knows 1M");
        // Opus 5 is not a diff model in the catalog: the choice stays.
        assert_eq!(opus.edit_format, Some(EditFormat::Diff));

        let gpt = selection.get("hai-proxy-openai/gpt-5.6-sol").unwrap();
        assert_eq!(gpt.default_effort, Some(Effort::High));
        assert_eq!(
            gpt.config,
            json!({ "temperature": 1.0, "reasoning": { "context": "all_turns", "summary": "concise" } })
        );
        assert_eq!(
            gpt.context_token_limit,
            Some(275_000),
            "differs from the catalog"
        );
        assert_eq!(gpt.edit_format, None, "the catalog says diff already");
    }

    #[test]
    fn import_keeps_the_streaming_opt_out_of_a_replaced_tool_config() {
        let selection = ModelSelection::import_legacy(legacy(json!({
            "Gemini 3.5 Flash (HAI)": {
                "provider": "hai-vertex",
                "id": "gemini-3.5-flash",
                "context_token_limit": 1000000,
                "config": { "tool_config": { "function_calling_config": { "mode": "AUTO" } } }
            }
        })));
        assert_eq!(
            selection.models[0].config,
            json!({ "tool_config": { "function_calling_config": {
                "mode": "AUTO", "stream_function_call_arguments": null
            } } })
        );
    }

    #[test]
    fn enabling_and_disabling_models() {
        let mut selection = ModelSelection::default();
        selection.set_enabled("anthropic", "claude-opus-5", true);
        selection.set_enabled("anthropic", "claude-opus-5", true);
        assert_eq!(selection.models.len(), 1);
        selection.set_enabled("anthropic", "claude-opus-5", false);
        assert!(selection.models.is_empty());
    }

    #[test]
    fn round_trips_through_disk_without_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut selection = ModelSelection::default();
        selection.set_enabled("ollama", "qwen3.8:27b-mlx", true);
        selection.save_to(&path).unwrap();

        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(),
            json!({ "models": [{ "provider": "ollama", "id": "qwen3.8:27b-mlx" }] })
        );
        assert_eq!(ModelSelection::load_from(&path).unwrap(), Some(selection));
        assert_eq!(
            ModelSelection::load_from(&dir.path().join("missing.json")).unwrap(),
            None
        );
    }
}
