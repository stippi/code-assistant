use crate::{ApiError, ApiErrorContext, RateLimitHandler, StreamingCallback, StreamingChunk};
use anyhow::Result;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// How long establishing a connection to an LLM endpoint may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a streaming request may wait for its response to start: the
/// response headers, and then the first chunk. Streaming servers answer right
/// away, so a longer wait means the request is stuck somewhere (VPN, proxy,
/// gateway). Nothing was shown yet at that point, so the request is retried.
pub(crate) const RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a streaming response may stay silent between two chunks once it
/// started. Generous, since a model may reason for minutes without emitting
/// anything, but bounded so a connection that silently died surfaces as an
/// error instead of leaving the agent waiting forever.
pub(crate) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Upper bound for short auxiliary requests (token fetches, lookups) whose
/// answer is small and should arrive within seconds.
pub(crate) const SHORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// HTTP client for LLM providers.
///
/// `reqwest::Client::new()` has no timeouts at all. A connect timeout catches
/// unreachable endpoints; TCP keepalive lets the OS notice dead peers. Reads are
/// bounded per request by [`send_request`] and [`crate::streaming::HttpChunkStream`],
/// since a total timeout would cut off long but healthy streams.
///
/// TLS goes through rustls. By default certificates are checked by the
/// platform verifier (the OS trust store); with only the `tls-webpki-roots`
/// feature the embedded Mozilla roots are used instead.
pub(crate) fn http_client() -> Client {
    let builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_keepalive(Duration::from_secs(30));
    #[cfg(all(feature = "tls-webpki-roots", not(feature = "tls-native-roots")))]
    let builder = builder.tls_certs_only(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|der| reqwest::Certificate::from_der(der).ok()),
    );
    builder.build().unwrap_or_else(|e| {
        warn!("Failed to build HTTP client with timeouts, using defaults: {e}");
        Client::new()
    })
}

/// Send a request, mapping transport failures to `ApiError::NetworkError` so
/// the retry logic picks them up. For streaming requests the wait for the
/// response headers is bounded by [`RESPONSE_START_TIMEOUT`]; non-streaming
/// requests only get their headers once the whole answer is generated, so they
/// are not bounded here.
pub(crate) async fn send_request(request: RequestBuilder, streaming: bool) -> Result<Response> {
    let response = if streaming {
        tokio::time::timeout(RESPONSE_START_TIMEOUT, request.send())
            .await
            .map_err(|_| {
                ApiError::NetworkError(format!(
                    "no response from the LLM within {}s",
                    RESPONSE_START_TIMEOUT.as_secs()
                ))
            })?
    } else {
        request.send().await
    };
    // reqwest's top-level message alone ("error sending request") hides the
    // cause, e.g. a connect timeout or DNS failure.
    response.map_err(|e| ApiError::NetworkError(error_chain(&e)).into())
}

/// Render an error with all its sources, separated by ": ".
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Check if an authentication error is transient and worth retrying.
/// Examples: expired JWT tokens that will be refreshed on the next attempt.
fn is_transient_auth_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("jwt is expired")
        || lower.contains("token expired")
        || lower.contains("token has expired")
        || lower.contains("expired token")
}

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
    let (api_error, rate_limits) = if let Some(ctx) = error.downcast_ref::<ApiErrorContext<T>>() {
        (&ctx.error, ctx.rate_limits.as_ref())
    } else if let Some(api_error) = error.downcast_ref::<ApiError>() {
        // Raised before a response arrived (connect failure, timeout while
        // waiting for headers), so there is no rate limit context.
        (api_error, None)
    } else {
        return false;
    };
    match api_error {
        ApiError::RateLimit(_) => {
            if let Some(rate_limits) = rate_limits {
                if attempts < max_retries {
                    // Get base delay from rate limit headers
                    let mut delay = rate_limits.get_retry_delay();

                    // Apply exponential backoff based on retry attempts
                    // Double the delay for each retry attempt after the first
                    if attempts > 0 {
                        let multiplier = 2u32.pow(attempts);
                        delay *= multiplier;
                    }
                    let delay_secs = delay.as_secs();
                    warn!(
                        "Rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                        attempts, max_retries, delay_secs
                    );

                    return wait_rate_limit(delay, streaming_callback).await;
                }
            } else {
                // Fallback if no rate limit info available
                if attempts < max_retries {
                    // Implement exponential backoff with a more appropriate base delay
                    // Start with 60 seconds for the first retry, then double for each subsequent attempt
                    let base_delay_secs = 60u64;
                    let delay = Duration::from_secs(base_delay_secs * 2u64.pow(attempts));
                    warn!(
                        "Rate limit hit but no timing info available (attempt {}/{}), using exponential backoff: {} seconds",
                        attempts,
                        max_retries,
                        delay.as_secs()
                    );
                    return wait_rate_limit(delay, streaming_callback).await;
                }
            }
        }
        ApiError::Authentication(msg) if is_transient_auth_error(msg) => {
            if attempts < max_retries {
                let delay = Duration::from_secs(2u64.pow(attempts.saturating_sub(1)));
                warn!(
                    "Transient auth error: {} (attempt {}/{}), retrying in {} seconds",
                    error,
                    attempts,
                    max_retries,
                    delay.as_secs()
                );
                return announce_retry(api_error, attempts, max_retries, delay, streaming_callback)
                    .await;
            }
        }
        ApiError::ServiceError(_) | ApiError::NetworkError(_) | ApiError::Overloaded(_) => {
            if attempts < max_retries {
                let delay = Duration::from_secs(2u64.pow(attempts.saturating_sub(1)));
                warn!(
                    "Error: {} (attempt {}/{}), retrying in {} seconds",
                    error,
                    attempts,
                    max_retries,
                    delay.as_secs()
                );
                return announce_retry(api_error, attempts, max_retries, delay, streaming_callback)
                    .await;
            }
        }
        _ => {
            warn!(
                "Unhandled error (attempt {}/{}): {:?}",
                attempts, max_retries, error
            );
        }
    }
    false
}

/// Wait out a rate limit, showing a countdown when there is a callback.
/// Returns false if the user cancelled while waiting.
async fn wait_rate_limit(delay: Duration, streaming_callback: Option<&StreamingCallback>) -> bool {
    let Some(callback) = streaming_callback else {
        sleep(delay).await;
        return true;
    };
    let delay_secs = delay.as_secs();
    let _ = callback(&StreamingChunk::RateLimit {
        seconds_remaining: delay_secs,
    });

    // Count down with precise timing
    let start_time = std::time::Instant::now();
    let mut next_update = start_time + Duration::from_secs(1);
    while start_time.elapsed() < delay {
        // Sleep until the next update time, accounting for callback execution time
        let now = std::time::Instant::now();
        if now < next_update {
            sleep(next_update - now).await;
        }

        let remaining_secs = delay_secs.saturating_sub(start_time.elapsed().as_secs());
        if callback(&StreamingChunk::RateLimit {
            seconds_remaining: remaining_secs,
        })
        .is_err()
        {
            // User requested streaming to cancel, exit the wait loop
            let _ = callback(&StreamingChunk::RateLimitClear);
            return false;
        }
        next_update += Duration::from_secs(1);
    }

    let _ = callback(&StreamingChunk::RateLimitClear);
    true
}

/// Tell the UI that the request is sent again, then wait `delay`.
/// Returns false if the user cancelled.
async fn announce_retry(
    error: &ApiError,
    attempts: u32,
    max_retries: u32,
    delay: Duration,
    streaming_callback: Option<&StreamingCallback>,
) -> bool {
    if let Some(callback) = streaming_callback {
        let chunk = StreamingChunk::Retrying {
            reason: error.to_string(),
            attempt: attempts + 1,
            max_attempts: max_retries,
        };
        if callback(&chunk).is_err() {
            return false;
        }
    }
    sleep(delay).await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct NoRateLimits;

    impl RateLimitHandler for NoRateLimits {
        fn from_response(_response: &Response) -> Self {
            NoRateLimits
        }
        fn get_retry_delay(&self) -> Duration {
            Duration::ZERO
        }
        fn log_status(&self) {}
    }

    #[tokio::test]
    async fn bare_network_errors_are_retried() {
        let error: anyhow::Error = ApiError::NetworkError("connection reset".into()).into();
        assert!(handle_retryable_error::<NoRateLimits>(&error, 0, 3, None).await);
        assert!(!handle_retryable_error::<NoRateLimits>(&error, 3, 3, None).await);

        // Context added on the way up must not hide the error.
        let error = error.context("LLM request failed");
        assert!(handle_retryable_error::<NoRateLimits>(&error, 1, 3, None).await);
    }

    #[tokio::test]
    async fn retries_are_announced_and_can_be_cancelled() {
        use std::sync::{Arc, Mutex};

        let seen = Arc::new(Mutex::new(Vec::new()));
        let callback: StreamingCallback = Box::new({
            let seen = seen.clone();
            move |chunk: &StreamingChunk| {
                if let StreamingChunk::Retrying {
                    reason,
                    attempt,
                    max_attempts,
                } = chunk
                {
                    seen.lock()
                        .unwrap()
                        .push((reason.clone(), *attempt, *max_attempts));
                }
                Ok(())
            }
        });
        let error: anyhow::Error = ApiError::NetworkError("connection reset".into()).into();
        assert!(handle_retryable_error::<NoRateLimits>(&error, 0, 3, Some(&callback)).await);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![("Network error: connection reset".to_string(), 1, 3)]
        );

        // A stop request (callback error) ends the retry loop.
        let cancelled: StreamingCallback =
            Box::new(|_: &StreamingChunk| Err(anyhow::anyhow!("stopped")));
        assert!(!handle_retryable_error::<NoRateLimits>(&error, 0, 3, Some(&cancelled)).await);
    }

    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let error = anyhow::anyhow!("LLM stream stalled: no data received for 300s");
        assert!(!handle_retryable_error::<NoRateLimits>(&error, 0, 3, None).await);

        let error: anyhow::Error = ApiError::InvalidRequest("bad".into()).into();
        assert!(!handle_retryable_error::<NoRateLimits>(&error, 0, 3, None).await);
    }

    #[test]
    fn error_chain_lists_all_causes() {
        let inner = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let outer = anyhow::Error::new(inner).context("error sending request");
        let outer: &dyn std::error::Error = outer.as_ref();
        assert_eq!(error_chain(outer), "error sending request: timed out");
    }
}
