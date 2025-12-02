use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::{PerplexityCitation, PerplexityClient, PerplexityMessage};

// Input type for the perplexity_ask tool
#[derive(Deserialize, Serialize)]
pub struct PerplexityAskInput {
    pub messages: Vec<PerplexityMessage>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct PerplexityAskOutput {
    #[allow(dead_code)]
    pub query: String,
    pub answer: String,
    pub citations: Vec<PerplexityCitation>,
    pub error: Option<String>,
}

// Render implementation for output formatting
impl Render for PerplexityAskOutput {
    fn status(&self) -> String {
        if let Some(e) = &self.error {
            format!("Failed to get answer from Perplexity: {e}")
        } else {
            "Answer received from Perplexity".to_string()
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Failed to get answer from Perplexity: {e}");
        }

        let mut output = self.answer.clone();

        if !self.citations.is_empty() {
            output.push_str("\n\nCitations:\n");
            for (i, citation) in self.citations.iter().enumerate() {
                output.push_str(&format!(
                    "[{}] {}: {}\n",
                    i + 1,
                    citation.text,
                    citation.url
                ));
            }
        }

        output
    }
}

// ToolResult implementation
impl ToolResult for PerplexityAskOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

// The tool implementation
pub struct PerplexityAskTool;

#[async_trait::async_trait]
impl Tool for PerplexityAskTool {
    type Input = PerplexityAskInput;
    type Output = PerplexityAskOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Engages in a conversation using the Perplexity Sonar API and returns an AI-generated answer with citations.\n",
            "This tool allows you to ask questions and receive comprehensive answers with references to source materials.\n",
            "The conversation is maintained as an array of messages with different roles (system, user, assistant), ",
            "allowing for multi-turn interactions (asking follow up questions)."
        );
        ToolSpec {
            name: "perplexity_ask",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "messages": {
                        "type": "array",
                        "description": "Array of conversation messages",
                        "items": {
                            "type": "object",
                            "properties": {
                                "role": {
                                    "type": "string",
                                    "description": "Role of the message (e.g., system, user, assistant)"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "The content of the message"
                                }
                            },
                            "required": ["role", "content"]
                        }
                    }
                },
                "required": ["messages"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": false,
                "openWorldHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
            hidden: false,
            title_template: None, // Uses default tool name
        }
    }

    async fn execute<'a>(
        &self,
        _context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Check if the API key exists
        let api_key = std::env::var("PERPLEXITY_API_KEY").ok();

        // Create a new Perplexity client
        let client = PerplexityClient::new(api_key);

        // Extract last 'user' message for display
        let query = input
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .next_back()
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "No user query found".to_string());

        // Call Perplexity API
        match client.ask(&input.messages, None).await {
            Ok(response) => Ok(PerplexityAskOutput {
                query,
                answer: response.content,
                citations: response.citations,
                error: None,
            }),
            Err(e) => Ok(PerplexityAskOutput {
                query,
                answer: String::new(),
                citations: vec![],
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
        let output = PerplexityAskOutput {
            query: "What is Rust?".to_string(),
            answer:
                "Rust is a systems programming language focused on safety, speed, and concurrency."
                    .to_string(),
            citations: vec![PerplexityCitation {
                text: "Rust Programming Language".to_string(),
                url: "https://www.rust-lang.org".to_string(),
            }],
            error: None,
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Rust is a systems programming language"));
        assert!(rendered.contains("Citations:"));
        assert!(rendered.contains("[1] Rust Programming Language: https://www.rust-lang.org"));
    }

    #[test]
    fn test_error_rendering() {
        // Create error output
        let output = PerplexityAskOutput {
            query: "What is Rust?".to_string(),
            answer: String::new(),
            citations: vec![],
            error: Some("API key not provided".to_string()),
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Failed to get answer from Perplexity: API key not provided"));
    }
}
