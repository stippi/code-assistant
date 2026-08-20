//! Filtered projection of a single session's content.
//!
//! A [`ContentProjection`] composes three orthogonal, intuitive knobs so a
//! caller (or an LLM tool) can read exactly what it needs:
//!
//! * `parts` — which kinds of content to include (user text, assistant
//!   replies, thinking, tool calls, tool results). Any combination; empty
//!   means "the conversation narrative" (user + assistant text).
//! * `tool_names` — narrow tool calls/results to specific tools.
//! * `range` + `max_chars_per_item` — bound the volume (a window of messages
//!   and a per-item character cap).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::SessionSource;
use super::extract::{ContentKind, Role, extract_items};
use crate::persistence::NodeId;

/// A selectable kind of content. Maps directly to the LLM-facing JSON strings
/// `user_text`, `assistant_text`, `thinking`, `tool_calls`, `tool_results`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPart {
    /// Text the user wrote.
    UserText,
    /// The assistant's plain-text replies / end-of-turn summaries.
    AssistantText,
    /// The assistant's reasoning.
    Thinking,
    /// Tool invocations (name + input).
    ToolCalls,
    /// Tool results.
    ToolResults,
}

/// A half-open window of messages by position along the walked path:
/// `[start, end)`. Either bound may be omitted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageRange {
    /// First message index to include (inclusive). Defaults to the start.
    #[serde(default)]
    pub start: Option<usize>,
    /// One past the last message index to include (exclusive). Defaults to
    /// the end.
    #[serde(default)]
    pub end: Option<usize>,
}

/// How to project a session's content. See the module docs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentProjection {
    /// Which content kinds to include. Empty means user + assistant text
    /// (the conversation narrative).
    #[serde(default)]
    pub parts: Vec<ContentPart>,
    /// Restrict tool calls/results to these tool names. `None` (or absent)
    /// means all tools; ignored for non-tool parts.
    #[serde(default)]
    pub tool_names: Option<Vec<String>>,
    /// Restrict to a window of messages.
    #[serde(default)]
    pub range: Option<MessageRange>,
    /// Truncate each item's text to this many characters.
    #[serde(default)]
    pub max_chars_per_item: Option<usize>,
    /// Project across all branches instead of just the active path.
    #[serde(default)]
    pub include_all_branches: bool,
}

/// The projected content of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionContent {
    pub session_id: String,
    pub name: String,
    pub project: String,
    /// Total number of messages along the walked path (before filtering) —
    /// context for interpreting `range` and `message_index`.
    pub message_count: usize,
    pub items: Vec<ContentItem>,
}

/// One projected item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContentItem {
    pub message_index: usize,
    pub node_id: NodeId,
    pub role: Role,
    pub kind: ContentKind,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Whether `text` was cut to `max_chars_per_item`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Project a single session. Errors if the session does not exist.
pub fn get_session_content(
    source: &dyn SessionSource,
    session_id: &str,
    projection: &ContentProjection,
) -> Result<SessionContent> {
    let session = source
        .load(session_id)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;

    let message_count = if projection.include_all_branches {
        session.message_nodes.len()
    } else {
        session.active_path.len()
    };

    let (range_start, range_end) = match &projection.range {
        Some(range) => (range.start.unwrap_or(0), range.end.unwrap_or(usize::MAX)),
        None => (0, usize::MAX),
    };

    let items = extract_items(&session, projection.include_all_branches)
        .into_iter()
        .filter(|item| kind_selected(item.kind, &projection.parts))
        .filter(|item| item.message_index >= range_start && item.message_index < range_end)
        .filter(|item| tool_name_allowed(item, projection.tool_names.as_deref()))
        .map(|item| {
            let (text, truncated) = match projection.max_chars_per_item {
                Some(max) => truncate(&item.text, max),
                None => (item.text, false),
            };
            ContentItem {
                message_index: item.message_index,
                node_id: item.node_id,
                role: item.role,
                kind: item.kind,
                text,
                tool_name: item.tool_name,
                tool_input: item.tool_input,
                is_error: item.is_error,
                truncated,
            }
        })
        .collect();

    Ok(SessionContent {
        session_id: session.id.clone(),
        name: session.name.clone(),
        project: session.initial_project().to_string(),
        message_count,
        items,
    })
}

/// A tool-name filter only constrains tool calls/results; other kinds pass.
fn tool_name_allowed(item: &super::extract::ExtractedItem, tool_names: Option<&[String]>) -> bool {
    if !matches!(item.kind, ContentKind::ToolCall | ContentKind::ToolResult) {
        return true;
    }
    let Some(names) = tool_names else {
        return true;
    };
    match &item.tool_name {
        Some(name) => names.iter().any(|n| n == name),
        None => false,
    }
}

fn kind_selected(kind: ContentKind, parts: &[ContentPart]) -> bool {
    let part = match kind {
        ContentKind::UserText => ContentPart::UserText,
        ContentKind::AssistantText => ContentPart::AssistantText,
        ContentKind::Thinking => ContentPart::Thinking,
        ContentKind::ToolCall => ContentPart::ToolCalls,
        ContentKind::ToolResult => ContentPart::ToolResults,
    };
    if parts.is_empty() {
        // Default narrative: user + assistant text only.
        matches!(part, ContentPart::UserText | ContentPart::AssistantText)
    } else {
        parts.contains(&part)
    }
}

fn truncate(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    let cut: String = text.chars().take(max).collect();
    (cut, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::ChatSession;
    use crate::session::SessionConfig;
    use crate::session_query::test_source::InMemorySource;
    use llm::{ContentBlock, Message};

    fn sample_session() -> ChatSession {
        let mut session = ChatSession::new_empty(
            "s1".to_string(),
            "Sample".to_string(),
            SessionConfig {
                initial_project: "mlflow".to_string(),
                ..SessionConfig::default()
            },
            None,
        );
        session.add_message(Message::new_user("Investigate token refresh")); // msg 0
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_thinking("reasoning here", "sig"),
            ContentBlock::new_text("I will edit the doc."),
            ContentBlock::new_tool_use("t1", "edit", serde_json::json!({ "path": "docs/a.md" })),
        ])); // msg 1
        session.add_message(Message::new_user_content(vec![
            ContentBlock::new_tool_result("t1", "edit applied"),
        ])); // msg 2
        session.add_message(Message::new_assistant("Done, the doc is updated.")); // msg 3
        session
    }

    fn source() -> InMemorySource {
        InMemorySource::new().with_session(sample_session())
    }

    #[test]
    fn default_parts_return_only_narrative_text() {
        let content = get_session_content(&source(), "s1", &ContentProjection::default()).unwrap();
        let kinds: Vec<ContentKind> = content.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ContentKind::UserText,
                ContentKind::AssistantText,
                ContentKind::AssistantText,
            ]
        );
        assert_eq!(content.message_count, 4);
        assert_eq!(content.project, "mlflow");
    }

    #[test]
    fn select_tool_calls_and_filter_by_name() {
        let projection = ContentProjection {
            parts: vec![ContentPart::ToolCalls, ContentPart::ToolResults],
            tool_names: Some(vec!["edit".to_string()]),
            ..Default::default()
        };
        let content = get_session_content(&source(), "s1", &projection).unwrap();
        let kinds: Vec<ContentKind> = content.items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, vec![ContentKind::ToolCall, ContentKind::ToolResult]);
        assert_eq!(content.items[0].tool_name.as_deref(), Some("edit"));
        assert_eq!(content.items[1].tool_name.as_deref(), Some("edit"));
    }

    #[test]
    fn tool_name_filter_excludes_other_tools() {
        let projection = ContentProjection {
            parts: vec![ContentPart::ToolCalls],
            tool_names: Some(vec!["write_file".to_string()]),
            ..Default::default()
        };
        let content = get_session_content(&source(), "s1", &projection).unwrap();
        assert!(content.items.is_empty());
    }

    #[test]
    fn range_selects_message_window() {
        // Include all parts, restrict to message index 1 only.
        let projection = ContentProjection {
            parts: vec![
                ContentPart::UserText,
                ContentPart::AssistantText,
                ContentPart::Thinking,
                ContentPart::ToolCalls,
                ContentPart::ToolResults,
            ],
            range: Some(MessageRange {
                start: Some(1),
                end: Some(2),
            }),
            ..Default::default()
        };
        let content = get_session_content(&source(), "s1", &projection).unwrap();
        assert!(content.items.iter().all(|i| i.message_index == 1));
        // Message 1 has thinking, assistant text, and a tool call.
        let kinds: Vec<ContentKind> = content.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ContentKind::Thinking,
                ContentKind::AssistantText,
                ContentKind::ToolCall,
            ]
        );
    }

    #[test]
    fn truncates_long_text() {
        let projection = ContentProjection {
            parts: vec![ContentPart::UserText],
            max_chars_per_item: Some(4),
            ..Default::default()
        };
        let content = get_session_content(&source(), "s1", &projection).unwrap();
        assert_eq!(content.items.len(), 1);
        assert_eq!(content.items[0].text, "Inve");
        assert!(content.items[0].truncated);
    }

    #[test]
    fn missing_session_is_an_error() {
        let err =
            get_session_content(&source(), "nope", &ContentProjection::default()).unwrap_err();
        assert!(format!("{err}").contains("nope"));
    }

    #[test]
    fn projection_json_is_intuitive() {
        let parsed: ContentProjection = serde_json::from_value(serde_json::json!({
            "parts": ["user_text", "assistant_text", "tool_calls"],
            "tool_names": ["write_file", "edit"],
            "range": { "start": 0, "end": 10 },
            "max_chars_per_item": 500
        }))
        .unwrap();
        assert_eq!(parsed.parts.len(), 3);
        assert_eq!(parsed.tool_names.as_ref().unwrap().len(), 2);
        assert_eq!(parsed.range.as_ref().unwrap().end, Some(10));
        assert_eq!(parsed.max_chars_per_item, Some(500));
    }
}
