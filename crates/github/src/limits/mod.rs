//! Staying inside GitHub's rate limits: recognising a refusal, tracking the
//! quota GitHub reports, and pausing a host after it refused.
//!
//! Times are milliseconds since the Unix epoch, the unit GitHub's
//! `x-ratelimit-reset` header counts in (as seconds).

mod pause;
mod quota;

pub(crate) use pause::HostPauses;
pub(crate) use quota::Quota;

use crate::error::GraphQlError;
use crate::transport::Headers;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub(crate) fn system_time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

/// GitHub refused for quota reasons. GraphQL answers HTTP 200 with a
/// `RATE_LIMITED` error (or an untyped one with spent quota headers); a
/// secondary limit is a 403 that says so in its body or headers.
pub(crate) fn is_rate_limited(
    status: u16,
    headers: &Headers,
    graphql_errors: Option<&[GraphQlError]>,
    body: &str,
) -> bool {
    let spent = headers.get("x-ratelimit-remaining") == Some("0");
    if status == 429 {
        return true;
    }
    if let Some(errors) = graphql_errors
        && (spent
            || errors.iter().any(|e| {
                e.is_kind("RATE_LIMITED") || {
                    let message = e.message.to_ascii_lowercase();
                    message.contains("rate limit exceeded")
                        || message.contains("rate limit already exceeded")
                }
            }))
    {
        return true;
    }
    status == 403
        && (spent
            || headers.get("retry-after").is_some()
            || body.to_ascii_lowercase().contains("rate limit"))
}

/// When a refused request may be retried: `retry-after`, else the quota
/// window's reset.
pub(crate) fn retry_at(headers: &Headers, now: u64) -> Option<u64> {
    if let Some(at) = headers
        .get("retry-after")
        .and_then(|value| retry_at_from_header(value, now))
    {
        return Some(at);
    }
    let reset = headers
        .get("x-ratelimit-reset")?
        .trim()
        .parse::<u64>()
        .ok()?
        * 1000;
    (reset > now).then_some(reset)
}

/// `Retry-After` as delay seconds or as an HTTP date.
pub(crate) fn retry_at_from_header(value: &str, now: u64) -> Option<u64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return value
            .parse::<u64>()
            .ok()
            .and_then(|s| s.checked_mul(1000))
            .and_then(|ms| now.checked_add(ms));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let at = u64::try_from(at.timestamp_millis()).ok()?;
    (at > now).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(kind: Option<&str>, message: &str) -> GraphQlError {
        GraphQlError {
            message: message.into(),
            kind: kind.map(str::to_string),
            path: Vec::new(),
        }
    }

    #[test]
    fn recognises_graphql_rate_limits_on_http_200() {
        let headers = Headers::new();
        let typed = [error(Some("RATE_LIMITED"), "API rate limit exceeded")];
        let untyped = [error(
            None,
            "API rate limit already exceeded for user ID 1.",
        )];
        let other = [error(Some("NOT_FOUND"), "Could not resolve")];
        assert!(is_rate_limited(200, &headers, Some(&typed), ""));
        assert!(is_rate_limited(200, &headers, Some(&untyped), ""));
        assert!(!is_rate_limited(200, &headers, Some(&other), ""));
        let spent = Headers::new().with("x-ratelimit-remaining", "0");
        assert!(is_rate_limited(200, &spent, Some(&other), ""));
        assert!(!is_rate_limited(200, &spent, None, ""));
    }

    #[test]
    fn recognises_primary_and_secondary_limits_on_rest() {
        let headers = Headers::new();
        assert!(is_rate_limited(429, &headers, None, ""));
        assert!(is_rate_limited(
            403,
            &headers,
            None,
            r#"{"message":"You have exceeded a secondary rate limit"}"#
        ));
        assert!(is_rate_limited(
            403,
            &Headers::new().with("retry-after", "60"),
            None,
            ""
        ));
        assert!(!is_rate_limited(
            403,
            &headers,
            None,
            r#"{"message":"Forbidden"}"#
        ));
    }

    #[test]
    fn parses_retry_after_seconds_and_http_dates() {
        let now = 1_000_000;
        assert_eq!(retry_at_from_header("30", now), Some(now + 30_000));
        // Wed, 21 Oct 2015 07:28:00 GMT
        let date = 1_445_412_480_000;
        assert_eq!(
            retry_at_from_header("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(date)
        );
        assert_eq!(
            retry_at_from_header("Wed, 21 Oct 2015 07:28:00 GMT", date + 1),
            None
        );
        assert_eq!(retry_at_from_header("soon", now), None);
    }

    #[test]
    fn retry_at_prefers_retry_after_over_the_reset() {
        let now = 1_000_000;
        let both = Headers::new()
            .with("retry-after", "5")
            .with("x-ratelimit-reset", "2000");
        assert_eq!(retry_at(&both, now), Some(now + 5_000));
        let reset = Headers::new().with("x-ratelimit-reset", "2000");
        assert_eq!(retry_at(&reset, now), Some(2_000_000));
        let past = Headers::new().with("x-ratelimit-reset", "500");
        assert_eq!(retry_at(&past, now), None);
    }
}
