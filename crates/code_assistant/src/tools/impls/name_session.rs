use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

// Input type
#[derive(Deserialize, Serialize)]
pub struct NameSessionInput {
    pub title: String,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct NameSessionOutput {
    pub title: String,
}

// Render implementation for output formatting
impl Render for NameSessionOutput {
    fn status(&self) -> String {
        format!("Session named: {}", self.title)
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        format!("Session named: {}", self.title)
    }
}

// ToolResult implementation
impl ToolResult for NameSessionOutput {
    fn is_success(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

// The actual tool implementation
pub struct NameSessionTool;

#[async_trait::async_trait]
impl Tool for NameSessionTool {
    type Input = NameSessionInput;
    type Output = NameSessionOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Provide a concise, descriptive title for this session (3-5 words). ",
            "Use this tool when the user has provided a clear task or question that gives the session a clear purpose."
        );
        ToolSpec {
            name: "name_session",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "A concise, descriptive title for this session (3-5 words)"
                    }
                },
                "required": ["title"]
            }),
            annotations: None,
            supported_scopes: &[ToolScope::Agent, ToolScope::AgentWithDiffBlocks],
            hidden: true, // This tool should be hidden from UI
        }
    }

    async fn execute<'a>(
        &self,
        _context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Basic validation and cleanup
        let title = input.title.trim().to_string();

        if title.is_empty() {
            return Err(anyhow::anyhow!("Session title cannot be empty"));
        }

        // Limit title length to prevent extremely long names
        let title = if title.len() > 100 {
            format!("{}...", &title[..97])
        } else {
            title
        };

        Ok(NameSessionOutput { title })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::ToolTestFixture;

    #[tokio::test]
    async fn test_name_session_basic() {
        let tool = NameSessionTool;
        let mut fixture = ToolTestFixture::new()
            .with_ui()
            .with_tool_id("test-tool-1".to_string());
        let mut context = fixture.context();

        let mut input = NameSessionInput {
            title: "Test Session Title".to_string(),
        };

        let result = tool.execute(&mut context, &mut input).await.unwrap();
        assert_eq!(result.title, "Test Session Title");
        assert!(result.is_success());
    }

    #[tokio::test]
    async fn test_name_session_empty_title() {
        let tool = NameSessionTool;
        let mut fixture = ToolTestFixture::new()
            .with_ui()
            .with_tool_id("test-tool-1".to_string());
        let mut context = fixture.context();

        let mut input = NameSessionInput {
            title: "   ".to_string(), // Only whitespace
        };

        let result = tool.execute(&mut context, &mut input).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_name_session_long_title() {
        let tool = NameSessionTool;
        let mut fixture = ToolTestFixture::new()
            .with_ui()
            .with_tool_id("test-tool-1".to_string());
        let mut context = fixture.context();

        let long_title = "A".repeat(150);
        let mut input = NameSessionInput { title: long_title };

        let result = tool.execute(&mut context, &mut input).await.unwrap();
        assert!(result.title.len() <= 100);
        assert!(result.title.ends_with("..."));
        assert!(result.is_success());
    }

    #[tokio::test]
    async fn test_name_session_render() {
        let output = NameSessionOutput {
            title: "Test Session".to_string(),
        };
        let mut tracker = ResourcesTracker::new();

        let rendered = output.render(&mut tracker);
        assert_eq!(rendered, "Session named: Test Session");

        let status = output.status();
        assert_eq!(status, "Session named: Test Session");
    }
}
