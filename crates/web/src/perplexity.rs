use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerplexityMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerplexityResponse {
    pub content: String,
    pub citations: Vec<PerplexityCitation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerplexityCitation {
    pub text: String,
    pub url: String,
}

pub struct PerplexityClient {
    http_client: Client,
    api_key: Option<String>,
}

impl PerplexityClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
        }
    }

    pub async fn ask(&self, messages: &[PerplexityMessage], model: Option<String>) -> Result<PerplexityResponse> {
        // Ensure API key is available
        let api_key = match &self.api_key {
            Some(key) => key,
            None => return Err(anyhow::anyhow!("Perplexity API key not provided")),
        };

        // Use provided model or default to "sonar-pro"
        let model = model.unwrap_or_else(|| "sonar-pro".to_string());

        // Prepare the request body with the messages
        let body = json!({
            "model": model,
            "messages": messages
        });

        // Make the API request
        let response = self.http_client
            .post("https://api.perplexity.ai/chat/completions")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&body)
            .send()
            .await?;

        // Check if the request was successful
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Perplexity API error: {} {}\n{}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown"),
                error_text
            ));
        }

        // Parse the response
        let json_response = response.json::<serde_json::Value>().await?;
        
        // Extract content from the response
        let content = json_response
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| content.as_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid response format"))?
            .to_string();

        // Extract citations if available
        let mut citations = Vec::new();
        if let Some(citation_array) = json_response.get("citations").and_then(|c| c.as_array()) {
            for (i, citation) in citation_array.iter().enumerate() {
                if let Some(url) = citation.as_str() {
                    citations.push(PerplexityCitation {
                        text: format!("Citation {}", i + 1),
                        url: url.to_string(),
                    });
                }
            }
        }

        Ok(PerplexityResponse {
            content,
            citations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{mock, server_url};
    use serde_json::json;

    #[tokio::test]
    async fn test_perplexity_ask() {
        // Setup mock server
        let _m = mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "choices": [
                        {
                            "message": {
                                "content": "This is a test response."
                            }
                        }
                    ],
                    "citations": [
                        "https://example.com/citation1",
                        "https://example.com/citation2"
                    ]
                })
                .to_string(),
            )
            .create();

        // Create client with mock server URL
        let mut client = PerplexityClient::new(Some("test_api_key".to_string()));
        
        // Override the client's base URL for testing
        let test_url = server_url();
        let test_client = reqwest::Client::builder()
            .build()
            .unwrap();
        client.http_client = test_client;

        // Create test messages
        let messages = vec![
            PerplexityMessage {
                role: "user".to_string(),
                content: "Test question".to_string(),
            },
        ];

        // Make the request
        let response = client.ask(&messages, None).await;
        
        // Check if the mock was called and the response was parsed correctly
        assert!(response.is_ok());
        let response = response.unwrap();
        assert_eq!(response.content, "This is a test response.");
        assert_eq!(response.citations.len(), 2);
        assert_eq!(response.citations[0].url, "https://example.com/citation1");
        assert_eq!(response.citations[1].url, "https://example.com/citation2");
    }

    #[tokio::test]
    async fn test_perplexity_ask_error() {
        // Setup mock server for error response
        let _m = mock("POST", "/chat/completions")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "error": {
                        "message": "Invalid API key",
                        "type": "auth_error"
                    }
                })
                .to_string(),
            )
            .create();

        // Create client with mock server URL
        let mut client = PerplexityClient::new(Some("invalid_api_key".to_string()));
        
        // Override the client's base URL for testing
        let test_url = server_url();
        let test_client = reqwest::Client::builder()
            .build()
            .unwrap();
        client.http_client = test_client;

        // Create test messages
        let messages = vec![
            PerplexityMessage {
                role: "user".to_string(),
                content: "Test question".to_string(),
            },
        ];

        // Make the request
        let response = client.ask(&messages, None).await;
        
        // Check if the error was handled correctly
        assert!(response.is_err());
        let error = response.err().unwrap();
        assert!(error.to_string().contains("Invalid API key"));
    }
}
