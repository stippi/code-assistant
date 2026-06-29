//! Discovery of skills on disk across scopes.
//!
//! Skills are found under three roots, in precedence order:
//! - **Project**: `<project_root>/.agents/skills/<name>/SKILL.md`
//! - **User**:    `~/.agents/skills/<name>/SKILL.md` (shared across harnesses)
//! - **System**:  `<config_dir>/skills/.system/<name>/SKILL.md` (bundled)
//!
//! On a name collision the higher-precedence scope wins (project > user >
//! system).

use crate::config::{
    explorer_for_scope, system_skills_root, user_skills_root, ProjectManager, SCOPE_CONFIG,
    SCOPE_SYSTEM,
};
use crate::skills::config::SkillsConfig;
use crate::skills::manifest::parse_skill_content;

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Deepest a `SKILL.md`-containing directory may sit below a skills root. A
/// skill normally lives at depth 1 (a direct child of the root); we tolerate a
/// few levels of organizational nesting (e.g. `<root>/category/<name>/`).
const MAX_SKILL_DEPTH: usize = 4;

/// Upper bound on directories visited per root, guarding against pathological
/// trees (and symlink cycles, alongside the depth cap).
const MAX_DIRS_PER_ROOT: usize = 2000;

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
    /// When `true`, the skill is hidden from the model-facing catalog and the
    /// `list_skills` tool (the model must not auto-invoke it), but it remains
    /// loadable via `read_skill` and visible in the settings UI. Maps to the
    /// `disable-model-invocation` frontmatter field.
    pub disable_model_invocation: bool,
}

impl Skill {
    /// Whether this skill should be advertised to the model (system-prompt
    /// catalog and the `list_skills` tool). Skills with
    /// `disable-model-invocation: true` are hidden from the model but stay
    /// loadable via `read_skill`.
    pub fn is_model_invocable(&self) -> bool {
        !self.disable_model_invocation
    }
}

/// Filter a slice of skills down to those that may be advertised to the model,
/// preserving order. Skills flagged `disable-model-invocation` are dropped.
pub fn model_invocable(skills: &[Skill]) -> Vec<Skill> {
    skills
        .iter()
        .filter(|s| s.is_model_invocable())
        .cloned()
        .collect()
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
/// - [`SCOPE_CONFIG`] (`:config:`) → `~/.agents/skills`
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

/// Discover the skills available to a session as `(skill, scope_token)` pairs,
/// where `scope_token` is what callers pass back to load the skill (the
/// `project_name`, or `:config:` / `:system:`). Deduped across scopes with the
/// same precedence as the catalog (project > user > system) and sorted by name.
///
/// Unlike [`discover_all_skills`] (which takes a path), this resolves each
/// scope through the `project_manager`, so it agrees with how `read_skill` and
/// the invocation path resolve scope tokens. Includes skills flagged
/// `disable-model-invocation`, since a user may explicitly invoke any of them.
pub fn discover_session_catalog(
    project_manager: &dyn ProjectManager,
    project_name: &str,
    config: &SkillsConfig,
) -> Vec<(Skill, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(Skill, String)> = Vec::new();
    for token in [project_name, SCOPE_CONFIG, SCOPE_SYSTEM] {
        let resolved = match discover_scope_skills_filtered(project_manager, token, config) {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        for skill in resolved.skills {
            // First scope to define a name wins (project > user > system).
            if seen.insert(skill.name.clone()) {
                out.push((skill, token.to_string()));
            }
        }
    }
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
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
        (user_skills_root(), SkillScope::User),
        (system_skills_root(&config_dir), SkillScope::System),
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
        (user_skills_root(), SkillScope::User),
        (system_skills_root(&config_dir), SkillScope::System),
    ]);
    config.filter_skills(discovered)
}

/// Discover skills under `skills_root`, tagging them with `scope`. A directory
/// is a skill when it contains a `SKILL.md`; discovery recurses through
/// non-skill directories (bounded by [`MAX_SKILL_DEPTH`] and
/// [`MAX_DIRS_PER_ROOT`]) so skills may be organized one or more levels deep
/// (e.g. `<root>/category/<name>/SKILL.md`).
///
/// Once a `SKILL.md` is found, that subtree is not descended into further (a
/// skill's bundled `references/`, `scripts/`, etc. may themselves contain a
/// `SKILL.md` that is not a separate skill). Hidden directories (e.g. the
/// `.system` cache under the user root) are skipped at every level. Skills that
/// fail to parse (or whose `name` does not match the directory name) are
/// skipped with a warning. Results are sorted by name.
pub(crate) fn discover_skills_in(skills_root: &Path, scope: SkillScope) -> Vec<Skill> {
    let mut skills = Vec::new();
    let mut visited = 0usize;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();

    // Seed with the immediate children of the root (depth 1).
    enqueue_subdirs(skills_root, 1, &mut queue);

    while let Some((dir, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_DIRS_PER_ROOT {
            warn!(
                "Reached the skill scan cap ({}) under {}; some skills may be skipped",
                MAX_DIRS_PER_ROOT,
                skills_root.display()
            );
            break;
        }

        // Skip hidden directories at any level (e.g. the `.system` cache under
        // the user root, or VCS/metadata dirs nested under an org folder).
        if is_hidden_dir(&dir) {
            continue;
        }

        let skill_md = dir.join(SKILL_FILE);
        if skill_md.is_file() {
            match load_skill(&dir, &skill_md, scope) {
                Ok(skill) => skills.push(skill),
                Err(e) => warn!("Skipping skill at {}: {:#}", dir.display(), e),
            }
            // Do not descend into a skill's own subtree.
            continue;
        }

        // Not a skill directory: descend, bounded by depth.
        if depth < MAX_SKILL_DEPTH {
            enqueue_subdirs(&dir, depth + 1, &mut queue);
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Whether `dir`'s file name starts with a dot (or cannot be read as UTF-8).
fn is_hidden_dir(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(true)
}

/// Push every immediate subdirectory of `parent` onto `queue` at `depth`.
/// A missing/unreadable directory is treated as empty.
fn enqueue_subdirs(parent: &Path, depth: usize, queue: &mut VecDeque<(PathBuf, usize)>) {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            queue.push_back((path, depth));
        }
    }
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
        disable_model_invocation: manifest.disable_model_invocation,
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
    fn discovers_nested_skill_one_level_deep() {
        let dir = tempdir().unwrap();
        // <root>/category/my-skill/SKILL.md — one organizational level deep.
        write_skill(
            &dir.path().join("category"),
            "my-skill",
            "my-skill",
            "Nested.",
        );

        let skills = discover_skills_in(dir.path(), SkillScope::Project);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["my-skill"]);
    }

    #[test]
    fn does_not_descend_into_a_skill_subtree() {
        let dir = tempdir().unwrap();
        // A real skill ...
        write_skill(dir.path(), "outer", "outer", "Outer skill.");
        // ... whose bundled resources happen to contain another SKILL.md must
        // not surface as a second skill.
        let nested = dir.path().join("outer").join("references").join("inner");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join(SKILL_FILE),
            "---\nname: inner\ndescription: Not a real skill.\n---\nbody",
        )
        .unwrap();

        let skills = discover_skills_in(dir.path(), SkillScope::Project);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["outer"]);
    }

    #[test]
    fn respects_max_scan_depth() {
        let dir = tempdir().unwrap();
        // Depth 4 (a/b/c/<skill>) is discovered; depth 5 is not.
        write_skill(
            &dir.path().join("a").join("b").join("c"),
            "deep-ok",
            "deep-ok",
            "Reachable.",
        );
        write_skill(
            &dir.path().join("a2").join("b").join("c").join("d"),
            "too-deep",
            "too-deep",
            "Beyond the depth cap.",
        );

        let skills = discover_skills_in(dir.path(), SkillScope::Project);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["deep-ok"]);
    }

    #[test]
    fn skips_hidden_directories_when_recursing() {
        let dir = tempdir().unwrap();
        // A skill inside a hidden organizational directory is skipped at depth.
        write_skill(&dir.path().join(".hidden"), "ghost", "ghost", "Hidden.");
        write_skill(dir.path(), "real", "real", "Fine.");

        let skills = discover_skills_in(dir.path(), SkillScope::Project);
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
