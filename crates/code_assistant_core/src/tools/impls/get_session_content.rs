//! `get_session_content` — read a filtered projection of a single stored
//! session.

use crate::persistence::FileSessionPersistence;
use crate::session_query::{
    ContentItem, ContentKind, ContentPart, ContentProjection, MessageRange, SessionSource,
    get_session_content,
};
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

// Output-size guards, so an unbounded projection cannot flood the context:
//   * item cap    — at most MAX_ITEMS items are returned (older ones kept).
//   * char budget — when the caller set no `max_chars_per_item` and the total
//     text exceeds CHAR_BUDGET, each item is auto-truncated to a computed cap.
const MAX_ITEMS: usize = 200;
const CHAR_BUDGET: usize = 16_000;
const MIN_AUTO_CAP: usize = 200;

/// Input for the `get_session_content` tool. Mirrors [`ContentProjection`]
/// plus the target session id.
#[derive(Deserialize, Serialize, Default)]
pub struct GetSessionContentInput {
    pub session_id: String,
    #[serde(default)]
    pub parts: Vec<ContentPart>,
    #[serde(default)]
    pub tool_names: Option<Vec<String>>,
    #[serde(default)]
    pub range: Option<MessageRange>,
    #[serde(default)]
    pub max_chars_per_item: Option<usize>,
    #[serde(default)]
    pub include_all_branches: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetSessionContentOutput {
    pub session_id: String,
    pub name: String,
    pub project: String,
    /// Total messages along the walked path (before filtering).
    pub message_count: usize,
    /// Items selected by the projection (before the item cap).
    pub total_items: usize,
    /// Items actually returned (after the item cap).
    pub returned_items: usize,
    /// Whether items were dropped by the item cap.
    #[serde(default)]
    pub truncated: bool,
    /// Whether item text was auto-truncated to fit the character budget
    /// (only happens when the caller set no `max_chars_per_item`).
    #[serde(default)]
    pub auto_truncated: bool,
    pub items: Vec<ContentItem>,
}

impl Render for GetSessionContentOutput {
    fn status(&self) -> String {
        format!(
            "{} item(s) from session {}",
            self.returned_items, self.session_id
        )
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let name = if self.name.is_empty() {
            "(unnamed)"
        } else {
            self.name.as_str()
        };
        let mut out = format!(
            "Session {} [{}] \"{}\" — {} of {} message(s)\n",
            self.session_id, self.project, name, self.returned_items, self.message_count
        );
        for item in &self.items {
            out.push('\n');
            out.push_str(&render_item(item));
        }
        if self.truncated {
            out.push_str(&format!(
                "\n… {} more item(s) not shown. Narrow with `range`, `parts`, `tool_names`, \
                 or set `max_chars_per_item`.\n",
                self.total_items - self.returned_items
            ));
        } else if self.auto_truncated {
            out.push_str(
                "\n(item text auto-truncated to fit; set `max_chars_per_item` or a `range` \
                 for finer control.)\n",
            );
        }
        out
    }

    fn render_for_ui(&self, _tracker: &mut ResourcesTracker) -> String {
        let items: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|item| {
                json!({
                    "message_index": item.message_index,
                    "role": item.role,
                    "kind": item.kind,
                    "text": item.text,
                    "tool_name": item.tool_name,
                    "tool_input": item.tool_input,
                    "is_error": item.is_error,
                    "truncated": item.truncated,
                })
            })
            .collect();

        json!({
            "kind": "get_session_content",
            "session_id": self.session_id,
            "name": self.name,
            "project": self.project,
            "message_count": self.message_count,
            "total_items": self.total_items,
            "returned_items": self.returned_items,
            "truncated": self.truncated,
            "auto_truncated": self.auto_truncated,
            "items": items,
        })
        .to_string()
    }
}

fn render_item(item: &ContentItem) -> String {
    let idx = item.message_index;
    let ellipsis = if item.truncated { "…" } else { "" };
    match item.kind {
        ContentKind::UserText => format!("[#{idx}] User: {}{ellipsis}\n", item.text),
        ContentKind::AssistantText => {
            format!("[#{idx}] Assistant: {}{ellipsis}\n", item.text)
        }
        ContentKind::Thinking => format!("[#{idx}] Thinking: {}{ellipsis}\n", item.text),
        ContentKind::ToolCall => {
            let name = item.tool_name.as_deref().unwrap_or("tool");
            let args = item
                .tool_input
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default();
            format!("[#{idx}] → {name} {args}\n")
        }
        ContentKind::ToolResult => {
            let name = item.tool_name.as_deref().unwrap_or("tool");
            let status = match item.is_error {
                Some(true) => "error",
                _ => "ok",
            };
            format!(
                "[#{idx}]   {name} result ({status}): {}{ellipsis}\n",
                item.text
            )
        }
    }
}

impl ToolResult for GetSessionContentOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct GetSessionContentTool;

#[async_trait::async_trait]
impl Tool for GetSessionContentTool {
    type Input = GetSessionContentInput;
    type Output = GetSessionContentOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Read a filtered slice of one stored session (found via `search_sessions`). Choose ",
            "which kinds of content to include with `parts`; narrow tool calls/results to ",
            "specific tools with `tool_names`; bound the volume with `range` (a window of ",
            "messages) and `max_chars_per_item`. Content is returned in conversation order. To ",
            "recover a session's original request, project `user_text` (and optionally the final ",
            "`assistant_text`)."
        );
        ToolSpec {
            name: "get_session_content".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id of the session to read (from search_sessions)."
                    },
                    "parts": {
                        "type": "array",
                        "description": "Which content kinds to include, in any combination. Empty or omitted returns the conversation narrative (user_text + assistant_text).",
                        "items": {
                            "type": "string",
                            "enum": ["user_text", "assistant_text", "thinking", "tool_calls", "tool_results"]
                        }
                    },
                    "tool_names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "When tool_calls/tool_results are included, restrict them to these tool names. Omit for all tools; ignored for non-tool parts."
                    },
                    "range": {
                        "type": "object",
                        "description": "Restrict to a window of messages by position (0-based) along the conversation. Half-open [start, end).",
                        "properties": {
                            "start": { "type": "integer", "description": "First message index to include (inclusive). Default: the beginning." },
                            "end":   { "type": "integer", "description": "One past the last message index (exclusive). Default: the end." }
                        }
                    },
                    "max_chars_per_item": {
                        "type": "integer",
                        "description": "Truncate each item's text to at most this many characters (items are marked as truncated)."
                    },
                    "include_all_branches": {
                        "type": "boolean",
                        "description": "Project across all branches instead of just the active conversation path. Default false."
                    }
                },
                "required": ["session_id"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true
            })),
            capabilities: ToolSpec::capabilities(&[
                capabilities::READ_ONLY,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Reading session {session_id}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let source = session_source(context);

        let projection = ContentProjection {
            parts: input.parts.clone(),
            tool_names: input.tool_names.clone(),
            range: input.range.clone(),
            max_chars_per_item: input.max_chars_per_item,
            include_all_branches: input.include_all_branches,
        };

        let content = get_session_content(source.as_ref(), &input.session_id, &projection)?;
        Ok(build_output(content, input.max_chars_per_item.is_some()))
    }
}

/// Apply the output-size guards to a projected session.
fn build_output(
    content: crate::session_query::SessionContent,
    caller_set_char_cap: bool,
) -> GetSessionContentOutput {
    let total_items = content.items.len();
    let mut items = content.items;

    let truncated = total_items > MAX_ITEMS;
    if truncated {
        items.truncate(MAX_ITEMS);
    }

    // Character-budget auto-truncation, only when the caller left the per-item
    // cap unset (otherwise the caller's choice is authoritative).
    let mut auto_truncated = false;
    if !caller_set_char_cap && !items.is_empty() {
        let total_chars: usize = items.iter().map(|i| i.text.chars().count()).sum();
        if total_chars > CHAR_BUDGET {
            let cap = (CHAR_BUDGET / items.len()).max(MIN_AUTO_CAP);
            for item in &mut items {
                if item.text.chars().count() > cap {
                    item.text = item.text.chars().take(cap).collect();
                    item.truncated = true;
                    auto_truncated = true;
                }
            }
        }
    }

    GetSessionContentOutput {
        session_id: content.session_id,
        name: content.name,
        project: content.project,
        message_count: content.message_count,
        total_items,
        returned_items: items.len(),
        truncated,
        auto_truncated,
        items,
    }
}

/// The injected session store, or the default on-disk store.
fn session_source(context: &ToolContext<'_>) -> Arc<dyn SessionSource> {
    use crate::tools::ToolServicesAccess;
    context
        .services()
        .session_source
        .clone()
        .unwrap_or_else(|| Arc::new(FileSessionPersistence::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::persistence::ChatSession;
    use crate::session::SessionConfig;
    use crate::session_query::test_source::InMemorySource;
    use llm::{ContentBlock, Message};

    fn sample() -> ChatSession {
        let mut session = ChatSession::new_empty(
            "s1".to_string(),
            "Investigate token refresh".to_string(),
            SessionConfig {
                initial_project: "mlflow".to_string(),
                ..SessionConfig::default()
            },
            None,
        );
        session.add_message(Message::new_user("Investigate the OAuth token refresh"));
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_thinking("reasoning", "sig"),
            ContentBlock::new_text("I will edit the doc."),
            ContentBlock::new_tool_use("t1", "edit", serde_json::json!({ "path": "docs/a.md" })),
        ]));
        session.add_message(Message::new_user_content(vec![
            ContentBlock::new_tool_result("t1", "edit applied"),
        ]));
        session.add_message(Message::new_assistant("Done, the doc is updated."));
        session
    }

    async fn run_on(
        session: ChatSession,
        params: serde_json::Value,
    ) -> Result<GetSessionContentOutput> {
        let source = InMemorySource::new().with_session(session);
        let mut fixture = ToolTestFixture::new().with_session_source(Arc::new(source));
        let mut context = fixture.context();
        let mut input: GetSessionContentInput = serde_json::from_value(params).unwrap();
        GetSessionContentTool
            .execute(&mut context, &mut input)
            .await
    }

    async fn run(params: serde_json::Value) -> Result<GetSessionContentOutput> {
        run_on(sample(), params).await
    }

    #[tokio::test]
    async fn default_projection_returns_narrative() {
        let output = run(json!({ "session_id": "s1" })).await.unwrap();
        let kinds: Vec<ContentKind> = output.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ContentKind::UserText,
                ContentKind::AssistantText,
                ContentKind::AssistantText,
            ]
        );
        assert_eq!(output.message_count, 4);
        assert!(!output.truncated);
        assert!(!output.auto_truncated);
        let rendered = output.render(&mut ResourcesTracker::new());
        assert!(rendered.contains("User: Investigate the OAuth token refresh"));
    }

    #[tokio::test]
    async fn projects_tool_calls_only() {
        let output = run(json!({
            "session_id": "s1",
            "parts": ["tool_calls"],
            "tool_names": ["edit"]
        }))
        .await
        .unwrap();
        assert_eq!(output.returned_items, 1);
        assert_eq!(output.items[0].kind, ContentKind::ToolCall);
        assert_eq!(output.items[0].tool_name.as_deref(), Some("edit"));
        let rendered = output.render(&mut ResourcesTracker::new());
        assert!(rendered.contains("→ edit"));
    }

    #[tokio::test]
    async fn missing_session_errors() {
        let err = run(json!({ "session_id": "does-not-exist" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("does-not-exist"));
    }

    #[tokio::test]
    async fn explicit_truncation_marks_items() {
        let output = run(json!({
            "session_id": "s1",
            "parts": ["user_text"],
            "max_chars_per_item": 5
        }))
        .await
        .unwrap();
        assert_eq!(output.items.len(), 1);
        assert!(output.items[0].truncated);
        assert_eq!(output.items[0].text.chars().count(), 5);
        // Explicit cap is not reported as auto-truncation.
        assert!(!output.auto_truncated);
    }

    #[tokio::test]
    async fn item_cap_truncates_huge_sessions() {
        let mut session = ChatSession::new_empty(
            "big".to_string(),
            "Big".to_string(),
            SessionConfig::default(),
            None,
        );
        for i in 0..(MAX_ITEMS + 50) {
            session.add_message(Message::new_user(format!("msg {i}")));
        }
        let output = run_on(
            session,
            json!({ "session_id": "big", "parts": ["user_text"] }),
        )
        .await
        .unwrap();
        assert_eq!(output.total_items, MAX_ITEMS + 50);
        assert_eq!(output.returned_items, MAX_ITEMS);
        assert!(output.truncated);
        let rendered = output.render(&mut ResourcesTracker::new());
        assert!(rendered.contains("more item(s) not shown"));
    }

    #[tokio::test]
    async fn char_budget_auto_truncates_when_no_cap_set() {
        let mut session = ChatSession::new_empty(
            "big".to_string(),
            "Big".to_string(),
            SessionConfig::default(),
            None,
        );
        // Ten long messages, well over the budget.
        for _ in 0..10 {
            session.add_message(Message::new_user("x".repeat(4_000)));
        }
        let output = run_on(
            session,
            json!({ "session_id": "big", "parts": ["user_text"] }),
        )
        .await
        .unwrap();
        assert!(output.auto_truncated);
        assert!(!output.truncated);
        // Each item capped to CHAR_BUDGET / items (>= MIN_AUTO_CAP).
        assert!(
            output
                .items
                .iter()
                .all(|i| i.text.chars().count() <= CHAR_BUDGET)
        );
        assert!(output.items.iter().any(|i| i.truncated));
    }
}
