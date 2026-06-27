//! Discovery of skills on disk across scopes.
//!
//! Skills are found under three roots, in precedence order:
//! - **Project**: `<project_root>/.agents/skills/<name>/SKILL.md`
//! - **User**:    `<config_dir>/skills/<name>/SKILL.md`
//! - **System**:  `<config_dir>/skills/.system/<name>/SKILL.md` (bundled)
//!
//! On a name collision the higher-precedence scope wins (project > user >
//! system).

use crate::config::{explorer_for_scope, ProjectManager, SCOPE_CONFIG, SCOPE_SYSTEM};
use crate::skills::config::SkillsConfig;
use crate::skills::manifest::parse_skill_content;
use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

/// The file name that marks a directory as a skill.
const SKILL_FILE: &str = "SKILL.md";

/// The scope a skill was discovered in. Lower [`SkillScope::rank`] wins on
/// name collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    /// `<project_root>/.agents/skills`
    Project,
    /// `<config_dir>/skills` (user-authored, shared across projects)
    User,
    /// `<config_dir>/skills/.system` (bundled with the binary)
    System,
}

impl SkillScope {
    /// Precedence rank; lower wins on name collisions.
    fn rank(self) -> u8 {
        match self {
            SkillScope::Project => 0,
            SkillScope::User => 1,
            SkillScope::System => 2,
        }
    }

    /// Human-readable label used in the catalog (`project`/`user`/`system`).
    pub fn label(self) -> &'static str {
        match self {
            SkillScope::Project => "project",
            SkillScope::User => "user",
            SkillScope::System => "system",
        }
    }
}

/// A discovered skill with the information needed to advertise and load it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub skill_md: PathBuf,
    /// The skill's directory (parent of `SKILL.md`).
    pub dir: PathBuf,
    /// The scope this skill was discovered in.
    pub scope: SkillScope,
}

/// The skills found in a single scope, with the sandbox root `read_files`
/// would use for that scope (resource paths in [`Skill::dir`] are relative to
/// it).
pub struct ScopeSkills {
    pub skills: Vec<Skill>,
    pub sandbox_root: PathBuf,
    pub scope: SkillScope,
}

/// Resolve a scope token to the skills it contains and the sandbox root that
/// `read_files` uses for that scope:
/// - a project name → the project's `.agents/skills`, sandbox root = project root
/// - [`SCOPE_CONFIG`] (`:config:`) → `<config_dir>/skills`
/// - [`SCOPE_SYSTEM`] (`:system:`) → `<config_dir>/skills/.system`
pub fn discover_scope_skills(
    project_manager: &dyn ProjectManager,
    token: &str,
) -> Result<ScopeSkills> {
    discover_scope_skills_filtered(project_manager, token, &SkillsConfig::load())
}

/// Like [`discover_scope_skills`], but using an explicit [`SkillsConfig`] for
/// filtering (master switch, bundled toggle, disabled list).
pub fn discover_scope_skills_filtered(
    project_manager: &dyn ProjectManager,
    token: &str,
    config: &SkillsConfig,
) -> Result<ScopeSkills> {
    let explorer = explorer_for_scope(project_manager, token)?;
    let sandbox_root = explorer.root_dir();
    let (skills_root, scope) = match token {
        SCOPE_CONFIG => (sandbox_root.clone(), SkillScope::User),
        SCOPE_SYSTEM => (sandbox_root.clone(), SkillScope::System),
        _ => (
            sandbox_root.join(".agents").join("skills"),
            SkillScope::Project,
        ),
    };
    let skills = config.filter_skills(discover_skills_in(&skills_root, scope));
    Ok(ScopeSkills {
        skills,
        sandbox_root,
        scope,
    })
}

/// Discover skills across all scopes for `project_root`, applying precedence
/// (project > user > system) on name collisions. Sorted by name.
pub fn discover_all_skills(project_root: &Path) -> Vec<Skill> {
    discover_all_skills_filtered(project_root, &SkillsConfig::load())
}

/// Discover the shared user (`:config:`) and bundled system (`:system:`)
/// skills across both roots, **unfiltered** and deduped by precedence.
///
/// Unlike [`discover_all_skills`], this does not apply [`SkillsConfig`]
/// filtering, so disabled skills are still returned. Intended for settings UIs
/// that need to show every skill alongside its enabled/disabled state.
pub fn discover_config_and_system_skills() -> Vec<Skill> {
    let config_dir = crate::config_dir::config_dir();
    discover_across_roots(&[
        (config_dir.join("skills"), SkillScope::User),
        (
            config_dir.join("skills").join(".system"),
            SkillScope::System,
        ),
    ])
}

/// Like [`discover_all_skills`], but using an explicit [`SkillsConfig`] for
/// filtering (master switch, bundled toggle, disabled list).
pub fn discover_all_skills_filtered(project_root: &Path, config: &SkillsConfig) -> Vec<Skill> {
    let config_dir = crate::config_dir::config_dir();
    let discovered = discover_across_roots(&[
        (
            project_root.join(".agents").join("skills"),
            SkillScope::Project,
        ),
        (config_dir.join("skills"), SkillScope::User),
        (
            config_dir.join("skills").join(".system"),
            SkillScope::System,
        ),
    ]);
    config.filter_skills(discovered)
}

/// Discover skills directly under `skills_root` — each immediate subdirectory
/// containing a `SKILL.md` — tagging them with `scope`. Skills that fail to
/// parse (or whose `name` does not match the directory name) are skipped with
/// a warning. Results are sorted by name.
pub(crate) fn discover_skills_in(skills_root: &Path, scope: SkillScope) -> Vec<Skill> {
    let entries = match fs::read_dir(skills_root) {
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
        // Skip hidden directories (e.g. the `.system` cache under the user root).
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

        match load_skill(&dir, &skill_md, scope) {
            Ok(skill) => skills.push(skill),
            Err(e) => warn!("Skipping skill at {}: {:#}", dir.display(), e),
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Discover skills across the given `(root, scope)` pairs and resolve name
/// collisions by precedence (lowest [`SkillScope::rank`] wins). Sorted by name.
fn discover_across_roots(roots: &[(PathBuf, SkillScope)]) -> Vec<Skill> {
    let mut all = Vec::new();
    for (root, scope) in roots {
        all.extend(discover_skills_in(root, *scope));
    }
    dedupe_by_precedence(all)
}

fn dedupe_by_precedence(skills: Vec<Skill>) -> Vec<Skill> {
    let mut by_name: HashMap<String, Skill> = HashMap::new();
    for skill in skills {
        match by_name.get(&skill.name) {
            // Keep the existing skill when it has equal-or-higher precedence.
            Some(existing) if existing.scope.rank() <= skill.scope.rank() => {}
            _ => {
                by_name.insert(skill.name.clone(), skill);
            }
        }
    }
    let mut result: Vec<Skill> = by_name.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

fn load_skill(dir: &Path, skill_md: &Path, scope: SkillScope) -> Result<Skill> {
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
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Write a `SKILL.md` for `name` under `<skills_root>/<dir_name>/`.
    fn write_skill(skills_root: &Path, dir_name: &str, name: &str, description: &str) {
        let dir = skills_root.join(dir_name);
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
        assert!(discover_skills_in(&dir.path().join("missing"), SkillScope::User).is_empty());
    }

    #[test]
    fn discovers_sorts_and_tags_scope() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "zeta", "zeta", "Last skill.");
        write_skill(dir.path(), "alpha", "alpha", "First skill.");

        let skills = discover_skills_in(dir.path(), SkillScope::User);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        assert_eq!(skills[0].description, "First skill.");
        assert!(skills.iter().all(|s| s.scope == SkillScope::User));
    }

    #[test]
    fn skips_skill_with_directory_name_mismatch() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "wrong-dir", "actual-name", "Mismatched.");
        write_skill(dir.path(), "good", "good", "Fine.");

        let skills = discover_skills_in(dir.path(), SkillScope::Project);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["good"]);
    }

    #[test]
    fn skips_hidden_and_skill_md_less_directories() {
        let dir = tempdir().unwrap();
        // A hidden `.system` cache under the user root must be ignored here.
        write_skill(&dir.path().join(".system"), "hidden", "hidden", "Hidden.");
        fs::create_dir_all(dir.path().join("empty")).unwrap();
        write_skill(dir.path(), "real", "real", "Fine.");

        let skills = discover_skills_in(dir.path(), SkillScope::User);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn higher_precedence_scope_shadows_lower() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        let system = tempdir().unwrap();

        // `shared` exists in all three scopes with distinct descriptions.
        write_skill(project.path(), "shared", "shared", "From project.");
        write_skill(user.path(), "shared", "shared", "From user.");
        write_skill(system.path(), "shared", "shared", "From system.");
        // Plus scope-unique skills.
        write_skill(user.path(), "user-only", "user-only", "User only.");
        write_skill(system.path(), "system-only", "system-only", "System only.");

        let skills = discover_across_roots(&[
            (user.path().to_path_buf(), SkillScope::User),
            (system.path().to_path_buf(), SkillScope::System),
            (project.path().to_path_buf(), SkillScope::Project),
        ]);

        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        // Sorted by name, deduped.
        assert_eq!(names, vec!["shared", "system-only", "user-only"]);

        let shared = skills.iter().find(|s| s.name == "shared").unwrap();
        // Project wins regardless of discovery order.
        assert_eq!(shared.scope, SkillScope::Project);
        assert_eq!(shared.description, "From project.");

        let user_only = skills.iter().find(|s| s.name == "user-only").unwrap();
        assert_eq!(user_only.scope, SkillScope::User);
        let system_only = skills.iter().find(|s| s.name == "system-only").unwrap();
        assert_eq!(system_only.scope, SkillScope::System);
    }

    #[test]
    fn user_shadows_system_when_no_project() {
        let user = tempdir().unwrap();
        let system = tempdir().unwrap();
        write_skill(user.path(), "shared", "shared", "From user.");
        write_skill(system.path(), "shared", "shared", "From system.");

        let skills = discover_across_roots(&[
            (system.path().to_path_buf(), SkillScope::System),
            (user.path().to_path_buf(), SkillScope::User),
        ]);
        let shared = skills.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.scope, SkillScope::User);
    }

    #[test]
    fn discover_scope_skills_filtered_applies_disabled_list() {
        use std::collections::HashMap;

        let dir = tempdir().unwrap();
        let skills_root = dir.path().join(".agents").join("skills");
        write_skill(&skills_root, "alpha", "alpha", "Keep me.");
        write_skill(&skills_root, "beta", "beta", "Hide me.");

        let explorer = crate::mocks::MockExplorer::new(HashMap::new(), None)
            .with_root(dir.path().to_path_buf());
        let pm = crate::mocks::MockProjectManager::default().with_project_path(
            "proj",
            dir.path().to_path_buf(),
            Box::new(explorer),
        );

        let config = SkillsConfig {
            disabled: vec!["beta".to_string()],
            ..Default::default()
        };
        let resolved = discover_scope_skills_filtered(&pm, "proj", &config).unwrap();
        let names: Vec<_> = resolved.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    #[test]
    fn discover_scope_skills_filtered_master_switch_hides_all() {
        use std::collections::HashMap;

        let dir = tempdir().unwrap();
        let skills_root = dir.path().join(".agents").join("skills");
        write_skill(&skills_root, "alpha", "alpha", "Keep me.");

        let explorer = crate::mocks::MockExplorer::new(HashMap::new(), None)
            .with_root(dir.path().to_path_buf());
        let pm = crate::mocks::MockProjectManager::default().with_project_path(
            "proj",
            dir.path().to_path_buf(),
            Box::new(explorer),
        );

        let config = SkillsConfig {
            enabled: false,
            ..Default::default()
        };
        let resolved = discover_scope_skills_filtered(&pm, "proj", &config).unwrap();
        assert!(resolved.skills.is_empty());
    }

    #[test]
    fn discover_scope_skills_resolves_a_project_token() {
        use std::collections::HashMap;

        let dir = tempdir().unwrap();
        write_skill(
            &dir.path().join(".agents").join("skills"),
            "demo",
            "demo",
            "Demo skill.",
        );

        let explorer = crate::mocks::MockExplorer::new(HashMap::new(), None)
            .with_root(dir.path().to_path_buf());
        let pm = crate::mocks::MockProjectManager::default().with_project_path(
            "proj",
            dir.path().to_path_buf(),
            Box::new(explorer),
        );

        let resolved = discover_scope_skills(&pm, "proj").expect("resolves project token");
        assert_eq!(resolved.scope, SkillScope::Project);
        assert_eq!(resolved.sandbox_root, dir.path());
        let names: Vec<_> = resolved.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["demo"]);
    }
}
