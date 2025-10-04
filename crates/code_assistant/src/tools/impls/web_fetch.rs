use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::{WebClient, WebPage};

// Input type for the web_fetch tool
#[derive(Deserialize, Serialize)]
pub struct WebFetchInput {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selectors: Option<Vec<String>>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct WebFetchOutput {
    pub page: WebPage,
    pub error: Option<String>,
}

// Render implementation for output formatting
impl Render for WebFetchOutput {
    fn status(&self) -> String {
        if let Some(e) = &self.error {
            format!("Failed to fetch page: {e}")
        } else {
            "Page fetched successfully".to_string()
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Failed to fetch page: {e}");
        }

        format!(
            "Page fetched successfully:\n>>>>> CONTENT:\n{}\n<<<<< END CONTENT",
            self.page.content
        )
    }
}

// ToolResult implementation
impl ToolResult for WebFetchOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

// The tool implementation
pub struct WebFetchTool;

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    type Input = WebFetchInput;
    type Output = WebFetchOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Fetch and extract content from a web page.\n",
            "This tool downloads the specified web page and converts its content to a readable format.\n",
            "Optionally, you can provide CSS selectors to extract specific sections of the page.\n",
            "The tool handles various formats and cleans up the output to provide readable content that ",
            "can be used for further analysis."
        );
        ToolSpec {
            name: "web_fetch",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL of the web page to fetch"
                    },
                    "selectors": {
                        "type": "array",
                        "description": "Optional CSS selectors to extract specific content",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["url"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true,
                "openWorldHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
            hidden: false,
            title_template: Some("Fetching {url}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Create new client for each request
        let client = match WebClient::new().await {
            Ok(client) => client,
            Err(e) => {
                return Ok(WebFetchOutput {
                    page: WebPage::default(),
                    error: Some(format!("Failed to create web client: {e}")),
                });
            }
        };

        // Fetch the page
        match client.fetch(&input.url).await {
            Ok(page) => {
                // Update working memory if available
                if let Some(working_memory) = &mut context.working_memory {
                    // Use the URL as path (normalized)
                    let path =
                        std::path::PathBuf::from(page.url.replace([':', '/', '?', '#'], "_"));

                    // Use "web" as the project name for web resources
                    let project = "web".to_string();

                    // Store in working memory
                    working_memory.add_resource(
                        project,
                        path,
                        crate::types::LoadedResource::WebPage(page.clone()),
                    );
                }

                Ok(WebFetchOutput { page, error: None })
            }
            Err(e) => Ok(WebFetchOutput {
                page: WebPage::default(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rendering() {
        // Create sample output
        let output = WebFetchOutput {
            page: WebPage {
                url: "https://example.com".to_string(),
                content: "This is the page content.".to_string(),
                metadata: web::PageMetadata::default(),
            },
            error: None,
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Page fetched successfully"));
        assert!(rendered.contains("This is the page content."));
    }

    #[test]
    fn test_error_rendering() {
        // Create error output
        let output = WebFetchOutput {
            page: WebPage::default(),
            error: Some("Connection failed".to_string()),
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Failed to fetch page: Connection failed"));
    }
}
