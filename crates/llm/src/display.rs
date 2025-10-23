use crate::types::{ContentBlock, Message, MessageContent};
use std::fmt;

impl fmt::Display for ContentBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentBlock::Text { text, .. } => {
                writeln!(f, "Text: {}", text.replace('\n', "\n    "))
            }
            ContentBlock::Image {
                media_type, data, ..
            } => {
                let data_preview = if data.len() > 50 {
                    format!("{}...", &data[..50])
                } else {
                    data.clone()
                };
                writeln!(
                    f,
                    "Image: media_type={}, data_length={}, data_preview={}",
                    media_type,
                    data.len(),
                    data_preview
                )
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                writeln!(f, "ToolUse: id={id}, name={name}")?;
                writeln!(
                    f,
                    "  Input: {}",
                    serde_json::to_string_pretty(input)
                        .unwrap_or_else(|_| input.to_string())
                        .replace('\n', "\n  ")
                )
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } => {
                let error_suffix = if let Some(is_err) = is_error {
                    if *is_err {
                        " (ERROR)"
                    } else {
                        ""
                    }
                } else {
                    ""
                };
                writeln!(f, "ToolResult: tool_use_id={tool_use_id}{error_suffix}")?;
                writeln!(f, "  Content: {}", content.replace('\n', "\n  "))
            }
            ContentBlock::Thinking {
                thinking,
                signature,
                ..
            } => {
                writeln!(f, "Thinking: signature={signature}")?;
                writeln!(f, "  Content: {}", thinking.replace('\n', "\n  "))
            }

            ContentBlock::RedactedThinking { data, .. } => {
                writeln!(f, "RedactedThinking")?;
                writeln!(f, "  Data: {}", data.replace('\n', "\n  "))
            }
            ContentBlock::ContextCompaction {
                compaction_number,
                messages_archived,
                context_size_before,
                summary,
                ..
            } => {
                writeln!(
                    f,
                    "ContextCompaction #{}: archived={}, size={}",
                    compaction_number, messages_archived, context_size_before
                )?;
                writeln!(f, "  Summary: {}", summary.replace('\n', "\n  "))
            }
        }
    }
}

impl fmt::Display for MessageContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageContent::Text(content) => {
                writeln!(f, "Text: {}", content.replace('\n', "\n  "))
            }
            MessageContent::Structured(blocks) => {
                writeln!(f, "Structured content with {} blocks:", blocks.len())?;
                for (k, block) in blocks.iter().enumerate() {
                    write!(f, "  Block {k}: ")?;
                    // Convert the block display output to a string so we can add indentation
                    let block_output = format!("{block}");
                    // Already includes a newline, so we don't need to add one here
                    write!(f, "{}", block_output.replace('\n', "\n  "))?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Role: {:?}", self.role)?;
        write!(f, "{}", self.content)
    }
}
