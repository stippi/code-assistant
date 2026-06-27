use crate::skills::{discover_scope_skills_filtered, SkillsConfig};
use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::ToolServicesAccess;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

// Input type for the list_skills tool
#[derive(Deserialize, Serialize)]
pub struct ListSkillsInput {
    /// Scope to list: a project name, or `:config:` / `:system:` for user /
    /// bundled skills.
    pub project: String,
    /// Optional case-insensitive substring filter on name and description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ListSkillsOutput {
    pub project: String,
    pub scope_label: String,
    pub skills: Vec<SkillEntry>,
}

impl Render for ListSkillsOutput {
    fn status(&self) -> String {
        format!(
            "Found {} {} skill(s) in `{}`",
            self.skills.len(),
            self.scope_label,
            self.project
        )
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if self.skills.is_empty() {
            return format!("No skills found in scope `{}`.", self.project);
        }

        let mut out = format!(
            "Skills in scope `{}` ({}):\n",
            self.project, self.scope_label
        );
        for skill in &self.skills {
            out.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        out.push_str(&format!(
            "\nLoad a skill's full instructions with `read_skill` (project `{}`).",
            self.project
        ));
        out
    }
}

impl ToolResult for ListSkillsOutput {
    fn is_success(&self) -> bool {
        true
    }
}

// Tool implementation
pub struct ListSkillsTool;

#[async_trait::async_trait]
impl Tool for ListSkillsTool {
    type Input = ListSkillsInput;
    type Output = ListSkillsOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "List skills in a given scope. Skills are reusable, task-specific playbooks. The ",
            "skills relevant to the current project — its own project skills plus the shared user ",
            "(`:config:`) and bundled system (`:system:`) skills — are already listed in the ",
            "system prompt, so you normally do not need this tool. Use it to browse a *different* ",
            "project's skills (pass that project's name), or to filter a long catalog by query. ",
            "Load a skill's full instructions with `read_skill`."
        );
        ToolSpec {
            name: "list_skills",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Scope to list: a project name, or `:config:` / `:system:` for user / bundled skills",
                        "examples": ["project-name", ":config:", ":system:"]
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional case-insensitive substring filter on skill name and description"
                    }
                },
                "required": ["project"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true
            })),
            capabilities: &[
                capabilities::SCOPE_MCP,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ],
            multiline_params: &[],
            hidden: false,
            title_template: Some("Listing skills in {project}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let config = SkillsConfig::load();
        if !config.enabled {
            return Err(anyhow!(
                "Skills are disabled in this configuration (skills.json). \
                 Enable them to list skills."
            ));
        }

        let resolved =
            discover_scope_skills_filtered(context.project_manager(), &input.project, &config)
                .map_err(|e| anyhow!("Failed to resolve scope {}: {}", input.project, e))?;

        let query = input.query.as_deref().map(str::to_lowercase);
        let skills = resolved
            .skills
            .into_iter()
            .filter(|s| match &query {
                Some(q) => {
                    s.name.to_lowercase().contains(q) || s.description.to_lowercase().contains(q)
                }
                None => true,
            })
            .map(|s| SkillEntry {
                name: s.name,
                description: s.description,
            })
            .collect();

        Ok(ListSkillsOutput {
            project: input.project.clone(),
            scope_label: resolved.scope.label().to_string(),
            skills,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{MockExplorer, MockProjectManager, ToolTestFixture};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    fn fixture_for_root(project: &str, root: &std::path::Path) -> ToolTestFixture {
        let explorer = MockExplorer::new(HashMap::new(), None).with_root(root.to_path_buf());
        let pm = MockProjectManager::default().with_project_path(
            project,
            root.to_path_buf(),
            Box::new(explorer),
        );
        ToolTestFixture::with_project_manager(pm)
    }

    fn write_skill(root: &std::path::Path, name: &str, description: &str) {
        let dir = root.join(".agents").join("skills").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nbody"),
        )
        .unwrap();
    }

    async fn run(fixture: &mut ToolTestFixture, params: serde_json::Value) -> String {
        let mut context = fixture.context();
        let registry = crate::tools::test_registry();
        let tool = registry.get("list_skills").expect("list_skills registered");
        let mut params = params;
        let result = tool.invoke(&mut context, &mut params).await.unwrap();
        let mut tracker = ResourcesTracker::new();
        result.as_render().render(&mut tracker)
    }

    #[tokio::test]
    async fn lists_all_skills() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "First.");
        write_skill(dir.path(), "beta", "Second.");

        let mut fixture = fixture_for_root("my-project", dir.path());
        let output = run(&mut fixture, json!({ "project": "my-project" })).await;

        assert!(output.contains("- alpha: First."));
        assert!(output.contains("- beta: Second."));
    }

    #[tokio::test]
    async fn filters_by_query() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "pdf-extraction", "Extract text from PDFs.");
        write_skill(dir.path(), "security-review", "Audit auth and crypto.");

        let mut fixture = fixture_for_root("my-project", dir.path());
        let output = run(
            &mut fixture,
            json!({ "project": "my-project", "query": "pdf" }),
        )
        .await;

        assert!(output.contains("pdf-extraction"));
        assert!(!output.contains("security-review"));
    }

    #[tokio::test]
    async fn reports_empty_catalog() {
        let dir = tempdir().unwrap();
        let mut fixture = fixture_for_root("my-project", dir.path());
        let output = run(&mut fixture, json!({ "project": "my-project" })).await;

        assert!(output.contains("No skills found"));
    }
}
