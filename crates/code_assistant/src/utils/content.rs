use crate::persistence::DraftAttachment;
use llm::ContentBlock;
use std::time::SystemTime;

/// Generate a text summary from content blocks for UI display.
/// Images are shown as `[image/png]` etc., text blocks are joined with newlines.
pub fn text_summary_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            ContentBlock::Image { media_type, .. } => Some(format!("[{media_type}]")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn content_blocks_from(message: &str, attachments: &[DraftAttachment]) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    if !message.is_empty() {
        blocks.push(ContentBlock::new_text(message.to_owned()));
    }

    for attachment in attachments {
        match attachment {
            DraftAttachment::Image {
                content, mime_type, ..
            } => {
                blocks.push(ContentBlock::Image {
                    media_type: mime_type.clone(),
                    data: content.clone(),
                    start_time: Some(SystemTime::now()),
                    end_time: None,
                });
            }
            DraftAttachment::Text { content } => {
                blocks.push(ContentBlock::new_text(content.clone()));
            }
            DraftAttachment::File {
                content, filename, ..
            } => {
                blocks.push(ContentBlock::new_text(format!(
                    "File: {filename}\n{content}"
                )));
            }
        }
    }

    blocks
}
