//! Configuration for the skills feature, persisted at
//! `<config_dir>/skills.json`.
//!
//! ```json
//! {
//!   "enabled": true,
//!   "bundled_skills_enabled": true,
//!   "disabled": ["legacy-skill", "/abs/path/to/SKILL.md"]
//! }
//! ```
//!
//! - `enabled` (default `true`): master switch. When false, the catalog is not
//!   rendered into the system prompt and the skill tools return an explanatory
//!   error.
//! - `bundled_skills_enabled` (default `true`): when false, bundled system
//!   skills are not extracted (and any previously-extracted ones are removed).
//! - `disabled`: skill names *or* absolute `SKILL.md` paths to filter out
//!   before catalog rendering and tool resolution.

use crate::skills::loader::Skill;
use crate::utils::file_utils::atomic_write_json;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

/// User-facing configuration for the skills subsystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Master switch for the entire skills feature.
    pub enabled: bool,
    /// Whether bundled (system) skills are extracted and advertised.
    pub bundled_skills_enabled: bool,
    /// Skill names or absolute `SKILL.md` paths to hide.
    pub disabled: Vec<String>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bundled_skills_enabled: true,
            disabled: Vec::new(),
        }
    }
}

/// Path to the skills config file inside `config_dir`.
pub fn skills_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("skills.json")
}

/// Path to the skills config file in the resolved config directory.
pub fn skills_config_path() -> PathBuf {
    skills_config_path_in(&crate::config_dir::config_dir())
}

impl SkillsConfig {
    /// Load the config from the resolved config directory. A missing or
    /// malformed file yields [`SkillsConfig::default`] (best-effort).
    pub fn load() -> Self {
        Self::load_from(&skills_config_path())
    }

    /// Load the config from an explicit path. A missing file is the common
    /// case and yields defaults; a malformed file logs a warning and also
    /// yields defaults so a broken config never disables the feature silently
    /// in a confusing way.
    pub fn load_from(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str(&content) {
            Ok(config) => config,
            Err(e) => {
                warn!("Failed to parse {}: {e}; using defaults", path.display());
                Self::default()
            }
        }
    }

    /// Persist the config to the resolved config directory.
    pub fn save(&self) -> Result<()> {
        self.save_to(&skills_config_path())
    }

    /// Persist the config to an explicit path (atomic write).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }

    /// Whether the given skill is hidden by the `disabled` list. A disabled
    /// entry matches either the skill's `name` or the absolute path to its
    /// `SKILL.md`.
    pub fn is_skill_disabled(&self, skill: &Skill) -> bool {
        self.disabled
            .iter()
            .any(|entry| entry == &skill.name || Path::new(entry) == skill.skill_md.as_path())
    }

    /// Filter a list of discovered skills by configuration, consuming the
    /// input:
    /// - when the feature is disabled altogether, the result is empty;
    /// - when bundled skills are disabled, `system`-scoped skills are dropped;
    /// - skills named (or located) in `disabled` are dropped.
    pub fn filter_skills(&self, skills: Vec<Skill>) -> Vec<Skill> {
        if !self.enabled {
            return Vec::new();
        }
        skills
            .into_iter()
            .filter(|s| self.bundled_skills_enabled || s.scope != crate::skills::SkillScope::System)
            .filter(|s| !self.is_skill_disabled(s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillScope;
    use tempfile::tempdir;

    fn skill(name: &str, skill_md: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: "desc".to_string(),
            skill_md: PathBuf::from(skill_md),
            dir: PathBuf::from(skill_md).parent().unwrap().to_path_buf(),
            scope: SkillScope::Project,
        }
    }

    #[test]
    fn default_enables_everything() {
        let config = SkillsConfig::default();
        assert!(config.enabled);
        assert!(config.bundled_skills_enabled);
        assert!(config.disabled.is_empty());
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempdir().unwrap();
        let config = SkillsConfig::load_from(&dir.path().join("skills.json"));
        assert_eq!(config, SkillsConfig::default());
    }

    #[test]
    fn malformed_file_yields_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("skills.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        assert_eq!(SkillsConfig::load_from(&path), SkillsConfig::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("skills.json");
        std::fs::write(&path, r#"{ "bundled_skills_enabled": false }"#).unwrap();
        let config = SkillsConfig::load_from(&path);
        assert!(config.enabled);
        assert!(!config.bundled_skills_enabled);
        assert!(config.disabled.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("skills.json");
        let config = SkillsConfig {
            enabled: false,
            bundled_skills_enabled: false,
            disabled: vec!["legacy".to_string(), "/abs/SKILL.md".to_string()],
        };
        config.save_to(&path).unwrap();
        assert_eq!(SkillsConfig::load_from(&path), config);
    }

    #[test]
    fn disabled_matches_by_name() {
        let config = SkillsConfig {
            disabled: vec!["alpha".to_string()],
            ..Default::default()
        };
        assert!(config.is_skill_disabled(&skill("alpha", "/skills/alpha/SKILL.md")));
        assert!(!config.is_skill_disabled(&skill("beta", "/skills/beta/SKILL.md")));
    }

    #[test]
    fn disabled_matches_by_absolute_path() {
        let config = SkillsConfig {
            disabled: vec!["/skills/alpha/SKILL.md".to_string()],
            ..Default::default()
        };
        assert!(config.is_skill_disabled(&skill("alpha", "/skills/alpha/SKILL.md")));
        // Same name, different path is not matched by the path entry.
        assert!(!config.is_skill_disabled(&skill("alpha", "/other/alpha/SKILL.md")));
    }

    #[test]
    fn filter_skills_removes_disabled() {
        let config = SkillsConfig {
            disabled: vec!["beta".to_string()],
            ..Default::default()
        };
        let skills = vec![
            skill("alpha", "/s/alpha/SKILL.md"),
            skill("beta", "/s/beta/SKILL.md"),
            skill("gamma", "/s/gamma/SKILL.md"),
        ];
        let kept: Vec<_> = config
            .filter_skills(skills)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(kept, vec!["alpha".to_string(), "gamma".to_string()]);
    }

    #[test]
    fn filter_skills_drops_system_when_bundled_disabled() {
        let config = SkillsConfig {
            bundled_skills_enabled: false,
            ..Default::default()
        };
        let mut system_skill = skill("bundled", "/s/bundled/SKILL.md");
        system_skill.scope = SkillScope::System;
        let skills = vec![skill("project-one", "/s/p/SKILL.md"), system_skill];
        let kept: Vec<_> = config
            .filter_skills(skills)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(kept, vec!["project-one".to_string()]);
    }

    #[test]
    fn filter_skills_empty_when_feature_disabled() {
        let config = SkillsConfig {
            enabled: false,
            ..Default::default()
        };
        let skills = vec![skill("alpha", "/s/alpha/SKILL.md")];
        assert!(config.filter_skills(skills).is_empty());
    }
}
