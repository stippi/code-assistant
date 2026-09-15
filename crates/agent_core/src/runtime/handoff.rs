//! The hand-off message a compacted conversation resumes from.
//!
//! After compaction the prompt no longer contains the exchanges before the
//! summary. The summary is rendered as a single user message that frames it
//! as a hand-off from a previous instance and carries the user's own
//! messages verbatim, so what was asked for survives the compaction.
use llm::{ContentBlock, Message, MessageContent, MessageRole};

/// Rough budget for the verbatim user messages, in characters (about 20k
/// tokens). The newest messages take precedence.
const USER_MESSAGES_CHAR_BUDGET: usize = 80_000;

const PREAMBLE: &str = "Another instance of this assistant was working in this session and \
reached the context limit. It wrote the hand-off below. The user's messages are \
reproduced verbatim so nothing about what they asked for is lost. Build on the \
work already done instead of repeating it; the workspace reflects everything the \
previous instance did.";

/// The verbatim text of the real user messages among `messages`. Tool-result
/// messages and earlier compaction summaries share the user role but are not
/// user messages; images are dropped.
pub(super) fn user_message_texts<'a>(messages: impl Iterator<Item = &'a Message>) -> Vec<String> {
    messages
        .filter(|message| message.role == MessageRole::User && !message.is_compaction_summary)
        .filter_map(|message| match &message.content {
            MessageContent::Text(text) => Some(text.clone()),
            MessageContent::Structured(blocks) => {
                let text = blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.trim().is_empty()).then_some(text)
            }
        })
        .collect()
}

/// Renders the hand-off message from the user's messages and the summary the
/// previous instance wrote.
pub(super) fn render_handoff(user_messages: &[String], summary: &str) -> String {
    render_handoff_within(user_messages, summary, USER_MESSAGES_CHAR_BUDGET)
}

fn render_handoff_within(user_messages: &[String], summary: &str, budget: usize) -> String {
    let mut remaining = budget;
    let mut kept = 0;
    for message in user_messages.iter().rev() {
        if message.len() > remaining {
            break;
        }
        remaining -= message.len();
        kept += 1;
    }
    let omitted = user_messages.len() - kept;

    let mut out = String::new();
    out.push_str("<handoff>\n");
    out.push_str(PREAMBLE);
    out.push_str("\n\n<user_messages>\n");
    if omitted > 0 {
        out.push_str(&format!("({omitted} earlier messages omitted)\n"));
    }
    for (index, message) in user_messages.iter().enumerate().skip(omitted) {
        out.push_str(&format!(
            "<message index=\"{}\">\n{}\n</message>\n",
            index + 1,
            message.trim()
        ));
    }
    out.push_str("</user_messages>\n\n<summary>\n");
    let summary = summary.trim();
    out.push_str(if summary.is_empty() {
        "(no summary available)"
    } else {
        summary
    });
    out.push_str("\n</summary>\n</handoff>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_texts_skips_tool_results_and_summaries() {
        let messages = [
            Message::new_user("Explain the compaction feature"),
            Message::new_assistant("Looking."),
            Message::new_user_content(vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: llm::ToolResultContent::text("file contents"),
                is_error: None,
                start_time: None,
                end_time: None,
            }]),
            Message {
                content: MessageContent::Text("old summary".into()),
                is_compaction_summary: true,
                ..Default::default()
            },
            Message::new_user_content(vec![
                ContentBlock::new_text("Here is a screenshot"),
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "aaaa".into(),
                    start_time: None,
                    end_time: None,
                },
            ]),
        ];

        assert_eq!(
            user_message_texts(messages.iter()),
            vec![
                "Explain the compaction feature".to_string(),
                "Here is a screenshot".to_string()
            ]
        );
    }

    #[test]
    fn handoff_carries_user_messages_verbatim_before_the_summary() {
        let rendered = render_handoff(
            &["First ask".to_string(), "Second ask".to_string()],
            "Did A, B remains",
        );

        let messages_at = rendered.find("<user_messages>").unwrap();
        let summary_at = rendered.find("<summary>").unwrap();
        assert!(rendered.starts_with("<handoff>\n"));
        assert!(rendered.ends_with("</summary>\n</handoff>"));
        assert!(messages_at < summary_at);
        assert!(rendered.contains("<message index=\"1\">\nFirst ask\n</message>"));
        assert!(rendered.contains("<message index=\"2\">\nSecond ask\n</message>"));
        assert!(rendered.contains("<summary>\nDid A, B remains\n</summary>"));
    }

    #[test]
    fn handoff_keeps_the_newest_user_messages_within_the_budget() {
        let messages = ["x".repeat(30), "y".repeat(30), "z".repeat(30)];
        let rendered = render_handoff_within(&messages, "summary", 70);

        assert!(rendered.contains("(1 earlier messages omitted)"));
        assert!(!rendered.contains(&"x".repeat(30)));
        assert!(rendered.contains("<message index=\"2\">"));
        assert!(rendered.contains("<message index=\"3\">"));
    }

    #[test]
    fn handoff_marks_a_missing_summary() {
        let rendered = render_handoff(&[], "  ");
        assert!(rendered.contains("<summary>\n(no summary available)\n</summary>"));
    }
}
