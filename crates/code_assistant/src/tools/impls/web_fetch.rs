use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::{ParallelClient, WebClient, WebPage};

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
            "If a Parallel API key is configured, this tool uses the Parallel extract API for fast, structured access.\n",
            "Otherwise, it launches a headless browser to download and clean the requested page."
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
        if let Some(api_key) = crate::settings::parallel_api_key() {
            let client = ParallelClient::new(api_key);
            match client.fetch(&input.url).await {
                Ok(page) => {
                    store_in_working_memory(context, &page);
                    Ok(WebFetchOutput { page, error: None })
                }
                Err(e) => Ok(WebFetchOutput {
                    page: WebPage::default(),
                    error: Some(e.to_string()),
                }),
            }
        } else {
            // Create new browser client for each request
            let client = match WebClient::new().await {
                Ok(client) => client,
                Err(e) => {
                    return Ok(WebFetchOutput {
                        page: WebPage::default(),
                        error: Some(format!("Failed to create web client: {e}")),
                    });
                }
            };

            match client.fetch(&input.url).await {
                Ok(page) => {
                    store_in_working_memory(context, &page);
                    Ok(WebFetchOutput { page, error: None })
                }
                Err(e) => Ok(WebFetchOutput {
                    page: WebPage::default(),
                    error: Some(e.to_string()),
                }),
            }
        }
    }
}

fn store_in_working_memory(context: &mut ToolContext<'_>, page: &WebPage) {
    if let Some(working_memory) = &mut context.working_memory {
        let path = std::path::PathBuf::from(page.url.replace([':', '/', '?', '#'], "_"));
        let project = "web".to_string();
        working_memory.add_resource(
            project,
            path,
            crate::types::LoadedResource::WebPage(page.clone()),
        );
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
