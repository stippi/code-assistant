use crate::llm::{rate_limits::extract_rate_limit_header, types::RateLimitHandler};
use reqwest::Response;
use std::time::Duration;
use tracing::debug;

#[derive(Debug)]
pub struct OpenAIRateLimitInfo {
    requests_limit: Option<u32>,
    requests_remaining: Option<u32>,
    requests_reset: Option<Duration>,
    tokens_limit: Option<u32>,
    tokens_remaining: Option<u32>,
    tokens_reset: Option<Duration>,
}

impl RateLimitHandler for OpenAIRateLimitInfo {
    fn from_response(response: &Response) -> Self {
        let headers = response.headers();

        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {
            headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| {
                    // Parse OpenAI's duration format (e.g., "1s", "6m0s")
                    let mut seconds = 0u64;
                    let mut current_num = String::new();

                    for c in s.chars() {
                        match c {
                            '0'..='9' => current_num.push(c),
                            'm' => {
                                if let Ok(mins) = current_num.parse::<u64>() {
                                    seconds += mins * 60;
                                }
                                current_num.clear();
                            }
                            's' => {
                                if let Ok(secs) = current_num.parse::<u64>() {
                                    seconds += secs;
                                }
                                current_num.clear();
                            }
                            _ => current_num.clear(),
                        }
                    }
                    Some(Duration::from_secs(seconds))
                })
        }

        Self {
            requests_limit: extract_rate_limit_header(headers, "x-ratelimit-limit-requests"),
            requests_remaining: extract_rate_limit_header(headers, "x-ratelimit-remaining-requests"),
            requests_reset: parse_duration(headers, "x-ratelimit-reset-requests"),
            tokens_limit: extract_rate_limit_header(headers, "x-ratelimit-limit-tokens"),
            tokens_remaining: extract_rate_limit_header(headers, "x-ratelimit-remaining-tokens"),
            tokens_reset: parse_duration(headers, "x-ratelimit-reset-tokens"),
        }
    }

    fn get_retry_delay(&self) -> Duration {
        // Take the longer of the two reset times if both are present
        let mut delay = Duration::from_secs(2); // Default fallback

        if let Some(requests_reset) = self.requests_reset {
            delay = delay.max(requests_reset);
        }

        if let Some(tokens_reset) = self.tokens_reset {
            delay = delay.max(tokens_reset);
        }

        // Add a small buffer
        delay + Duration::from_secs(1)
    }

    fn log_status(&self) {
        debug!(
            "OpenAI Rate limits - Requests: {}/{} (reset in: {}s), Tokens: {}/{} (reset in: {}s)",
            self.requests_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.requests_limit
                .map_or("?".to_string(), |l| l.to_string()),
            self.requests_reset.map_or(0, |d| d.as_secs()),
            self.tokens_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.tokens_limit.map_or("?".to_string(), |l| l.to_string()),
            self.tokens_reset.map_or(0, |d| d.as_secs()),
        );
    }
}
