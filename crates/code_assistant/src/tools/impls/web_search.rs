use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::{WebClient, WebSearchResult};

// Input type for the web_search tool
#[derive(Deserialize)]
pub struct WebSearchInput {
    pub query: String,
    pub hits_page_number: u32,
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
            format!("Search failed: {}", e)
        } else if self.results.is_empty() {
            format!("No search results found")
        } else {
            format!("Found {} result(s)", self.results.len())
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Search failed: {}", e);
        }

        if self.results.is_empty() {
            return format!("No search results found.");
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
                        "minimum": 1
                    }
                },
                "required": ["query", "hits_page_number"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true,
                "openWorldHint": true
            })),
            supported_scopes: &[ToolScope::McpServer, ToolScope::Agent],
        }
    }

    async fn execute<'a>(
        &self,
        _context: &mut ToolContext<'a>,
        input: Self::Input,
    ) -> Result<Self::Output> {
        // Create new client for each request
        let client = match WebClient::new().await {
            Ok(client) => client,
            Err(e) => {
                return Ok(WebSearchOutput {
                    query: input.query,
                    results: vec![],
                    error: Some(format!("Failed to create web client: {}", e)),
                });
            }
        };

        // Execute search
        match client.search(&input.query, input.hits_page_number).await {
            Ok(results) => Ok(WebSearchOutput {
                query: input.query,
                results,
                error: None,
            }),
            Err(e) => Ok(WebSearchOutput {
                query: input.query,
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
}
