//! Discovery of project-scoped skills on disk.
//!
//! For the initial slice only the project scope is searched:
//! `<project_root>/.agents/skills/<skill-name>/SKILL.md`. User, system, and
//! bundled scopes are intentionally deferred.

use crate::skills::manifest::parse_skill_content;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

/// The file name that marks a directory as a skill.
const SKILL_FILE: &str = "SKILL.md";

/// A discovered skill with the information needed to advertise and load it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub skill_md: PathBuf,
    /// The skill's directory (parent of `SKILL.md`).
    pub dir: PathBuf,
}

/// Discover project-scoped skills under `<project_root>/.agents/skills/`.
///
/// Each immediate subdirectory containing a `SKILL.md` is parsed. Skills that
/// fail to parse — or whose `name` does not match the directory name — are
/// skipped with a warning rather than failing discovery. Results are sorted by
/// name for deterministic output.
pub fn discover_skills(project_root: &Path) -> Vec<Skill> {
    let root = project_root.join(".agents").join("skills");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        // A missing skills directory is the common case, not an error.
        Err(_) => return Vec::new(),
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Skip hidden directories (e.g. a future `.system` cache).
        let is_hidden = dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(true);
        if is_hidden {
            continue;
        }

        let skill_md = dir.join(SKILL_FILE);
        if !skill_md.is_file() {
            continue;
        }

        match load_skill(&dir, &skill_md) {
            Ok(skill) => skills.push(skill),
            Err(e) => warn!("Skipping skill at {}: {:#}", dir.display(), e),
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn load_skill(dir: &Path, skill_md: &Path) -> Result<Skill> {
    let content = fs::read_to_string(skill_md)?;
    let (manifest, _body) = parse_skill_content(&content)?;

    // Spec: a skill's `name` must match its directory name.
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    anyhow::ensure!(
        dir_name == manifest.name,
        "skill name `{}` does not match its directory name `{}`",
        manifest.name,
        dir_name
    );

    Ok(Skill {
        name: manifest.name,
        description: manifest.description,
        skill_md: skill_md.to_path_buf(),
        dir: dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Write a `SKILL.md` for `name` under `<root>/.agents/skills/<dir_name>/`.
    fn write_skill(root: &Path, dir_name: &str, name: &str, description: &str) {
        let dir = root.join(".agents").join("skills").join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(SKILL_FILE),
            format!("---\nname: {name}\ndescription: {description}\n---\nbody"),
        )
        .unwrap();
    }

    #[test]
    fn returns_empty_when_no_skills_dir() {
        let dir = tempdir().unwrap();
        assert!(discover_skills(dir.path()).is_empty());
    }

    #[test]
    fn discovers_and_sorts_skills() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "zeta", "zeta", "Last skill.");
        write_skill(dir.path(), "alpha", "alpha", "First skill.");

        let skills = discover_skills(dir.path());
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        assert_eq!(skills[0].description, "First skill.");
        assert!(skills[0].skill_md.ends_with("alpha/SKILL.md"));
    }

    #[test]
    fn skips_skill_with_directory_name_mismatch() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "wrong-dir", "actual-name", "Mismatched.");
        write_skill(dir.path(), "good", "good", "Fine.");

        let skills = discover_skills(dir.path());
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["good"]);
    }

    #[test]
    fn skips_directory_without_skill_md() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agents").join("skills").join("empty")).unwrap();
        write_skill(dir.path(), "real", "real", "Fine.");

        let skills = discover_skills(dir.path());
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }
}
