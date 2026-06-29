//! Loading a skill's body for invocation, shared by the `read_skill` tool and
//! the user-initiated ("explicit") invocation path.
//!
//! Two consumers need the same resolution + read + truncation logic:
//! - the `read_skill` tool (model-initiated, progressive disclosure), and
//! - the UI invocation path (terminal `/skill`, GPUI completion, ACP command),
//!   which injects the body directly as a synthetic user message so no extra
//!   model round-trip is needed.

use crate::config::ProjectManager;
use crate::skills::config::SkillsConfig;
use crate::skills::loader::discover_scope_skills_filtered;
use crate::skills::manifest::parse_skill_content;
use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// Cap on the returned skill body to keep context size reasonable.
pub const MAX_BODY_LEN: usize = 64 * 1024;

/// A resolved, read-from-disk skill body plus the metadata needed to render it
/// and to address its bundled resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPayload {
    /// The scope token the skill was loaded from (a project name, or
    /// `:config:` / `:system:`) — pass it back to `read_files` for resources.
    pub scope_token: String,
    /// Human-readable scope label (`project` / `user` / `system`).
    pub scope_label: String,
    pub name: String,
    /// The skill's directory, relative to the scope's sandbox root, so the
    /// paths referenced in the body resolve directly with `read_files`.
    pub dir: PathBuf,
    /// The skill body (possibly truncated to [`MAX_BODY_LEN`]).
    pub body: String,
}

/// Resolve `name` in `scope_token` (a project name, or `:config:` / `:system:`)
/// and read its `SKILL.md` body. Honors the skills master switch and the
/// disabled list, but **not** `disable-model-invocation` — a user may load any
/// discoverable skill explicitly.
pub fn load_skill_payload(
    project_manager: &dyn ProjectManager,
    scope_token: &str,
    name: &str,
    config: &SkillsConfig,
) -> Result<SkillPayload> {
    if !config.enabled {
        return Err(anyhow!(
            "Skills are disabled in this configuration (skills.json). \
             Enable them to load skills."
        ));
    }

    let resolved = discover_scope_skills_filtered(project_manager, scope_token, config)
        .map_err(|e| anyhow!("Failed to resolve scope {}: {}", scope_token, e))?;

    let skill = resolved
        .skills
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            anyhow!(
                "No skill named `{}` was found in scope `{}`",
                name,
                scope_token
            )
        })?;

    // Express the skill directory relative to the scope's sandbox root so the
    // body's relative resource references resolve directly via read_files with
    // the same scope token.
    let dir = skill
        .dir
        .strip_prefix(&resolved.sandbox_root)
        .unwrap_or(skill.dir.as_path())
        .to_path_buf();

    let content = std::fs::read_to_string(&skill.skill_md)
        .map_err(|e| anyhow!("Failed to read skill `{}`: {}", name, e))?;
    let (_manifest, mut body) = parse_skill_content(&content)?;

    if body.len() > MAX_BODY_LEN {
        // Truncate on a char boundary to avoid panicking on multi-byte text.
        let mut end = MAX_BODY_LEN;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
        body.push_str("\n\n[... skill truncated to keep context size reasonable ...]");
    }

    Ok(SkillPayload {
        scope_token: scope_token.to_string(),
        scope_label: resolved.scope.label().to_string(),
        name: skill.name,
        dir,
        body,
    })
}

/// Render the skill body with the standard header describing its scope and how
/// to reach its bundled resources. This is the verbatim text the `read_skill`
/// tool returns, and the core of the synthetic invocation message.
pub fn render_skill_body_with_header(payload: &SkillPayload) -> String {
    let dir = payload.dir.to_string_lossy().replace('\\', "/");
    format!(
        "# Skill: {name} ({scope_label})\n\n\
         Bundled resources live under `{dir}/`; the paths referenced below are relative to that \
         directory. Read a resource with `read_files` (project `{scope}`, path \
         `{dir}/<resource>`) or run a bundled script with `execute_command`.\n\n\
         ---\n\n\
         {body}",
        name = payload.name,
        scope_label = payload.scope_label,
        dir = dir,
        scope = payload.scope_token,
        body = payload.body,
    )
}

/// Render the synthetic user message used when a user explicitly activates a
/// skill from the UI. The full body is embedded inline so the model can act on
/// it immediately, without a `read_skill` round-trip.
pub fn render_skill_invocation_message(payload: &SkillPayload) -> String {
    format!(
        "The user activated the **{name}** skill. Its full instructions are included below — \
         follow them for the current task. You do not need to call `read_skill` for this skill \
         again.\n\n\
         {body}\n\n\
         ---\n\n\
         Apply this skill to the user's request. Explore the skill's bundled resources (as \
         described above) only as needed.",
        name = payload.name,
        body = render_skill_body_with_header(payload),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{MockExplorer, MockProjectManager};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    fn pm_for_root(project: &str, root: &std::path::Path) -> MockProjectManager {
        let explorer = MockExplorer::new(HashMap::new(), None).with_root(root.to_path_buf());
        MockProjectManager::default().with_project_path(
            project,
            root.to_path_buf(),
            Box::new(explorer),
        )
    }

    fn write_skill(root: &std::path::Path, name: &str, description: &str, body: &str) {
        let dir = root.join(".agents").join("skills").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn loads_payload_with_relative_dir() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "demo", "Demo skill.", "Step 1.");
        let pm = pm_for_root("proj", dir.path());

        let payload =
            load_skill_payload(&pm, "proj", "demo", &SkillsConfig::default()).expect("loads");
        assert_eq!(payload.name, "demo");
        assert_eq!(payload.scope_token, "proj");
        assert_eq!(payload.scope_label, "project");
        assert_eq!(payload.dir, PathBuf::from(".agents/skills/demo"));
        assert_eq!(payload.body, "Step 1.");
    }

    #[test]
    fn errors_when_missing() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "demo", "Demo.", "body");
        let pm = pm_for_root("proj", dir.path());

        let err = load_skill_payload(&pm, "proj", "nope", &SkillsConfig::default()).unwrap_err();
        assert!(err.to_string().contains("No skill named"));
    }

    #[test]
    fn errors_when_disabled() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "demo", "Demo.", "body");
        let pm = pm_for_root("proj", dir.path());
        let config = SkillsConfig {
            enabled: false,
            ..Default::default()
        };
        let err = load_skill_payload(&pm, "proj", "demo", &config).unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[test]
    fn loads_model_invocation_disabled_skill() {
        // Explicit user invocation may load a skill the model can't auto-invoke.
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join(".agents").join("skills").join("internal");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: internal\ndescription: Hidden.\ndisable-model-invocation: true\n---\nBody.",
        )
        .unwrap();
        let pm = pm_for_root("proj", dir.path());

        let payload =
            load_skill_payload(&pm, "proj", "internal", &SkillsConfig::default()).expect("loads");
        assert_eq!(payload.name, "internal");
        assert_eq!(payload.body, "Body.");
    }

    #[test]
    fn body_header_mentions_scope_and_resources() {
        let payload = SkillPayload {
            scope_token: "proj".to_string(),
            scope_label: "project".to_string(),
            name: "demo".to_string(),
            dir: PathBuf::from(".agents/skills/demo"),
            body: "Do the thing.".to_string(),
        };
        let rendered = render_skill_body_with_header(&payload);
        assert!(rendered.contains("# Skill: demo (project)"));
        assert!(rendered.contains(".agents/skills/demo/"));
        assert!(rendered.contains("project `proj`"));
        assert!(rendered.contains("Do the thing."));
    }

    #[test]
    fn invocation_message_embeds_body_and_framing() {
        let payload = SkillPayload {
            scope_token: ":config:".to_string(),
            scope_label: "user".to_string(),
            name: "security-review".to_string(),
            dir: PathBuf::from("security-review"),
            body: "Audit auth.".to_string(),
        };
        let rendered = render_skill_invocation_message(&payload);
        assert!(rendered.contains("activated the **security-review** skill"));
        assert!(rendered.contains("# Skill: security-review (user)"));
        assert!(rendered.contains("Audit auth."));
        // Tells the model not to call read_skill again.
        assert!(rendered.contains("read_skill"));
    }
}
