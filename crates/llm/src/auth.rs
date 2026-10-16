use crate::ApiError;
use anyhow::Result;
use base64::engine::{Engine, general_purpose};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;

pub struct TokenManager {
    client_id: String,
    client_secret: String,
    token_url: String,
    current_token: RwLock<Option<TokenInfo>>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

struct TokenInfo {
    token: String,
    expires_at: SystemTime,
}

impl TokenManager {
    pub async fn new(
        client_id: String,
        client_secret: String,
        token_url: String,
    ) -> Result<Arc<Self>> {
        tracing::debug!("Creating new TokenManager...");

        let manager = Arc::new(Self {
            client_id,
            client_secret,
            token_url,
            current_token: RwLock::new(None),
        });

        // Fetch initial token
        manager.refresh_token().await?;

        Ok(manager)
    }

    pub async fn get_valid_token(&self) -> Result<String> {
        // Check if we have a valid token
        if let Some(token_info) = self.current_token.read().await.as_ref()
            && SystemTime::now() < token_info.expires_at
        {
            return Ok(token_info.token.clone());
        }

        // If not, we need to fetch a new one
        self.refresh_token().await
    }

    async fn refresh_token(&self) -> Result<String> {
        tracing::debug!("Requesting new access token...");

        let client = crate::utils::http_client();
        let auth =
            general_purpose::STANDARD.encode(format!("{}:{}", self.client_id, self.client_secret));

        let res = client
            .post(&self.token_url)
            .header("Authorization", format!("Basic {auth}"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .timeout(crate::utils::SHORT_REQUEST_TIMEOUT)
            .send()
            .await
            // As a network error the provider's retry loop tries again.
            .map_err(|e| {
                ApiError::NetworkError(format!(
                    "token request failed: {}",
                    crate::utils::error_chain(&e)
                ))
            })?;

        let status = res.status();
        if !status.is_success() {
            let error_text = res.text().await.unwrap_or_default();
            let message = format!("Token request failed: {status} - {error_text}");
            if status.is_server_error() {
                return Err(ApiError::ServiceError(message).into());
            }
            anyhow::bail!(message);
        }

        let token_response = res.json::<TokenResponse>().await?;

        // Set expiry slightly before actual expiry to ensure we don't use expired tokens
        let expires_at =
            SystemTime::now() + Duration::from_secs(token_response.expires_in.saturating_sub(60));

        let token_info = TokenInfo {
            token: token_response.access_token.clone(),
            expires_at,
        };

        *self.current_token.write().await = Some(token_info);

        Ok(token_response.access_token)
    }
}
