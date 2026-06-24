use crate::skills::{discover_project_skills, parse_skill_content};
use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::ToolServicesAccess;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

/// Cap on the returned skill body to keep context size reasonable.
const MAX_BODY_LEN: usize = 64 * 1024;

// Input type for the read_skill tool
#[derive(Deserialize, Serialize)]
pub struct ReadSkillInput {
    pub project: String,
    pub name: String,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ReadSkillOutput {
    pub project: String,
    pub name: String,
    /// The skill's directory, relative to the project root, so the paths
    /// referenced in the body resolve directly with `read_files`.
    pub dir: PathBuf,
    pub body: String,
}

impl Render for ReadSkillOutput {
    fn status(&self) -> String {
        format!("Loaded skill '{}'", self.name)
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let dir = self.dir.to_string_lossy().replace('\\', "/");
        format!(
            "# Skill: {name} (project: {project})\n\n\
             Bundled resources live under `{dir}/`; the paths referenced below are relative to \
             that directory. Read a resource with `read_files` (project `{project}`, path \
             `{dir}/<resource>`) or run a bundled script with `execute_command`.\n\n\
             ---\n\n\
             {body}",
            name = self.name,
            project = self.project,
            dir = dir,
            body = self.body,
        )
    }
}

impl ToolResult for ReadSkillOutput {
    fn is_success(&self) -> bool {
        true
    }
}

// Tool implementation
pub struct ReadSkillTool;

#[async_trait::async_trait]
impl Tool for ReadSkillTool {
    type Input = ReadSkillInput;
    type Output = ReadSkillOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Load a skill's full instructions into the conversation. Skills are reusable, ",
            "task-specific playbooks discovered under a project's `.agents/skills/` directory and ",
            "advertised in the system prompt. Call this when the user's task clearly matches a ",
            "skill's description."
        );
        ToolSpec {
            name: "read_skill",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project that owns the skill",
                        "examples": ["project-name"]
                    },
                    "name": {
                        "type": "string",
                        "description": "Name of the skill to load (as shown in the skills catalog)"
                    }
                },
                "required": ["project", "name"]
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
            title_template: Some("Loading skill {name}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Resolve the project's root directory the same way file tools do.
        let explorer = context
            .project_manager()
            .get_explorer_for_project(&input.project)
            .map_err(|e| {
                anyhow!(
                    "Failed to get explorer for project {}: {}",
                    input.project,
                    e
                )
            })?;
        let root = explorer.root_dir();

        let skill = discover_project_skills(&root)
            .into_iter()
            .find(|s| s.name == input.name)
            .ok_or_else(|| {
                anyhow!(
                    "No skill named `{}` was found in project `{}`",
                    input.name,
                    input.project
                )
            })?;

        // Express the skill directory relative to the project root so the
        // body's relative resource references resolve directly via read_files.
        let dir = skill
            .dir
            .strip_prefix(&root)
            .unwrap_or(skill.dir.as_path())
            .to_path_buf();

        let content = std::fs::read_to_string(&skill.skill_md)
            .map_err(|e| anyhow!("Failed to read skill `{}`: {}", input.name, e))?;
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

        Ok(ReadSkillOutput {
            project: input.project.clone(),
            name: skill.name,
            dir,
            body,
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

    /// Build a fixture whose `project` resolves to `root` on the real filesystem.
    fn fixture_for_root(project: &str, root: &std::path::Path) -> ToolTestFixture {
        let explorer = MockExplorer::new(HashMap::new(), None).with_root(root.to_path_buf());
        let pm = MockProjectManager::default().with_project_path(
            project,
            root.to_path_buf(),
            Box::new(explorer),
        );
        ToolTestFixture::with_project_manager(pm)
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

    #[tokio::test]
    async fn loads_skill_body() -> Result<()> {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "pdf-extraction",
            "Extract PDFs.",
            "Step 1. Do it.",
        );

        let mut fixture = fixture_for_root("my-project", dir.path());
        let mut context = fixture.context();

        let registry = crate::tools::test_registry();
        let tool = registry.get("read_skill").expect("read_skill registered");

        let mut params = json!({ "project": "my-project", "name": "pdf-extraction" });
        let result = tool.invoke(&mut context, &mut params).await?;

        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        assert!(result.is_success());
        assert!(output.contains("# Skill: pdf-extraction (project: my-project)"));
        assert!(output.contains(".agents/skills/pdf-extraction/"));
        assert!(output.contains("Step 1. Do it."));
        Ok(())
    }

    #[tokio::test]
    async fn errors_when_skill_missing() -> Result<()> {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "existing", "Present.", "body");

        let mut fixture = fixture_for_root("my-project", dir.path());
        let mut context = fixture.context();

        let registry = crate::tools::test_registry();
        let tool = registry.get("read_skill").expect("read_skill registered");

        let mut params = json!({ "project": "my-project", "name": "nope" });
        let err = match tool.invoke(&mut context, &mut params).await {
            Ok(_) => panic!("expected an error for a missing skill"),
            Err(e) => e,
        };

        assert!(err.to_string().contains("No skill named"));
        Ok(())
    }
}
