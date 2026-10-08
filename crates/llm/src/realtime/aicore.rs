//! Realtime sessions through AI Core: OAuth client credentials from the
//! provider config, the deployment from its `models` map.
//!
//! The WebSocket endpoint is the deployment's `deploymentUrl` (read from the
//! deployment resource on every connect, so a token refresh or redeployment
//! is picked up on reconnect) plus `/v1/realtime`.

use super::transport::{RealtimeEndpoint, WsConnector};
use super::{RealtimeConnection, RealtimeConnector};
use crate::auth::TokenManager;
use anyhow::{Context, Result, anyhow};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::info;

const RESOURCE_GROUP: &str = "default";

pub struct AiCoreConnector {
    client_id: String,
    client_secret: String,
    token_url: String,
    /// `https://…/v2/inference`, as in the provider config.
    api_base_url: String,
    deployment_id: String,
    /// Created on the first connect (it fetches a token right away).
    token_manager: OnceCell<Arc<TokenManager>>,
}

impl AiCoreConnector {
    pub fn new(
        client_id: String,
        client_secret: String,
        token_url: String,
        api_base_url: String,
        deployment_id: String,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            token_url,
            api_base_url,
            deployment_id,
            token_manager: OnceCell::new(),
        }
    }

    async fn token(&self) -> Result<String> {
        let manager = self
            .token_manager
            .get_or_try_init(|| {
                TokenManager::new(
                    self.client_id.clone(),
                    self.client_secret.clone(),
                    self.token_url.clone(),
                )
            })
            .await
            .context("AI Core authentication failed")?;
        manager.get_valid_token().await
    }

    /// The deployment resource's `deploymentUrl`.
    async fn deployment_url(&self, token: &str) -> Result<String> {
        let url = format!(
            "{}/deployments/{}",
            lm_base_url(&self.api_base_url),
            self.deployment_id
        );
        let response: serde_json::Value = reqwest::Client::new()
            .get(&url)
            .bearer_auth(token)
            .header("AI-Resource-Group", RESOURCE_GROUP)
            .send()
            .await
            .with_context(|| format!("Failed to read the AI Core deployment {url}"))?
            .error_for_status()
            .with_context(|| format!("AI Core deployment {} not readable", self.deployment_id))?
            .json()
            .await?;
        response
            .get("deploymentUrl")
            .and_then(|v| v.as_str())
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow!(
                    "AI Core deployment {} has no URL (status: {})",
                    self.deployment_id,
                    response
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                )
            })
    }
}

#[async_trait::async_trait]
impl RealtimeConnector for AiCoreConnector {
    async fn connect(&self) -> Result<RealtimeConnection> {
        let token = self.token().await?;
        let deployment_url = self.deployment_url(&token).await?;
        let url = realtime_url(&deployment_url);
        info!("AI Core realtime endpoint: {url}");
        WsConnector::new(RealtimeEndpoint {
            url,
            headers: vec![
                ("Authorization".to_string(), format!("Bearer {token}")),
                ("AI-Resource-Group".to_string(), RESOURCE_GROUP.to_string()),
            ],
        })
        .connect()
        .await
    }

    fn declares_model(&self) -> bool {
        false
    }
}

/// Where deployment resources live: `<scheme>://<host>/v2/lm`, whatever
/// path the configured inference URL has (`…/v2/inference`, `…/inference`).
fn lm_base_url(api_base_url: &str) -> String {
    let base = api_base_url.trim_end_matches('/');
    let host_end = base
        .find("://")
        .map(|scheme| {
            let rest = scheme + 3;
            base[rest..]
                .find('/')
                .map_or(base.len(), |slash| rest + slash)
        })
        .unwrap_or(base.len());
    format!("{}/v2/lm", &base[..host_end])
}

fn realtime_url(deployment_url: &str) -> String {
    let base = deployment_url.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/v1/realtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_resources_live_under_v2_lm_on_the_api_host() {
        for base in [
            "https://api.ai.example.com/v2/inference/",
            "https://api.ai.example.com/inference",
            "https://api.ai.example.com",
        ] {
            assert_eq!(
                lm_base_url(base),
                "https://api.ai.example.com/v2/lm",
                "{base}"
            );
        }
    }

    #[test]
    fn realtime_url_appends_the_path_on_a_websocket_scheme() {
        assert_eq!(
            realtime_url("https://api.ai.example.com/v2/inference/deployments/d1"),
            "wss://api.ai.example.com/v2/inference/deployments/d1/v1/realtime"
        );
    }
}
