//! Recovery policy: classifies failed LLM requests by provider error text.

use crate::agent::hooks::{RecoveryAction, RecoveryPolicy};
use std::time::Duration;

/// Maximum number of retries for transient streaming errors (e.g. HTTP chunk
/// errors, connection resets).
const MAX_STREAMING_RETRIES: u32 = 2;

/// Base delay between streaming retries (doubles on each attempt).
const STREAMING_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

pub struct DefaultRecovery;

impl RecoveryPolicy for DefaultRecovery {
    fn classify(&self, error: &anyhow::Error, completed_retries: u32) -> RecoveryAction {
        if is_prompt_too_long_error(error) {
            return RecoveryAction::ReduceContext;
        }
        if is_retryable_streaming_error(error) && completed_retries < MAX_STREAMING_RETRIES {
            return RecoveryAction::RetryStream {
                delay: STREAMING_RETRY_BASE_DELAY * 2u32.saturating_pow(completed_retries),
                attempt: completed_retries + 1,
                max_attempts: MAX_STREAMING_RETRIES,
            };
        }
        RecoveryAction::Fail
    }
}

/// Check if an error from the LLM provider indicates the prompt exceeded the
/// model's context limit.
fn is_prompt_too_long_error(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    // Anthropic patterns
    msg.contains("prompt is too long")
        || msg.contains("request size exceeds")
        || msg.contains("exceed context limit")
        || msg.contains("exceeds model context")
        // OpenAI patterns
        || msg.contains("context_length_exceeded")
        || msg.contains("maximum context length")
        // Generic patterns
        || msg.contains("too many tokens")
        || msg.contains("request too large")
}

/// Check if an error from the LLM provider is a transient streaming/connection
/// error that is safe to retry (the request itself was valid, only the
/// transport failed mid-stream).
fn is_retryable_streaming_error(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    // HTTP chunked transfer errors
    msg.contains("http chunk error")
        || msg.contains("chunk size line")
        || msg.contains("unexpected eof")
        // hyper / reqwest body errors
        || msg.contains("error reading a body from connection")
        || msg.contains("request or response body error")
        // Connection-level errors
        || msg.contains("connection reset")
        || msg.contains("connection closed")
        || msg.contains("broken pipe")
        || msg.contains("connection abort")
        // Timeout errors (read timeouts, not connect timeouts)
        || msg.contains("operation timed out")
        || msg.contains("timed out reading")
        // Server errors that are transient
        || msg.contains("502 bad gateway")
        || msg.contains("503 service")
        || msg.contains("529")
        || msg.contains("overloaded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_prompt_reduces_context() {
        let err = anyhow::anyhow!("Invalid request: prompt is too long: 500000 tokens");
        assert!(matches!(
            DefaultRecovery.classify(&err, 0),
            RecoveryAction::ReduceContext
        ));
    }

    #[test]
    fn transient_stream_error_retries_with_backoff() {
        let err = anyhow::anyhow!("error reading a body from connection: connection reset");
        match DefaultRecovery.classify(&err, 0) {
            RecoveryAction::RetryStream {
                delay,
                attempt,
                max_attempts,
            } => {
                assert_eq!(delay, Duration::from_secs(2));
                assert_eq!(attempt, 1);
                assert_eq!(max_attempts, MAX_STREAMING_RETRIES);
            }
            _ => panic!("expected RetryStream"),
        }
        match DefaultRecovery.classify(&err, 1) {
            RecoveryAction::RetryStream { delay, attempt, .. } => {
                assert_eq!(delay, Duration::from_secs(4));
                assert_eq!(attempt, 2);
            }
            _ => panic!("expected RetryStream"),
        }
    }

    #[test]
    fn retries_are_bounded() {
        let err = anyhow::anyhow!("connection reset");
        assert!(matches!(
            DefaultRecovery.classify(&err, MAX_STREAMING_RETRIES),
            RecoveryAction::Fail
        ));
    }

    #[test]
    fn unknown_errors_fail() {
        let err = anyhow::anyhow!("invalid api key");
        assert!(matches!(
            DefaultRecovery.classify(&err, 0),
            RecoveryAction::Fail
        ));
    }
}
