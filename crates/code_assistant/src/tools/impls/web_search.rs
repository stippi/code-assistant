use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::{WebClient, WebSearchResult};

// Input type for the web_search tool
#[derive(Deserialize, Serialize)]
pub struct WebSearchInput {
    pub query: String,
    #[serde(default = "default_page_number")]
    pub hits_page_number: u32,
}

fn default_page_number() -> u32 {
    1
}

// Output type with search results
#[derive(Serialize, Deserialize)]
pub struct WebSearchOutput {
    #[allow(dead_code)]
    pub query: String,
    pub results: Vec<WebSearchResult>,
    pub error: Option<String>,
}

// Render implementation for output formatting
impl Render for WebSearchOutput {
    fn status(&self) -> String {
        if let Some(e) = &self.error {
            format!("Search failed: {e}")
        } else if self.results.is_empty() {
            "No search results found".to_string()
        } else {
            format!("Found {} result(s)", self.results.len())
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Search failed: {e}");
        }

        if self.results.is_empty() {
            return "No search results found.".to_string();
        }

        let mut output = format!("Found {} result(s):", self.results.len());

        for result in &self.results {
            output.push_str(&format!(
                "- Title: {}\n  URL: {}\n  Snippet: {}\n\n",
                result.title, result.url, result.snippet
            ));
        }

        output
    }
}

// ToolResult implementation
impl ToolResult for WebSearchOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

// The tool implementation
pub struct WebSearchTool;

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    type Input = WebSearchInput;
    type Output = WebSearchOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Search the web using DuckDuckGo. ",
            "This tool performs a web search for the specified query and returns a list of search results, ",
            "each containing a title, URL, and text snippet. ",
            "The search results are paginated, and you can request different pages of results using the ",
            "`hits_page_number` parameter (starting from 1)."
        );
        ToolSpec {
            name: "web_search",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "hits_page_number": {
                        "type": "integer",
                        "description": "Page number for pagination (1-based)",
                        "minimum": 1,
                        "default": 1
                    }
                },
                "required": ["query"]
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
                ToolScope::SubAgentReadOnly,
                ToolScope::SubAgentDefault,
            ],
            // Note: can be disabled in read-only sub-agents if needed later.
            hidden: false,
            title_template: Some("Searching web for '{query}'"),
        }
    }

    async fn execute<'a>(
        &self,
        _context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Create new client for each request
        let client = match WebClient::new().await {
            Ok(client) => client,
            Err(e) => {
                return Ok(WebSearchOutput {
                    query: input.query.clone(),
                    results: vec![],
                    error: Some(format!("Failed to create web client: {e}")),
                });
            }
        };

        // Execute search
        match client.search(&input.query, input.hits_page_number).await {
            Ok(results) => Ok(WebSearchOutput {
                query: input.query.clone(),
                results,
                error: None,
            }),
            Err(e) => Ok(WebSearchOutput {
                query: input.query.clone(),
                results: vec![],
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
        let output = WebSearchOutput {
            query: "test query".to_string(),
            results: vec![WebSearchResult {
                url: "https://example.com".to_string(),
                title: "Example Website".to_string(),
                snippet: "This is an example website for testing.".to_string(),
                metadata: web::PageMetadata::default(),
            }],
            error: None,
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Found 1 result(s):"));
        assert!(rendered.contains("Title: Example Website"));
        assert!(rendered.contains("URL: https://example.com"));
        assert!(rendered.contains("Snippet: This is an example website for testing."));
    }

    #[test]
    fn test_error_rendering() {
        // Create error output
        let output = WebSearchOutput {
            query: "test query".to_string(),
            results: vec![],
            error: Some("Connection failed".to_string()),
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Search failed: Connection failed"));
    }

    #[test]
    fn test_default_page_number_deserialization() {
        use serde_json::json;

        // Test with hits_page_number provided
        let input_with_page: WebSearchInput = serde_json::from_value(json!({
            "query": "test query",
            "hits_page_number": 2
        }))
        .unwrap();

        assert_eq!(input_with_page.query, "test query");
        assert_eq!(input_with_page.hits_page_number, 2);

        // Test with hits_page_number omitted (should use default)
        let input_default: WebSearchInput = serde_json::from_value(json!({
            "query": "test query"
        }))
        .unwrap();

        assert_eq!(input_default.query, "test query");
        assert_eq!(input_default.hits_page_number, 1); // Should use default
    }
}
