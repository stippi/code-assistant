//! Pure flattening of a [`ChatSession`] conversation tree into typed content
//! items.
//!
//! Both [`search`](super::search) and [`projection`](super::projection) build
//! on this single extraction pass, so "what counts as a user message / an
//! assistant reply / a tool call" is defined in exactly one place.

use llm::{ContentBlock, MessageContent, MessageRole};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::persistence::{ChatSession, NodeId};

/// The author of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// The kind of a single piece of content extracted from the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// Text authored by the user.
    UserText,
    /// Plain text authored by the assistant (a reply / end-of-turn summary,
    /// never a tool call or reasoning).
    AssistantText,
    /// Assistant reasoning ("thinking") content.
    Thinking,
    /// A tool invocation the assistant made.
    ToolCall,
    /// The result returned for a tool invocation.
    ToolResult,
}

/// One flattened piece of conversation content.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedItem {
    /// Position of the containing message along the walked path (0-based).
    pub message_index: usize,
    /// The conversation-tree node the item came from.
    pub node_id: NodeId,
    pub role: Role,
    pub kind: ContentKind,
    /// Human-readable text: the message/thinking text, or the tool result's
    /// textual content. Empty for [`ContentKind::ToolCall`].
    pub text: String,
    /// Tool name — set for [`ContentKind::ToolCall`] and (correlated by id)
    /// for [`ContentKind::ToolResult`].
    pub tool_name: Option<String>,
    /// Raw tool input — set for [`ContentKind::ToolCall`].
    pub tool_input: Option<serde_json::Value>,
    /// Correlation id linking a tool call to its result.
    pub tool_use_id: Option<String>,
    /// Whether a tool result was an error — set for [`ContentKind::ToolResult`].
    pub is_error: Option<bool>,
}

/// Flatten a session into ordered content items.
///
/// When `all_branches` is false the walk follows the session's active path
/// (the canonical linear conversation). When true it visits every node in the
/// tree in id order, so tool calls made in an abandoned branch are still
/// discovered — useful for "did this session ever touch file X".
///
/// Empty text/thinking items are dropped; tool calls and results are always
/// kept. Tool results have their `tool_name` backfilled from the matching
/// tool call when the id is present in the walk.
pub fn extract_items(session: &ChatSession, all_branches: bool) -> Vec<ExtractedItem> {
    let node_ids: Vec<NodeId> = if all_branches {
        // BTreeMap iterates keys in ascending order → creation order.
        session.message_nodes.keys().copied().collect()
    } else {
        session.active_path.clone()
    };

    let mut items = Vec::new();
    for (message_index, node_id) in node_ids.into_iter().enumerate() {
        let Some(node) = session.message_nodes.get(&node_id) else {
            continue;
        };
        let role = match node.message.role {
            MessageRole::User => Role::User,
            MessageRole::Assistant => Role::Assistant,
        };
        push_message_items(
            &mut items,
            message_index,
            node_id,
            role,
            &node.message.content,
        );
    }

    backfill_tool_result_names(&mut items);
    items
}

fn push_message_items(
    items: &mut Vec<ExtractedItem>,
    message_index: usize,
    node_id: NodeId,
    role: Role,
    content: &MessageContent,
) {
    let text_kind = match role {
        Role::User => ContentKind::UserText,
        Role::Assistant => ContentKind::AssistantText,
    };

    match content {
        MessageContent::Text(text) => {
            push_text(items, message_index, node_id, role, text_kind, text);
        }
        MessageContent::Structured(blocks) => {
            for block in blocks {
                match block {
                    ContentBlock::Text { text, .. } => {
                        push_text(items, message_index, node_id, role, text_kind, text);
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        push_text(
                            items,
                            message_index,
                            node_id,
                            role,
                            ContentKind::Thinking,
                            thinking,
                        );
                    }
                    ContentBlock::RedactedThinking { .. } => {
                        // No readable text; represented only by its presence.
                    }
                    ContentBlock::Image { .. } => {
                        // No textual content to extract.
                    }
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        items.push(ExtractedItem {
                            message_index,
                            node_id,
                            role,
                            kind: ContentKind::ToolCall,
                            text: String::new(),
                            tool_name: Some(name.clone()),
                            tool_input: Some(input.clone()),
                            tool_use_id: Some(id.clone()),
                            is_error: None,
                        });
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    } => {
                        items.push(ExtractedItem {
                            message_index,
                            node_id,
                            role,
                            kind: ContentKind::ToolResult,
                            text: content.text_content().to_string(),
                            tool_name: None,
                            tool_input: None,
                            tool_use_id: Some(tool_use_id.clone()),
                            is_error: *is_error,
                        });
                    }
                }
            }
        }
    }
}

fn push_text(
    items: &mut Vec<ExtractedItem>,
    message_index: usize,
    node_id: NodeId,
    role: Role,
    kind: ContentKind,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    items.push(ExtractedItem {
        message_index,
        node_id,
        role,
        kind,
        text: text.to_string(),
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        is_error: None,
    });
}

/// Fill in `tool_name` on tool-result items from the matching tool call.
fn backfill_tool_result_names(items: &mut [ExtractedItem]) {
    let names: HashMap<String, String> = items
        .iter()
        .filter(|item| item.kind == ContentKind::ToolCall)
        .filter_map(|item| {
            let id = item.tool_use_id.clone()?;
            let name = item.tool_name.clone()?;
            Some((id, name))
        })
        .collect();

    for item in items.iter_mut() {
        if item.kind == ContentKind::ToolResult
            && item.tool_name.is_none()
            && let Some(id) = &item.tool_use_id
            && let Some(name) = names.get(id)
        {
            item.tool_name = Some(name.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use llm::{ContentBlock, Message};

    fn empty_session() -> ChatSession {
        ChatSession::new_empty(
            "s1".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        )
    }

    #[test]
    fn flattens_active_path_in_order() {
        let mut session = empty_session();
        session.add_message(Message::new_user("What is the plan?"));
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_thinking("let me think", "sig"),
            ContentBlock::new_text("Here is the plan."),
            ContentBlock::new_tool_use(
                "tool-1",
                "write_file",
                serde_json::json!({ "path": "docs/plan.md" }),
            ),
        ]));
        session.add_message(Message::new_user_content(vec![
            ContentBlock::new_tool_result("tool-1", "ok"),
        ]));

        let items = extract_items(&session, false);
        let kinds: Vec<ContentKind> = items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ContentKind::UserText,
                ContentKind::Thinking,
                ContentKind::AssistantText,
                ContentKind::ToolCall,
                ContentKind::ToolResult,
            ]
        );

        // Message indices reflect node positions along the path.
        assert_eq!(items[0].message_index, 0);
        assert_eq!(items[3].message_index, 1);
        assert_eq!(items[4].message_index, 2);

        // Tool call carries name + input.
        assert_eq!(items[3].tool_name.as_deref(), Some("write_file"));
        assert_eq!(
            items[3].tool_input.as_ref().unwrap()["path"],
            serde_json::json!("docs/plan.md")
        );
    }

    #[test]
    fn backfills_tool_name_on_result() {
        let mut session = empty_session();
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_tool_use("t-9", "edit", serde_json::json!({ "path": "a.rs" })),
        ]));
        session.add_message(Message::new_user_content(vec![
            ContentBlock::new_tool_result("t-9", "done"),
        ]));

        let items = extract_items(&session, false);
        let result = items
            .iter()
            .find(|i| i.kind == ContentKind::ToolResult)
            .unwrap();
        assert_eq!(result.tool_name.as_deref(), Some("edit"));
        assert_eq!(result.text, "done");
    }

    #[test]
    fn all_branches_includes_off_path_nodes() {
        let mut session = empty_session();
        session.add_message(Message::new_user("root")); // node 1
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_tool_use("t-a", "write_file", serde_json::json!({ "path": "on.md" })),
        ])); // node 2 (on active path)
        // Branch off node 1 with a different tool call.
        session.add_message_with_parent(
            Message::new_assistant_content(vec![ContentBlock::new_tool_use(
                "t-b",
                "write_file",
                serde_json::json!({ "path": "off.md" }),
            )]),
            Some(1),
        ); // node 3 (now active path is 1 -> 3)

        // Switch active path back to node 2's branch.
        session.switch_branch(2).unwrap();

        let active_paths: Vec<String> = extract_items(&session, false)
            .into_iter()
            .filter(|i| i.kind == ContentKind::ToolCall)
            .map(|i| i.tool_input.unwrap()["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(active_paths, vec!["on.md".to_string()]);

        let all_paths: Vec<String> = extract_items(&session, true)
            .into_iter()
            .filter(|i| i.kind == ContentKind::ToolCall)
            .map(|i| i.tool_input.unwrap()["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(all_paths, vec!["on.md".to_string(), "off.md".to_string()]);
    }

    #[test]
    fn skips_empty_text() {
        let mut session = empty_session();
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_text("   "),
            ContentBlock::new_text("real"),
        ]));
        let items = extract_items(&session, false);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "real");
    }
}
