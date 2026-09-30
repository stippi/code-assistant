//! Explicit skill invocation from a user message: `/<skill-name> <request>`.
//!
//! When a user message starts with `/<name>` and `<name>` is a skill of the
//! session, the message is sent as typed (so the transcript shows what the
//! user wrote) and the skill's body is appended to the same user message as a
//! `<skill>…</skill>` text block. The model sees the request together with the
//! skill's instructions in one turn; the UIs hide the injected block.

use crate::config::ProjectManager;
use crate::skills::config::SkillsConfig;
use crate::skills::invoke::{SkillPayload, load_skill_payload, render_skill_body_with_header};
use crate::skills::loader::discover_session_catalog;
use anyhow::Result;

/// Split a message into a leading `/<token>` and the text after it, if the
/// message starts with one. Whether the token names a skill is up to the
/// caller.
pub fn parse_skill_trigger(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (name, request) = rest.split_at(end);
    (!name.is_empty()).then(|| (name, request.trim()))
}

/// Resolve the skill a user message invokes: `None` when the message does not
/// start with `/<name>` of a skill in the session's catalog, otherwise the
/// loaded skill (or the error loading it).
pub fn resolve_skill_trigger(
    project_manager: &dyn ProjectManager,
    project_name: &str,
    config: &SkillsConfig,
    text: &str,
) -> Option<Result<SkillPayload>> {
    if !config.enabled {
        return None;
    }
    let (name, _) = parse_skill_trigger(text)?;
    let (skill, scope_token) = discover_session_catalog(project_manager, project_name, config)
        .into_iter()
        .find(|(skill, _)| skill.name == name)?;
    Some(load_skill_payload(
        project_manager,
        &scope_token,
        &skill.name,
        config,
    ))
}

/// Render the text block appended to the user message that invokes a skill.
pub fn render_skill_injection(payload: &SkillPayload) -> String {
    crate::injection::wrap(
        "skill",
        &format!(
            "<name>{name}</name>\n\
             The user invoked the **{name}** skill with `/{name}` in the message above. Its full \
             instructions follow — apply them to the user's request. You do not need to call \
             `read_skill` for this skill again.\n\n\
             {body}",
            name = payload.name,
            body = render_skill_body_with_header(payload),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{MockExplorer, MockProjectManager};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn payload(name: &str) -> SkillPayload {
        SkillPayload {
            scope_token: "proj".to_string(),
            scope_label: "project".to_string(),
            name: name.to_string(),
            dir: PathBuf::from(format!(".agents/skills/{name}")),
            body: "Do the thing.".to_string(),
        }
    }

    fn pm_with_skill(root: &std::path::Path, name: &str) -> MockProjectManager {
        let dir = root.join(".agents").join("skills").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Demo.\n---\nStep 1."),
        )
        .unwrap();
        let explorer = MockExplorer::new(HashMap::new(), None).with_root(root.to_path_buf());
        MockProjectManager::default().with_project_path(
            "proj",
            root.to_path_buf(),
            Box::new(explorer),
        )
    }

    #[test]
    fn parses_trigger_and_request() {
        assert_eq!(
            parse_skill_trigger("/pdf-extraction extract report.pdf"),
            Some(("pdf-extraction", "extract report.pdf"))
        );
        assert_eq!(
            parse_skill_trigger("  /review\nfocus on auth\n"),
            Some(("review", "focus on auth"))
        );
        assert_eq!(parse_skill_trigger("/review"), Some(("review", "")));
    }

    #[test]
    fn ignores_messages_without_leading_trigger() {
        assert_eq!(parse_skill_trigger("please /review this"), None);
        assert_eq!(parse_skill_trigger("/"), None);
        assert_eq!(parse_skill_trigger("/ review"), None);
        assert_eq!(parse_skill_trigger(""), None);
    }

    #[test]
    fn resolves_known_skill_with_request() {
        let dir = tempdir().unwrap();
        let pm = pm_with_skill(dir.path(), "demo");
        let payload = resolve_skill_trigger(
            &pm,
            "proj",
            &SkillsConfig::default(),
            "/demo do it for foo.rs",
        )
        .expect("triggers")
        .expect("loads");
        assert_eq!(payload.name, "demo");
        assert_eq!(payload.body, "Step 1.");
    }

    #[test]
    fn unknown_or_disabled_trigger_is_an_ordinary_message() {
        let dir = tempdir().unwrap();
        let pm = pm_with_skill(dir.path(), "demo");
        let config = SkillsConfig::default();
        assert!(resolve_skill_trigger(&pm, "proj", &config, "/other do it").is_none());
        assert!(resolve_skill_trigger(&pm, "proj", &config, "demo do it").is_none());
        let disabled = SkillsConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(resolve_skill_trigger(&pm, "proj", &disabled, "/demo").is_none());
    }

    #[test]
    fn injection_wraps_body_and_is_recognized() {
        let injection = render_skill_injection(&payload("review"));
        assert!(injection.starts_with("<skill>\n<name>review</name>\n"));
        assert!(injection.contains("# Skill: review (project)"));
        assert!(injection.contains("Do the thing."));
        assert!(injection.ends_with("</skill>"));
        assert!(crate::injection::is_injection(&injection));
    }
}
