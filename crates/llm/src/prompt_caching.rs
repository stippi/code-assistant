//! Shared cache-marker placement logic for providers with explicit prompt caching.
//!
//! Both the Anthropic client and the OpenAI Responses clients place explicit
//! cache breakpoints into the message history. The placement strategy is
//! provider-independent: markers anchor at message indices derived from the
//! length of the stable (non-volatile) history prefix.

use crate::types::Message;

/// Get cache marker positions based on the stable prefix length.
///
/// Messages at and after the first volatile message are excluded because they
/// may change or disappear between requests, which would invalidate the
/// provider-side cached prefix.
///
/// 0-4 messages: no cache markers
/// 5-9 messages: marker at index 4
/// 10-14 messages: markers at indices 4 and 9
/// 15-19 messages: markers at indices 9 and 14
/// 20-24 messages: markers at indices 14 and 19
/// etc.
pub fn cache_marker_positions(messages: &[Message]) -> Vec<usize> {
    let stable_len = messages
        .iter()
        .position(|message| message.volatile)
        .unwrap_or(messages.len());

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
