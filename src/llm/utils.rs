use crate::llm::{ApiError, ApiErrorContext, RateLimitHandler};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Handle retryable errors and rate limiting for LLM providers.
/// Returns true if the error is retryable and we should continue the retry loop.
/// Returns false if we should exit the retry loop.
pub async fn handle_retryable_error<
    T: RateLimitHandler + std::fmt::Debug + Send + Sync + 'static,
>(
    error: &anyhow::Error,
    attempts: u32,
    max_retries: u32,
) -> bool {
    if let Some(ctx) = error.downcast_ref::<ApiErrorContext<T>>() {
        match &ctx.error {
            ApiError::RateLimit(_) => {
                if let Some(rate_limits) = &ctx.rate_limits {
                    if attempts < max_retries {
                        let delay = rate_limits.get_retry_delay();
                        warn!(
                            "Rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                            attempts,
                            max_retries,
                            delay.as_secs()
                        );
                        sleep(delay).await;
                        return true;
                    }
                } else {
                    // Fallback if no rate limit info available
                    if attempts < max_retries {
                        let delay = Duration::from_secs(2u64.pow(attempts - 1));
                        warn!(
                            "Rate limit hit but no timing info available (attempt {}/{}), using exponential backoff: {} seconds",
                            attempts,
                            max_retries,
                            delay.as_secs()
                        );
                        sleep(delay).await;
                        return true;
                    }
                }
            }
            ApiError::ServiceError(_) | ApiError::NetworkError(_) => {
                if attempts < max_retries {
                    let delay = Duration::from_secs(2u64.pow(attempts - 1));
                    warn!(
                        "Error: {} (attempt {}/{}), retrying in {} seconds",
                        error,
                        attempts,
                        max_retries,
                        delay.as_secs()
                    );
                    sleep(delay).await;
                    return true;
                }
            }
            _ => {
                warn!(
                    "Unhandled error (attempt {}/{}): {:?}",
                    attempts, max_retries, error
                );
            }
        }
    }
    false
}
