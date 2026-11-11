//! Counting and limiting the images a request carries. Some providers reject
//! requests with more than a fixed number of images (Azure OpenAI: 10 or 50,
//! depending on the model), wherever they sit in the history.

use llm::{ContentBlock, Message, MessageContent, ToolResultContent};

/// Stands in for an image left out of a request.
const OMITTED_IMAGE_NOTE: &str = "[Image omitted: the request reached the model's image limit]";

/// Images in user attachments and tool results.
pub(super) fn count_in_blocks(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Image { .. } => 1,
            ContentBlock::ToolResult { content, .. } => content.images().len(),
            _ => 0,
        })
        .sum()
}

pub(super) fn count(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| match &message.content {
            MessageContent::Structured(blocks) => count_in_blocks(blocks),
            MessageContent::Text(_) => 0,
        })
        .sum()
}

/// Keeps the first `keep` images and replaces every later one with a note.
/// Dropping from the end leaves the request's prefix, and with it the
/// provider's prompt cache, untouched. Returns how many were dropped.
pub(super) fn keep_first(messages: &mut [Message], keep: usize) -> usize {
    let mut seen = 0;
    let mut dropped = 0;
    for message in messages {
        let MessageContent::Structured(blocks) = &mut message.content else {
            continue;
        };
        for block in blocks.iter_mut() {
            match block {
                ContentBlock::Image { .. } => {
                    seen += 1;
                    if seen > keep {
                        *block = ContentBlock::new_text(OMITTED_IMAGE_NOTE);
                        dropped += 1;
                    }
                }
                ContentBlock::ToolResult { content, .. } if content.has_images() => {
                    let available = keep.saturating_sub(seen);
                    let images = content.images();
                    seen += images.len();
                    if images.len() > available {
                        let omitted = images.len() - available;
                        let text = format!("{}\n{}", content.text_content(), omitted_note(omitted));
                        *content =
                            ToolResultContent::with_images(text, images[..available].to_vec());
                        dropped += omitted;
                    }
                }
                _ => {}
            }
        }
    }
    dropped
}

fn omitted_note(count: usize) -> String {
    if count == 1 {
        OMITTED_IMAGE_NOTE.to_string()
    } else {
        format!("[{count} images omitted: the request reached the model's image limit]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::ToolResultImage;

    fn image() -> ContentBlock {
        ContentBlock::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
            start_time: None,
            end_time: None,
        }
    }

    fn tool_result(id: &str, images: usize) -> ContentBlock {
        ContentBlock::new_tool_result_with_images(
            id,
            "screenshot",
            (0..images)
                .map(|_| ToolResultImage {
                    media_type: "image/png".into(),
                    base64_data: "AAAA".into(),
                })
                .collect(),
        )
    }

    fn messages() -> Vec<Message> {
        vec![
            Message::new_user_content(vec![ContentBlock::new_text("look"), image()]),
            Message::new_assistant("ok"),
            Message::new_user_content(vec![tool_result("a", 2), tool_result("b", 1)]),
        ]
    }

    #[test]
    fn counts_attachments_and_tool_result_images() {
        assert_eq!(count(&messages()), 4);
    }

    #[test]
    fn keeps_the_oldest_images_and_notes_the_dropped_ones() {
        let mut messages = messages();
        assert_eq!(keep_first(&mut messages, 2), 2);
        assert_eq!(count(&messages), 2);

        let MessageContent::Structured(blocks) = &messages[2].content else {
            panic!("structured tool results expected");
        };
        let ContentBlock::ToolResult { content, .. } = &blocks[0] else {
            panic!("tool result expected");
        };
        assert_eq!(content.images().len(), 1);
        assert!(content.text_content().ends_with(OMITTED_IMAGE_NOTE));
        let ContentBlock::ToolResult { content, .. } = &blocks[1] else {
            panic!("tool result expected");
        };
        assert!(!content.has_images());
    }

    #[test]
    fn leaves_requests_within_the_limit_alone() {
        let mut messages = messages();
        let before = format!("{messages:?}");
        assert_eq!(keep_first(&mut messages, 4), 0);
        assert_eq!(format!("{messages:?}"), before);
    }
}
