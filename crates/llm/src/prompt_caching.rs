//! Shared cache-marker placement logic for providers with explicit prompt caching.
//!
//! Both the Anthropic client and the OpenAI Responses clients place explicit
//! cache breakpoints into the message history. The placement strategy is
//! provider-independent: markers anchor at message indices derived from the
//! stable (non-volatile) history prefix.

use crate::types::{Message, MessageRole};

/// Length of the stable history prefix.
///
/// Messages at and after the first volatile message are excluded because they
/// may change or disappear between requests, which would invalidate the
/// provider-side cached prefix.
fn stable_prefix_len(messages: &[Message]) -> usize {
    messages
        .iter()
        .position(|message| message.volatile)
        .unwrap_or(messages.len())
}

/// Get cache marker positions that move on every request. Used by the
/// Anthropic client, which resends the full message history each request.
///
/// Two markers are placed inside the stable prefix:
///
/// - the last stable message, so the entire prompt built in this request is
///   cached as early as possible (a write pays for itself as soon as the
///   prefix is reused once: 1.25x + 0.1x < 1x + 1x)
/// - the message directly before the last stable assistant message
///
/// Every request ends on a user message and the response is appended right
/// behind it as an assistant message, so the message before the last
/// assistant message is exactly where the previous request placed its leading
/// marker. The lookup there is guaranteed to hit and only the messages
/// appended since then are written — without tracking any state, and
/// regardless of how many messages arrived in between (tool results, pending
/// user messages).
pub fn per_request_marker_positions(messages: &[Message]) -> Vec<usize> {
    let stable_len = stable_prefix_len(messages);
    if stable_len == 0 {
        return vec![];
    }

    let leading = stable_len - 1;
    let trailing = messages[..stable_len]
        .iter()
        .rposition(|message| message.role == MessageRole::Assistant)
        .and_then(|assistant_index| assistant_index.checked_sub(1));

    match trailing {
        // `trailing < leading` always holds: the assistant message itself
        // sits between the two positions.
        Some(trailing) => vec![trailing, leading],
        None => vec![leading],
    }
}

/// Get cache marker positions quantized to blocks of five messages. Used by
/// the OpenAI Responses clients.
///
/// There the markers are stable *anchors* rather than the primary caching
/// mechanism: OpenAI's implicit mode already caches up to the latest message
/// on every request, and rarely-moving markers keep the WebSocket client's
/// incremental input intact (moving a marker changes an already-sent item and
/// forces a full resend).
///
/// 0-4 messages: no cache markers
/// 5-9 messages: marker at index 4
/// 10-14 messages: markers at indices 4 and 9
/// 15-19 messages: markers at indices 9 and 14
/// 20-24 messages: markers at indices 14 and 19
/// etc.
pub fn cache_marker_positions(messages: &[Message]) -> Vec<usize> {
    let stable_len = stable_prefix_len(messages);

    if stable_len < 5 {
        return vec![];
    }
    let remainder = stable_len % 5;
    let last_marker = stable_len - remainder;
    if last_marker > 5 {
        vec![last_marker - 6, last_marker - 1]
    } else {
        vec![last_marker - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trailing marker of each request must land on the leading marker
    /// position of the previous request, so that cache lookups always hit.
    #[test]
    fn per_request_markers_chain_across_requests() {
        // Request 1: a single user message → one marker
        let mut messages = vec![Message::new_user("u0")];
        assert_eq!(per_request_marker_positions(&messages), vec![0]);

        // Response appended, tool results form the next request
        messages.push(Message::new_assistant("a1"));
        messages.push(Message::new_user("u2"));
        assert_eq!(per_request_marker_positions(&messages), vec![0, 2]);

        // Next turn adds a pending user message on top — the trailing marker
        // still lands on the previous request's leading position (index 2)
        messages.push(Message::new_assistant("a3"));
        messages.push(Message::new_user("u4"));
        messages.push(Message::new_user("u5 (pending)"));
        assert_eq!(per_request_marker_positions(&messages), vec![2, 5]);
    }

    #[test]
    fn per_request_markers_respect_volatile_prefix() {
        let mut messages: Vec<Message> = (0..18)
            .map(|i| {
                if i % 2 == 0 {
                    Message::new_user(format!("u{i}"))
                } else {
                    Message::new_assistant(format!("a{i}"))
                }
            })
            .collect();
        assert_eq!(per_request_marker_positions(&messages), vec![16, 17]);

        // Volatile tail caps the markable prefix
        messages[12].volatile = true;
        assert_eq!(per_request_marker_positions(&messages), vec![10, 11]);

        // Everything volatile → no markers
        messages[0].volatile = true;
        assert!(per_request_marker_positions(&messages).is_empty());
    }

    #[test]
    fn per_request_markers_without_assistant_history() {
        assert!(per_request_marker_positions(&[]).is_empty());

        let messages: Vec<Message> = (0..3).map(|i| Message::new_user(format!("u{i}"))).collect();
        assert_eq!(per_request_marker_positions(&messages), vec![2]);
    }
}
