use crate::persistence::DraftAttachment;
use llm::ContentBlock;

pub fn content_blocks_from(message: &str, attachments: &[DraftAttachment]) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    if !message.is_empty() {
        blocks.push(ContentBlock::new_text(message.to_owned()));
    }

    for attachment in attachments {
        match attachment {
            DraftAttachment::Image { content, mime_type } => {
                blocks.push(ContentBlock::Image {
                    media_type: mime_type.clone(),
                    data: content.clone(),
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
