use crate::llm::{types::*, ApiError, ApiErrorContext};
use anyhow::Result;
use reqwest::Response;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Helper for implementing retry logic with rate limits
pub async fn send_with_retry<T, F, R, Fut>(
    operation: F,
    max_retries: u32,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(T, R)>>,
    R: RateLimitHandler + 'static,
{
    let mut attempts = 0;

    loop {
        match operation().await {
            Ok((response, rate_limits)) => {
                rate_limits.log_status();
                return Ok(response);
            }
            Err(e) => {
                let rate_limits = e
                    .downcast_ref::<ApiErrorContext<R>>()
                    .and_then(|ctx| ctx.rate_limits.as_ref());

                match e.downcast_ref::<ApiError>() {
                    Some(ApiError::RateLimit(_)) => {
                        if let Some(rate_limits) = rate_limits {
                            if attempts < max_retries {
                                attempts += 1;
                                let delay = rate_limits.get_retry_delay();
                                warn!(
                                    "Rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                                    attempts,
                                    max_retries,
                                    delay.as_secs()
                                );
                                sleep(delay).await;
                                continue;
                            }
                        }
                    }
                    Some(ApiError::ServiceError(_)) | Some(ApiError::NetworkError(_)) => {
                        if attempts < max_retries {
                            attempts += 1;
                            let delay = Duration::from_secs(2u64.pow(attempts - 1));
                            warn!(
                                "Error: {} (attempt {}/{}), retrying in {} seconds",
                                e,
                                attempts,
                                max_retries,
                                delay.as_secs()
                            );
                            sleep(delay).await;
                            continue;
                        }
                    }
                    _ => {} // Don't retry other types of errors
                }
                return Err(e);
            }
        }
    }
}

/// Helper for extracting rate limit information from response headers
pub fn extract_rate_limit_header<T: std::str::FromStr>(
    headers: &reqwest::header::HeaderMap,
    name: &str,
) -> Option<T> {
    headers
        .get(name)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok())
}

/// Helper for handling error responses with rate limit information
pub async fn check_response_error<R: RateLimitHandler + 'static>(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let rate_limits = R::from_response(&response);
    let response_text = response
        .text()
        .await
        .map_err(|e| ApiError::NetworkError(e.to_string()))?;

    let error = ApiError::Unknown(format!("Status {}: {}", status, response_text));

    Err(ApiErrorContext {
        error,
        rate_limits: Some(rate_limits),
    }
    .into())
}
