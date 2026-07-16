//! System prompt construction: model-specific base prompt, project file
//! trees, durable goals, and repository guidance (AGENTS.md / CLAUDE.md).

use crate::plugins::AgentAppState;
use crate::tool_dialects::system_message::generate_system_message;
use agent_core::hooks::{PromptCtx, SystemPromptProvider};
use agent_orchestration::goals::GoalStore;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

pub struct CodeAssistantSystemPrompt {
    goals_path: PathBuf,
}

impl Default for CodeAssistantSystemPrompt {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeAssistantSystemPrompt {
    pub fn new() -> Self {
        Self::with_goals_path(crate::goals::default_goals_path())
    }

    /// Provider reading goals from a custom store path (tests, embedders).
    pub fn with_goals_path(goals_path: impl Into<PathBuf>) -> Self {
        Self {
            goals_path: goals_path.into(),
        }
    }
}

impl SystemPromptProvider for CodeAssistantSystemPrompt {
    fn build(&self, ctx: &PromptCtx) -> String {
        let state = AgentAppState::of_ref(ctx.extensions);
        let initial_project = state.session_config.initial_project.as_str();

        // Generate the system message using the tools module
        let mut system_message =
            generate_system_message(ctx.dialect, state.tool_scope, ctx.model_hint, ctx.registry);

        // Add project information
        let mut project_info = String::new();

        // Add information about the initial project if available
        if !initial_project.is_empty() {
            project_info.push_str("\n\n# Project Information\n\n");
            project_info.push_str(&format!("## Initial Project: {initial_project}\n\n"));

            // Add file tree for the initial project if available
            if let Some(tree) = state.file_trees.get(initial_project) {
                project_info.push_str("### File Structure:\n");
                project_info.push_str(&format!("```\n{tree}\n```\n\n"));
            }
        }

        // Add information about available projects
        if !state.available_projects.is_empty() {
            project_info.push_str("## Available Projects:\n");
            for project in &state.available_projects {
                project_info.push_str(&format!("- {project}\n"));
            }
        }
        // Append project information to base prompt if available
        if !project_info.is_empty() {
            system_message = format!("{system_message}\n{project_info}");
        }

        // Append the skills catalog (progressive disclosure: metadata only;
        // bodies load on demand via `read_skill`). Covers the initial project's
        // skills plus the shared user (`:config:`) and bundled system
        // (`:system:`) scopes.
        if let Some(project_root) = state.session_config.effective_project_path() {
            let skills = crate::skills::discover_all_skills(project_root);
            if let Some(section) = crate::skills::render_skills_section(initial_project, &skills) {
                system_message.push_str("\n\n");
                system_message.push_str(&section);
            }
        }

        // Surface the session's durable goals, so a reloaded session knows
        // what it is still committed to (the goal controller keeps driving
        // them while the app is open).
        if let Some(section) = ctx.session_id.and_then(|id| self.render_goals_section(id)) {
            system_message.push_str("\n\n");
            system_message.push_str(&section);
        }

        // Append guidance files if present. Global AGENTS.md is loaded first so
        // project-specific guidance can refine or override it in the prompt.
        let guidance_files = read_guidance_files(
            state
                .session_config
                .effective_project_path()
                .map(|p| p.as_path()),
        );
        if !guidance_files.is_empty() {
            let mut guidance_section = String::new();
            guidance_section.push_str("\n\n# Repository Guidance\n");

            for (file_name, content) in guidance_files {
                guidance_section.push('\n');
                guidance_section.push_str(&format!("Loaded from `{file_name}`.\n\n"));
                guidance_section.push_str(&content);
                guidance_section.push('\n');
            }

            system_message.push_str(&guidance_section);
        }

        system_message
    }
}

impl CodeAssistantSystemPrompt {
    /// The `# Durable Goals` block for this session's active goals; `None`
    /// when there are none (or the store is unreadable — the prompt must not
    /// fail over an optional block).
    fn render_goals_section(&self, session_id: &str) -> Option<String> {
        let goals = GoalStore::new(&self.goals_path)
            .active_for_owner(&crate::goals::session_owner(session_id))
            .map_err(|error| warn!("failed to read goals for the system prompt: {error:#}"))
            .ok()?;
        if goals.is_empty() {
            return None;
        }
        let mut section = String::from(
            "# Durable Goals\n\nThis session is committed to the following goal(s). They are \
             pursued autonomously while the app is open and survive session reloads; manage \
             them with the `goal` tool.\n",
        );
        for goal in goals {
            let note = goal
                .note
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default();
            section.push_str(&format!(
                "- {} [{}] {}/{} turns: {}{}\n",
                goal.id,
                goal.state.label(),
                goal.turns_used(),
                goal.budget.max_turns,
                goal.objective,
                note,
            ));
        }
        Some(section.trim_end().to_string())
    }
}

/// Attempt to read guidance from the global config directory and project root.
///
/// Global `~/.config/code-assistant/AGENTS.md` is included when present.
/// Project-root guidance preserves the existing behavior: AGENTS.md is preferred
/// over CLAUDE.md and matching is case-insensitive.
fn read_guidance_files(project_root: Option<&Path>) -> Vec<(String, String)> {
    let mut guidance_files = Vec::new();

    let config_dir = crate::config_dir::config_dir();
    if let Some((_, content)) = read_guidance_from_dir(&config_dir, &["AGENTS.md"]) {
        let label = format!("{}/AGENTS.md", config_dir.display());
        guidance_files.push((label, content));
    }

    let root_path = project_root
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());

    if let Some(root_path) = root_path {
        if let Some(guidance) = read_guidance_from_dir(&root_path, &["AGENTS.md", "CLAUDE.md"]) {
            guidance_files.push(guidance);
        }
    }

    guidance_files
}

fn read_guidance_from_dir(dir: &Path, candidates: &[&str]) -> Option<(String, String)> {
    // Read directory entries once for case-insensitive lookup
    let dir_entries: Vec<_> = fs::read_dir(dir)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default();

    for candidate in candidates.iter() {
        // Find the first directory entry that matches case-insensitively
        let matched = dir_entries
            .iter()
            .find(|entry| entry.eq_ignore_ascii_case(candidate));

        if let Some(actual_name) = matched {
            let path = dir.join(actual_name);
            match fs::read_to_string(&path) {
                Ok(mut content) => {
                    // Guard against excessively large files (truncate politely)
                    const MAX_LEN: usize = 64 * 1024; // 64KB
                    if content.len() > MAX_LEN {
                        content.truncate(MAX_LEN);
                        content.push_str("\n\n[... truncated to keep context size reasonable ...]");
                    }
                    return Some((actual_name.to_string(), content));
                }
                Err(e) => {
                    warn!("Failed to read guidance file {}: {}", path.display(), e);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;

    #[test]
    fn reads_agents_md_case_insensitively_from_directory() -> Result<()> {
        let dir = tempdir()?;
        std::fs::write(dir.path().join("agents.md"), "global guidance")?;

        let guidance =
            read_guidance_from_dir(dir.path(), &["AGENTS.md"]).expect("expected guidance file");

        assert!(guidance.0.ends_with("agents.md"));
        assert_eq!(guidance.1, "global guidance");
        Ok(())
    }

    #[test]
    fn goals_section_lists_only_the_sessions_active_goals() -> Result<()> {
        use agent_orchestration::goals::{Budget, CompletionContract};

        let dir = tempdir()?;
        let goals_path = dir.path().join("goals.json");
        let store = GoalStore::new(&goals_path);
        let now = chrono::NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let contract = CompletionContract::new("done", "check", "stop");
        store.add_new(
            crate::goals::session_owner("sess-1"),
            "ship the widget",
            contract.clone(),
            Budget::turns(5),
            now,
        )?;
        store.add_new(
            crate::goals::session_owner("sess-2"),
            "other session's goal",
            contract.clone(),
            Budget::turns(5),
            now,
        )?;
        let mut done = store.add_new(
            crate::goals::session_owner("sess-1"),
            "already cancelled",
            contract,
            Budget::turns(5),
            now,
        )?;
        done.fail("cancelled", now)?;
        store.update(&done)?;

        let provider = CodeAssistantSystemPrompt::with_goals_path(&goals_path);
        let section = provider
            .render_goals_section("sess-1")
            .expect("active goal should render a section");
        assert!(section.starts_with("# Durable Goals"));
        assert!(section.contains("ship the widget"));
        assert!(section.contains("0/5 turns"));
        assert!(!section.contains("other session's goal"));
        assert!(!section.contains("already cancelled"));

        assert!(provider.render_goals_section("sess-3").is_none());
        Ok(())
    }

    #[test]
    fn prefers_agents_md_over_claude_md() -> Result<()> {
        let dir = tempdir()?;
        std::fs::write(dir.path().join("CLAUDE.md"), "claude guidance")?;
        std::fs::write(dir.path().join("AGENTS.md"), "agents guidance")?;

        let guidance = read_guidance_from_dir(dir.path(), &["AGENTS.md", "CLAUDE.md"])
            .expect("expected guidance file");

        assert!(guidance.0.ends_with("AGENTS.md"));
        assert_eq!(guidance.1, "agents guidance");
        Ok(())
    }
}
