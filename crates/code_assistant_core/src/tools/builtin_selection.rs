//! Which built-in tool groups (browser, web, terminal, …) a session has
//! switched on, and what the next session of a project starts with. Mirrors
//! [`crate::tools::mcp_selection`]:
//!
//! - **Global**: `tools.json` (`builtin_tools`) can switch a group off for
//!   every session (`enabled: false`), or off in new sessions only
//!   (`off_in_new_sessions`). A group without an entry is on.
//! - **Project**: `<config_dir>/project-builtin-tools.json` keeps, per project
//!   root, only the groups whose state differs from their global default.
//!   Switching a group in a session records it for the session's project.
//! - **Session**: `SessionConfig::enabled_builtin_tools` /
//!   `disabled_builtin_tools` hold every group's state from when the session
//!   was created, plus later switches. A group the session has no state for
//!   (a session from before this existed) is on or off as in a new session of
//!   the project.
//!
//! Tools of a group carry its [`group_capability`] tag; the agent excludes the
//! tags of the groups that are off for its session. The tools stay in the
//! registry, so past calls of them still render.

use crate::session::SessionConfig;
use crate::tools::config::ToolsConfig;
use crate::tools::core::ToolRegistry;
use crate::tools::mcp_selection::ProjectChoiceStore;
use crate::ui::ui_events::BuiltinToolToggle;
use anyhow::Result;

const PROJECTS: ProjectChoiceStore = ProjectChoiceStore::new("project-builtin-tools.json");

/// A set of built-in tools switched on and off together.
pub struct ToolGroup {
    /// Stable id, used in `tools.json`, session configs and project choices.
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    contains: fn(&str) -> bool,
}

/// The built-in tool groups. Tools in none of them (files, plan, skills, …)
/// are always on.
pub const GROUPS: &[ToolGroup] = &[
    ToolGroup {
        id: "browser",
        label: "Browser",
        description: "Drive a Chrome browser: navigate, click, read pages, screenshots",
        contains: |name| name.starts_with("browser_"),
    },
    ToolGroup {
        id: "web",
        label: "Web search & fetch",
        description: "web_search, web_fetch and perplexity_ask",
        contains: |name| matches!(name, "web_search" | "web_fetch" | "perplexity_ask"),
    },
    ToolGroup {
        id: "terminal",
        label: "Terminal",
        description: "Run shell commands (execute_command, write_stdin)",
        contains: |name| matches!(name, "execute_command" | "write_stdin"),
    },
    ToolGroup {
        id: "sub_agents",
        label: "Sub-agents",
        description: "Delegate work to sub-agents (spawn_agent)",
        contains: |name| name == "spawn_agent",
    },
    ToolGroup {
        id: "wakeups",
        label: "Wakeups",
        description: "Schedule the agent to resume later (schedule_wakeup, cancel_wakeup)",
        contains: |name| matches!(name, "schedule_wakeup" | "cancel_wakeup"),
    },
    ToolGroup {
        id: "monitors",
        label: "Monitors",
        description: "Watch a command's output in the background and get notified (monitor, stop_monitor)",
        contains: |name| matches!(name, "monitor" | "stop_monitor"),
    },
    ToolGroup {
        id: "session_history",
        label: "Session history",
        description: "Search and read other sessions (search_sessions, get_session_content)",
        contains: |name| matches!(name, "search_sessions" | "get_session_content"),
    },
];

/// The capability tag carried by the tools of the group `id`.
pub fn group_capability(id: &str) -> String {
    format!("toolset:{id}")
}

/// Tag every registered built-in tool with the capability of its group.
pub fn tag_groups(registry: &mut ToolRegistry) {
    let names: Vec<String> = registry.names().map(str::to_string).collect();
    for name in names {
        if let Some(group) = GROUPS.iter().find(|group| (group.contains)(&name)) {
            registry.add_capability(&name, group_capability(group.id));
        }
    }
}

fn load_config() -> ToolsConfig {
    // Raw: a missing secret must not reset the groups to their defaults.
    ToolsConfig::load_raw().unwrap_or_else(|e| {
        tracing::warn!("{e:#}");
        ToolsConfig::default()
    })
}

/// The globally enabled groups, each with whether it is on by default.
fn available_groups(config: &ToolsConfig) -> impl Iterator<Item = (&'static ToolGroup, bool)> {
    GROUPS.iter().filter_map(|group| {
        let settings = config.builtin_group(group.id);
        settings
            .enabled
            .then_some((group, !settings.off_in_new_sessions))
    })
}

fn session_choice(config: &SessionConfig, id: &str) -> Option<bool> {
    if config.enabled_builtin_tools.iter().any(|n| n == id) {
        Some(true)
    } else if config.disabled_builtin_tools.iter().any(|n| n == id) {
        Some(false)
    } else {
        None
    }
}

/// The built-in tool groups available to a session (the globally enabled
/// ones) and whether each is on for it.
pub fn session_builtin_tools(config: &SessionConfig) -> Vec<BuiltinToolToggle> {
    let project = PROJECTS.choices(config.init_path.as_deref());
    available_groups(&load_config())
        .map(|(group, on_by_default)| {
            let enabled = session_choice(config, group.id)
                .or_else(|| project.get(group.id).copied())
                .unwrap_or(on_by_default);
            BuiltinToolToggle {
                id: group.id.to_string(),
                label: group.label.to_string(),
                enabled,
            }
        })
        .collect()
}

/// The capability tags of the groups that are off for a session, globally or
/// by its own switch; the agent hides their tools.
pub fn session_excluded_capabilities(config: &SessionConfig) -> Vec<String> {
    let on: Vec<String> = session_builtin_tools(config)
        .into_iter()
        .filter(|group| group.enabled)
        .map(|group| group.id)
        .collect();
    GROUPS
        .iter()
        .filter(|group| !on.iter().any(|id| id == group.id))
        .map(|group| group_capability(group.id))
        .collect()
}

/// Record every available group's state in a new session's config: the
/// project's choice, else the global default.
pub fn seed_new_session(config: &mut SessionConfig) {
    config.enabled_builtin_tools.clear();
    config.disabled_builtin_tools.clear();
    for group in session_builtin_tools(config) {
        if group.enabled {
            config.enabled_builtin_tools.push(group.id);
        } else {
            config.disabled_builtin_tools.push(group.id);
        }
    }
}

/// Switch a group on or off in a session's config.
pub fn switch_group(config: &mut SessionConfig, id: &str, on: bool) {
    config.enabled_builtin_tools.retain(|n| n != id);
    config.disabled_builtin_tools.retain(|n| n != id);
    if on {
        config.enabled_builtin_tools.push(id.to_string());
    } else {
        config.disabled_builtin_tools.push(id.to_string());
    }
}

/// Remember a switch in a session for the project the config belongs to, so
/// the project's next sessions start that way. Only a state that differs from
/// the group's default is kept.
pub fn remember_for_project(config: &SessionConfig, id: &str, on: bool) -> Result<()> {
    let Some(root) = config.init_path.as_deref() else {
        return Ok(());
    };
    let on_by_default = !load_config().builtin_group(id).off_in_new_sessions;
    PROJECTS.remember(root, id, on, on_by_default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_global(config_dir: &Path) {
        std::fs::write(
            config_dir.join("tools.json"),
            r#"{ "builtin_tools": {
                "browser": { "off_in_new_sessions": true },
                "terminal": { "enabled": false }
            } }"#,
        )
        .unwrap();
    }

    fn states(config: &SessionConfig) -> Vec<(String, bool)> {
        session_builtin_tools(config)
            .into_iter()
            .map(|g| (g.id, g.enabled))
            .collect()
    }

    fn session_in(project: &Path) -> SessionConfig {
        SessionConfig {
            init_path: Some(project.to_path_buf()),
            ..SessionConfig::default()
        }
    }

    #[test]
    fn without_settings_every_group_is_on() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let session = session_in(project.path());
            assert!(states(&session).iter().all(|(_, on)| *on));
            assert_eq!(states(&session).len(), GROUPS.len());
            assert!(session_excluded_capabilities(&session).is_empty());
        });
    }

    #[test]
    fn global_settings_hide_and_default_groups() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_global(config_dir.path());
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let mut session = session_in(project.path());
            seed_new_session(&mut session);
            assert_eq!(session.disabled_builtin_tools, vec!["browser".to_string()]);
            let listed = states(&session);
            assert!(listed.contains(&("browser".to_string(), false)));
            assert!(
                !listed.iter().any(|(id, _)| id == "terminal"),
                "a globally disabled group is not offered"
            );
            let excluded = session_excluded_capabilities(&session);
            assert!(excluded.contains(&"toolset:browser".to_string()));
            assert!(excluded.contains(&"toolset:terminal".to_string()));
            assert_eq!(excluded.len(), 2);

            // Switching it on in the session cannot bring back a globally
            // disabled group.
            switch_group(&mut session, "browser", true);
            switch_group(&mut session, "terminal", true);
            assert_eq!(
                session_excluded_capabilities(&session),
                vec!["toolset:terminal".to_string()]
            );
        });
    }

    #[test]
    fn a_switch_carries_over_to_the_projects_next_session() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let other_project = tempfile::tempdir().unwrap();
        write_global(config_dir.path());
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let mut first = session_in(project.path());
            seed_new_session(&mut first);
            switch_group(&mut first, "browser", true);
            remember_for_project(&first, "browser", true).unwrap();
            switch_group(&mut first, "web", false);
            remember_for_project(&first, "web", false).unwrap();

            let mut next = session_in(project.path());
            seed_new_session(&mut next);
            assert!(states(&next).contains(&("browser".to_string(), true)));
            assert!(states(&next).contains(&("web".to_string(), false)));

            let mut elsewhere = session_in(other_project.path());
            seed_new_session(&mut elsewhere);
            assert!(states(&elsewhere).contains(&("browser".to_string(), false)));
            assert!(states(&elsewhere).contains(&("web".to_string(), true)));

            // Back to the defaults: nothing left to remember.
            remember_for_project(&first, "browser", false).unwrap();
            remember_for_project(&first, "web", true).unwrap();
            assert!(PROJECTS.choices(Some(project.path())).is_empty());
        });
    }

    #[test]
    fn tagging_marks_the_tools_of_each_group() {
        let registry = crate::tools::test_registry();
        let tagged =
            |name: &str, id: &str| registry.tool_has_capability(name, &group_capability(id));
        assert!(tagged("browser_navigate", "browser"));
        assert!(tagged("web_fetch", "web"));
        assert!(tagged("execute_command", "terminal"));
        assert!(tagged("spawn_agent", "sub_agents"));
        assert!(!tagged("read_files", "browser"));
        assert!(
            GROUPS
                .iter()
                .all(|group| !registry.tool_has_capability("edit", &group_capability(group.id)))
        );
    }
}
