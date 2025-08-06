use crate::{ApiError, ApiErrorContext, RateLimitHandler, StreamingCallback, StreamingChunk};
use anyhow::Result;
use reqwest::{Response, StatusCode};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Check response error and extract rate limit information.
/// Returns Ok(Response) if successful, or an error with rate limit context if not.
pub async fn check_response_error<T: RateLimitHandler + std::fmt::Debug + Send + Sync + 'static>(
    response: Response,
) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let rate_limits = T::from_response(&response);
    let response_text = response
        .text()
        .await
        .map_err(|e| ApiError::NetworkError(e.to_string()))?;

    let error = match status {
        StatusCode::TOO_MANY_REQUESTS => ApiError::RateLimit(response_text),
        StatusCode::UNAUTHORIZED => ApiError::Authentication(response_text),
        StatusCode::BAD_REQUEST => ApiError::InvalidRequest(response_text),
        status if status.is_server_error() => ApiError::ServiceError(response_text),
        _ => ApiError::Unknown(format!("Status {status}: {response_text}")),
    };

    Err(ApiErrorContext {
        error,
        rate_limits: Some(rate_limits),
    }
    .into())
}

/// Handle retryable errors and rate limiting for LLM providers.
/// Returns true if the error is retryable and we should continue the retry loop.
/// Returns false if we should exit the retry loop.
///
/// If a streaming_callback is provided, rate limit notifications will be sent to the UI.
pub async fn handle_retryable_error<
    T: RateLimitHandler + std::fmt::Debug + Send + Sync + 'static,
>(
    error: &anyhow::Error,
    attempts: u32,
    max_retries: u32,
    streaming_callback: Option<&StreamingCallback>,
) -> bool {
    if let Some(ctx) = error.downcast_ref::<ApiErrorContext<T>>() {
        match &ctx.error {
            ApiError::RateLimit(_) => {
                if let Some(rate_limits) = &ctx.rate_limits {
                    if attempts < max_retries {
                        let delay = rate_limits.get_retry_delay();
                        let delay_secs = delay.as_secs();
                        warn!(
                            "Rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                            attempts, max_retries, delay_secs
                        );

                        // Send rate limit notification if callback is available
                        if let Some(callback) = streaming_callback {
                            let _ = callback(&StreamingChunk::RateLimit {
                                seconds_remaining: delay_secs,
                            });

                            // Start a countdown with precise timing
                            let start_time = std::time::Instant::now();
                            let mut next_update = start_time + Duration::from_secs(1);

                            while start_time.elapsed() < delay {
                                // Sleep until the next update time, accounting for callback execution time
                                let now = std::time::Instant::now();
                                if now < next_update {
                                    sleep(next_update - now).await;
                                }

                                // Calculate remaining time based on actual elapsed time
                                let elapsed = start_time.elapsed();
                                let remaining_secs = delay_secs.saturating_sub(elapsed.as_secs());

                                if callback(&StreamingChunk::RateLimit {
                                    seconds_remaining: remaining_secs,
                                })
                                .is_err()
                                {
                                    // User requested streaming to cancel, exit the wait loop
                                    let _ = callback(&StreamingChunk::RateLimitClear);
                                    return false;
                                }

                                // Schedule next update
                                next_update += Duration::from_secs(1);
                            }

                            // Clear the rate limit notification
                            let _ = callback(&StreamingChunk::RateLimitClear);
                        } else {
                            // No callback, just wait
                            sleep(delay).await;
                        }

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
            ApiError::ServiceError(_) | ApiError::NetworkError(_) | ApiError::Overloaded(_) => {
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
