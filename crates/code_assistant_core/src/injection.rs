//! Instructions the core appends to a user message as extra text blocks: the
//! body of an invoked skill (`<skill>`), the request to write a hand-off
//! (`<hand-off-request>`). The model reads them together with what the user
//! typed; transcripts, the edit context and pending-message summaries show
//! only the typed text.

use llm::{ContentBlock, Message, MessageContent, MessageRole};
use std::borrow::Cow;

/// The tags an injected block is wrapped in.
const TAGS: &[&str] = &["skill", "hand-off-request"];

/// Wrap `body` as an injected block tagged `tag` (one of [`TAGS`]).
pub(crate) fn wrap(tag: &str, body: &str) -> String {
    debug_assert!(TAGS.contains(&tag), "unknown injection tag {tag}");
    format!("<{tag}>\n{body}\n</{tag}>")
}

/// Whether `text` is a block rendered by [`wrap`].
pub fn is_injection(text: &str) -> bool {
    TAGS.iter().any(|tag| {
        text.strip_prefix(&format!("<{tag}>\n"))
            .is_some_and(|rest| rest.ends_with(&format!("\n</{tag}>")))
    })
}

/// Whether `block` is a text block rendered by [`wrap`].
pub fn is_injection_block(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Text { text, .. } if is_injection(text))
}

/// The user message as it should be shown: without injected blocks. Borrows
/// when there is nothing to strip.
pub fn without_injections(message: &Message) -> Cow<'_, Message> {
    let MessageContent::Structured(blocks) = &message.content else {
        return Cow::Borrowed(message);
    };
    if message.role != MessageRole::User || !blocks.iter().any(is_injection_block) {
        return Cow::Borrowed(message);
    }
    let mut stripped = message.clone();
    stripped.content = MessageContent::Structured(
        blocks
            .iter()
            .filter(|b| !is_injection_block(b))
            .cloned()
            .collect(),
    );
    Cow::Owned(stripped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_blocks_are_recognized() {
        assert!(is_injection(&wrap("skill", "Do the thing.")));
        assert!(is_injection(&wrap("hand-off-request", "Write it.")));
        assert!(!is_injection("/review focus on auth"));
        assert!(!is_injection("<other>\nx\n</other>"));
        assert!(!is_injection("<skill>\nunterminated"));
    }

    #[test]
    fn strips_injected_blocks_from_user_messages_only() {
        let message = Message::new_user_content(vec![
            ContentBlock::new_text("/review focus on auth"),
            ContentBlock::new_text(wrap("skill", "Do the thing.")),
        ]);
        let stripped = without_injections(&message);
        let MessageContent::Structured(blocks) = &stripped.content else {
            panic!("expected structured content");
        };
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0], ContentBlock::Text { text, .. } if text == "/review focus on auth")
        );

        let plain = Message::new_user("hello");
        assert!(matches!(without_injections(&plain), Cow::Borrowed(_)));

        let mut assistant = message.clone();
        assistant.role = MessageRole::Assistant;
        assert!(matches!(without_injections(&assistant), Cow::Borrowed(_)));
    }
}
