use crate::skills::{
    load_skill_payload, render_skill_body_with_header, SkillPayload, SkillsConfig,
};
use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::ToolServicesAccess;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the read_skill tool
#[derive(Deserialize, Serialize)]
pub struct ReadSkillInput {
    /// The scope to load the skill from: a project name, or `:config:` /
    /// `:system:` for user / bundled skills (as shown in the skills catalog).
    pub project: String,
    pub name: String,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ReadSkillOutput {
    /// The scope token the skill was loaded from (a project name, or
    /// `:config:` / `:system:`) — pass it back to `read_files` for resources.
    pub scope: String,
    /// Human-readable scope label (`project` / `user` / `system`).
    pub scope_label: String,
    pub name: String,
    /// The skill's directory, relative to the scope's sandbox root, so the
    /// paths referenced in the body resolve directly with `read_files`.
    pub dir: PathBuf,
    pub body: String,
}

impl ReadSkillOutput {
    fn payload(&self) -> SkillPayload {
        SkillPayload {
            scope_token: self.scope.clone(),
            scope_label: self.scope_label.clone(),
            name: self.name.clone(),
            dir: self.dir.clone(),
            body: self.body.clone(),
        }
    }
}

impl Render for ReadSkillOutput {
    fn status(&self) -> String {
        format!("Loaded skill '{}'", self.name)
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        render_skill_body_with_header(&self.payload())
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
            "task-specific playbooks advertised in the system prompt. Call this when the user's ",
            "task clearly matches a skill's description."
        );
        ToolSpec {
            name: "read_skill".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Scope of the skill: a project name, or `:config:` / `:system:` for user / bundled skills (as shown after the skill name in the catalog)",
                        "examples": ["project-name", ":config:", ":system:"]
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
            capabilities: ToolSpec::capabilities(&[
                capabilities::SCOPE_MCP,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
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
        let config = SkillsConfig::load();
        let payload = load_skill_payload(
            context.project_manager(),
            &input.project,
            &input.name,
            &config,
        )?;

        Ok(ReadSkillOutput {
            scope: payload.scope_token,
            scope_label: payload.scope_label,
            name: payload.name,
            dir: payload.dir,
            body: payload.body,
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
        assert!(output.contains("# Skill: pdf-extraction (project)"));
        assert!(output.contains(".agents/skills/pdf-extraction/"));
        // The resource line names the scope token to pass back to read_files.
        assert!(output.contains("project `my-project`"));
        assert!(output.contains("Step 1. Do it."));
        Ok(())
    }

    #[tokio::test]
    async fn loads_model_invocation_disabled_skill() -> Result<()> {
        // A skill hidden from the model catalog must still be loadable via
        // read_skill (e.g. when the user explicitly activates it).
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join(".agents").join("skills").join("internal");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: internal\ndescription: Hidden.\ndisable-model-invocation: true\n---\nBody here.",
        )
        .unwrap();

        let mut fixture = fixture_for_root("my-project", dir.path());
        let mut context = fixture.context();

        let registry = crate::tools::test_registry();
        let tool = registry.get("read_skill").expect("read_skill registered");

        let mut params = json!({ "project": "my-project", "name": "internal" });
        let result = tool.invoke(&mut context, &mut params).await?;

        assert!(result.is_success());
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);
        assert!(output.contains("# Skill: internal (project)"));
        assert!(output.contains("Body here."));
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
