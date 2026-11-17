//! Recovery policy: classifies failed LLM requests by provider error text.

use agent_core::hooks::{RecoveryAction, RecoveryPolicy};
use agent_core::types::EmptyResponse;
use llm::RetryPolicy;
use std::time::Duration;

/// Wait before the first retry of a broken stream or an empty answer
/// (doubles on each further one, up to the policy's maximum).
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Recovers from oversized prompts by compacting, and sends a request again
/// after a broken stream or an empty answer, as often as `policy` allows.
pub struct DefaultRecovery {
    policy: RetryPolicy,
}

impl DefaultRecovery {
    pub fn new(policy: RetryPolicy) -> Self {
        Self { policy }
    }
}

impl RecoveryPolicy for DefaultRecovery {
    fn classify(&self, error: &anyhow::Error, completed_retries: u32) -> RecoveryAction {
        if let Some(limit) = image_limit_error(error) {
            return RecoveryAction::TooManyImages { limit };
        }
        if is_prompt_too_long_error(error) {
            return RecoveryAction::ReduceContext;
        }
        let retryable = error.is::<EmptyResponse>() || is_retryable_streaming_error(error);
        if retryable && self.policy.allows_retry(completed_retries) {
            return RecoveryAction::RetryStream {
                delay: self.policy.backoff(RETRY_BASE_DELAY, completed_retries),
                attempt: completed_retries + 1,
                max_attempts: self.policy.max_attempts(),
            };
        }
        RecoveryAction::Fail
    }
}

/// Whether the provider rejected the request for its number of images, and
/// the limit when the error names it:
/// - OpenAI: "Exceeded maximum number of images (50) allowed in the
///   request."
/// - Azure OpenAI: "Too many images in request: 20, maximum allowed: 10"
fn image_limit_error(error: &anyhow::Error) -> Option<Option<usize>> {
    let msg = error.to_string().to_lowercase();
    if !(msg.contains("number of images") || msg.contains("too many images")) {
        return None;
    }
    let number_after = |marker: &str| {
        let rest = &msg[msg.find(marker)? + marker.len()..];
        let rest = rest.trim_start_matches(|c: char| !c.is_ascii_digit());
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<usize>().ok().filter(|limit| *limit > 0)
    };
    Some(number_after("maximum allowed").or_else(|| number_after("maximum number of images")))
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
        // No data for a while (`llm::streaming`)
        || msg.contains("stream stalled")
        // Server errors that are transient
        || msg.contains("502 bad gateway")
        || msg.contains("503 service")
        || msg.contains("529")
        || msg.contains("overloaded")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_retries() -> DefaultRecovery {
        DefaultRecovery::new(RetryPolicy {
            retry_forever: false,
            max_retries: 2,
            max_delay_secs: 300,
        })
    }

    #[test]
    fn oversized_prompt_reduces_context() {
        let err = anyhow::anyhow!("Invalid request: prompt is too long: 500000 tokens");
        assert!(matches!(
            two_retries().classify(&err, 0),
            RecoveryAction::ReduceContext
        ));
    }

    #[test]
    fn transient_stream_error_retries_with_backoff() {
        let err = anyhow::anyhow!("error reading a body from connection: connection reset");
        match two_retries().classify(&err, 0) {
            RecoveryAction::RetryStream {
                delay,
                attempt,
                max_attempts,
            } => {
                assert_eq!(delay, Duration::from_secs(2));
                assert_eq!(attempt, 1);
                assert_eq!(max_attempts, Some(2));
            }
            _ => panic!("expected RetryStream"),
        }
        match two_retries().classify(&err, 1) {
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
            two_retries().classify(&err, 2),
            RecoveryAction::Fail
        ));
    }

    #[test]
    fn image_limit_errors_compact_with_the_named_limit() {
        let classify = |message: &str| two_retries().classify(&anyhow::anyhow!("{message}"), 0);
        let openai = r#"Invalid request: {"error":{"message":"Exceeded maximum number of images (50) allowed in the request.","param":"input","type":"invalid_request_error"}}"#;
        assert!(matches!(
            classify(openai),
            RecoveryAction::TooManyImages { limit: Some(50) }
        ));
        assert!(matches!(
            classify("Too many images in request: 20, maximum allowed: 10"),
            RecoveryAction::TooManyImages { limit: Some(10) }
        ));
        assert!(matches!(
            classify("request too large: too many images"),
            RecoveryAction::TooManyImages { limit: None }
        ));
    }

    #[test]
    fn empty_answers_and_stalled_streams_are_retried() {
        for err in [
            anyhow::Error::from(EmptyResponse),
            anyhow::anyhow!("LLM stream stalled: no data received for 300s"),
        ] {
            assert!(
                matches!(
                    two_retries().classify(&err, 0),
                    RecoveryAction::RetryStream { attempt: 1, .. }
                ),
                "{err}"
            );
        }
    }

    #[test]
    fn retrying_forever_keeps_retrying_with_capped_delay() {
        let recovery = DefaultRecovery::new(RetryPolicy {
            retry_forever: true,
            max_retries: 0,
            max_delay_secs: 30,
        });
        match recovery.classify(&EmptyResponse.into(), 100) {
            RecoveryAction::RetryStream {
                delay,
                attempt,
                max_attempts,
            } => {
                assert_eq!(delay, Duration::from_secs(30));
                assert_eq!(attempt, 101);
                assert_eq!(max_attempts, None);
            }
            _ => panic!("expected RetryStream"),
        }
    }

    #[test]
    fn unknown_errors_fail() {
        let err = anyhow::anyhow!("invalid api key");
        assert!(matches!(
            two_retries().classify(&err, 0),
            RecoveryAction::Fail
        ));
    }
}
