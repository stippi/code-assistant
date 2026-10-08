use crate::persistence::DraftAttachment;
use llm::ContentBlock;
use std::path::Path;
use std::time::SystemTime;

pub use agent_core::text_summary_from_blocks;

/// The content blocks of a user message. Line comments name their files
/// relative to `project_root` where they lie inside it.
pub fn content_blocks_from(
    message: &str,
    attachments: &[DraftAttachment],
    project_root: Option<&Path>,
) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    if !message.is_empty() {
        blocks.push(ContentBlock::new_text(message.to_owned()));
    }

    for attachment in attachments {
        match attachment {
            DraftAttachment::Image {
                content, mime_type, ..
            } => {
                // Cap oversized user screenshots/attachments once, here at the
                // point they enter the conversation, so the bounded version is
                // what gets stored and re-sent on later turns.
                let (media_type, data) = match tools_core::cap_base64_image(
                    mime_type,
                    content,
                    tools_core::MAX_IMAGE_EDGE,
                ) {
                    Some((media_type, data)) => (media_type, data),
                    None => (mime_type.clone(), content.clone()),
                };
                blocks.push(ContentBlock::Image {
                    media_type,
                    data,
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
            DraftAttachment::LineComments { comments } if !comments.is_empty() => {
                let comments = match project_root {
                    Some(root) => crate::line_comments::relative_to(comments, root),
                    None => comments.clone(),
                };
                blocks.push(ContentBlock::new_text(crate::line_comments::render(
                    &comments,
                )));
            }
            DraftAttachment::LineComments { .. } => {}
        }
    }

    blocks
}
