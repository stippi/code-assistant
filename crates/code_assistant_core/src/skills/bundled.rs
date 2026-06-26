//! Bundled (system) skills shipped with the binary.
//!
//! The sample skills under `resources/skills/samples/` are embedded at compile
//! time and extracted on startup into `<config_dir>/skills/.system/`, where the
//! `:system:` scope discovers them. Extraction is fingerprint-gated so it only
//! rewrites the tree when the embedded content (or [`SALT`]) changes.

use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use std::fs;
use std::path::Path;

#[derive(RustEmbed)]
#[folder = "resources/skills/samples"]
struct BundledSkills;

/// Name of the fingerprint marker written into the system skills directory.
const FINGERPRINT_FILE: &str = ".fingerprint";

/// Bump to force re-extraction even when embedded file contents are unchanged.
const SALT: &str = "v1";

/// Extract the bundled system skills into `<config_dir>/skills/.system`,
/// skipping the work when the installed fingerprint already matches.
pub fn install_system_skills() -> Result<()> {
    let system_root = crate::config_dir::config_dir()
        .join("skills")
        .join(".system");
    install_system_skills_into(&system_root)
}

/// Testable core of [`install_system_skills`] that targets an explicit root.
fn install_system_skills_into(system_root: &Path) -> Result<()> {
    let fingerprint = compute_fingerprint();
    let fingerprint_path = system_root.join(FINGERPRINT_FILE);

    if let Ok(existing) = fs::read_to_string(&fingerprint_path) {
        if existing.trim() == fingerprint {
            return Ok(());
        }
    }

    // The directory is fully managed by us, so replace it wholesale.
    if system_root.exists() {
        fs::remove_dir_all(system_root)
            .with_context(|| format!("Failed to clear {}", system_root.display()))?;
    }
    fs::create_dir_all(system_root)
        .with_context(|| format!("Failed to create {}", system_root.display()))?;

    for path in BundledSkills::iter() {
        let file = BundledSkills::get(&path)
            .ok_or_else(|| anyhow::anyhow!("Embedded skill file vanished: {path}"))?;
        let dest = system_root.join(path.as_ref());
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        fs::write(&dest, file.data.as_ref())
            .with_context(|| format!("Failed to write {}", dest.display()))?;
    }

    fs::write(&fingerprint_path, &fingerprint)
        .with_context(|| format!("Failed to write {}", fingerprint_path.display()))?;
    Ok(())
}

/// A stable fingerprint of the embedded skill tree (plus [`SALT`]).
fn compute_fingerprint() -> String {
    let mut paths: Vec<String> = BundledSkills::iter().map(|p| p.to_string()).collect();
    paths.sort();

    let mut input = String::from(SALT);
    for path in paths {
        if let Some(file) = BundledSkills::get(&path) {
            input.push('\n');
            input.push_str(&path);
            input.push(':');
            input.push_str(&format!("{:x}", md5::compute(file.data.as_ref())));
        }
    }
    format!("{:x}", md5::compute(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::loader::discover_skills_in;
    use crate::skills::SkillScope;
    use tempfile::tempdir;

    #[test]
    fn embeds_the_skill_creator() {
        // Guards against an accidental empty/renamed resource folder.
        assert!(
            BundledSkills::get("skill-creator/SKILL.md").is_some(),
            "skill-creator must be embedded"
        );
    }

    #[test]
    fn installs_then_is_idempotent_and_discoverable() {
        let dir = tempdir().unwrap();
        let system_root = dir.path().join(".system");

        install_system_skills_into(&system_root).unwrap();
        let skill_md = system_root.join("skill-creator").join("SKILL.md");
        assert!(skill_md.is_file());
        assert!(system_root.join(FINGERPRINT_FILE).is_file());

        // The extracted tree is discoverable as a system-scoped skill.
        let skills = discover_skills_in(&system_root, SkillScope::System);
        assert!(skills.iter().any(|s| s.name == "skill-creator"));
        assert!(skills.iter().all(|s| s.scope == SkillScope::System));

        // A second run is a no-op (fingerprint matches) and still succeeds.
        install_system_skills_into(&system_root).unwrap();
        assert!(skill_md.is_file());
    }

    #[test]
    fn reextracts_when_fingerprint_differs() {
        let dir = tempdir().unwrap();
        let system_root = dir.path().join(".system");
        install_system_skills_into(&system_root).unwrap();

        let skill_md = system_root.join("skill-creator").join("SKILL.md");
        // Tamper with the installed content and stale the fingerprint.
        fs::write(&skill_md, "corrupted").unwrap();
        fs::write(system_root.join(FINGERPRINT_FILE), "stale").unwrap();

        install_system_skills_into(&system_root).unwrap();

        let restored = fs::read_to_string(&skill_md).unwrap();
        assert!(restored.contains("name: skill-creator"));
    }
}
