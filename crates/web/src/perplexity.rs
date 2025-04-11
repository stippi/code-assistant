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
    base_url: String,
}

impl PerplexityClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
            base_url: "https://api.perplexity.ai".to_string(),
        }
    }

    // For testing: allows specifying a different base URL
    #[cfg(test)]
    pub fn with_base_url(api_key: Option<String>, base_url: String) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
            base_url,
        }
    }

    pub async fn ask(
        &self,
        messages: &[PerplexityMessage],
        model: Option<String>,
    ) -> Result<PerplexityResponse> {
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
        let endpoint = format!("{}/chat/completions", self.base_url);
        let response = self
            .http_client
            .post(endpoint)
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

        Ok(PerplexityResponse { content, citations })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Path, response::IntoResponse, routing::post, Json, Router};
    use serde_json::json;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    // Helper to create a mock Perplexity API server
    async fn create_perplexity_mock_server(
        status_code: axum::http::StatusCode,
        response_body: serde_json::Value,
    ) -> String {
        let app = Router::new().route(
            "/*path",
            post(
                move |Path(_path): Path<String>, _req: Json<serde_json::Value>| {
                    let response_body = response_body.clone();
                    let status_code = status_code;
                    async move { (status_code, Json(response_body)).into_response() }
                },
            ),
        );

        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let listener = TcpListener::bind(addr).await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        format!("http://{}", server_addr)
    }

    #[tokio::test]
    async fn test_perplexity_ask() -> Result<()> {
        // Setup mock server with successful response
        let response_body = json!({
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
        });

        let base_url =
            create_perplexity_mock_server(axum::http::StatusCode::OK, response_body).await;

        // Create client with mock server URL
        let client = PerplexityClient::with_base_url(Some("test_api_key".to_string()), base_url);

        // Create test messages
        let messages = vec![PerplexityMessage {
            role: "user".to_string(),
            content: "Test question".to_string(),
        }];

        // Make the request
        let response = client.ask(&messages, None).await;

        // Check if the response was parsed correctly
        assert!(response.is_ok(), "Response should be successful");
        let response = response.unwrap();
        assert_eq!(response.content, "This is a test response.");
        assert_eq!(response.citations.len(), 2);
        assert_eq!(response.citations[0].url, "https://example.com/citation1");
        assert_eq!(response.citations[1].url, "https://example.com/citation2");

        Ok(())
    }

    #[tokio::test]
    async fn test_perplexity_ask_error() -> Result<()> {
        // Setup mock server with error response
        let error_body = json!({
            "error": {
                "message": "Invalid API key",
                "type": "auth_error"
            }
        });

        let base_url =
            create_perplexity_mock_server(axum::http::StatusCode::UNAUTHORIZED, error_body).await;

        // Create client with mock server URL
        let client = PerplexityClient::with_base_url(Some("invalid_api_key".to_string()), base_url);

        // Create test messages
        let messages = vec![PerplexityMessage {
            role: "user".to_string(),
            content: "Test question".to_string(),
        }];

        // Make the request
        let response = client.ask(&messages, None).await;

        // Check if the error was handled correctly
        assert!(response.is_err(), "Response should be an error");
        let error = response.err().unwrap();
        assert!(
            error.to_string().contains("Invalid API key")
                || error.to_string().contains("Unauthorized")
        );

        Ok(())
    }
}
