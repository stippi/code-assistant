use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use tracing::info;

/// Handles HTTP communication with the LLM inference service
pub struct ApiClient {
    client: Client,
    base_url: String,
}

impl ApiClient {
    /// Creates a new ApiClient instance
    ///
    /// # Arguments
    /// * `base_url` - Base URL of the LLM inference service
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
        }
    }

    /// Makes a POST request to the LLM inference service
    ///
    /// # Arguments
    /// * `payload` - JSON payload for the request
    ///
    /// # Returns
    /// * `Result<Value>` - JSON response or an error
    pub async fn make_request(&self, payload: Value) -> Result<Value> {
        info!("Making API request");

        let response = self
            .client
            .post(&self.base_url)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        Ok(response)
    }
}
