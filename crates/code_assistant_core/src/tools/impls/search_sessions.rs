//! `search_sessions` — find sessions across the persisted store by cheap
//! metadata filters and/or by the tool calls they contain.

use crate::persistence::FileSessionPersistence;
use crate::session_query::{
    SessionMatch, SessionSearchQuery, SessionSource, ToolCallFilter, search_sessions,
};
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::SystemTime;

/// Input for the `search_sessions` tool. Mirrors [`SessionSearchQuery`] with
/// timestamps as RFC 3339 strings.
#[derive(Deserialize, Serialize, Default)]
pub struct SearchSessionsInput {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub name_contains: Option<String>,
    #[serde(default)]
    pub updated_after: Option<String>,
    #[serde(default)]
    pub updated_before: Option<String>,
    #[serde(default)]
    pub tool_call: Option<ToolCallFilter>,
    #[serde(default)]
    pub text_contains: Option<String>,
    #[serde(default)]
    pub include_all_branches: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCallMatchData {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_value: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionMatchData {
    pub session_id: String,
    pub name: String,
    pub project: String,
    pub updated_at: String,
    pub message_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_tool_calls: Vec<ToolCallMatchData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchSessionsOutput {
    pub total: usize,
    pub sessions: Vec<SessionMatchData>,
}

impl Render for SearchSessionsOutput {
    fn status(&self) -> String {
        format!("Found {} matching session(s)", self.total)
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if self.sessions.is_empty() {
            return "No sessions matched the query.".to_string();
        }
        let mut out = format!("{} matching session(s):\n", self.total);
        for s in &self.sessions {
            let name = if s.name.is_empty() {
                "(unnamed)"
            } else {
                s.name.as_str()
            };
            out.push_str(&format!(
                "\n- {} [{}] \"{}\" — {} message(s), updated {}\n",
                s.session_id, s.project, name, s.message_count, s.updated_at
            ));
            // Show unique matched values (e.g. distinct file paths).
            let mut seen = std::collections::BTreeSet::new();
            for tc in &s.matched_tool_calls {
                let line = match &tc.matched_value {
                    Some(v) => format!("    {} → {}", tc.tool_name, v),
                    None => format!("    {}", tc.tool_name),
                };
                if seen.insert(line.clone()) {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
        out
    }
}

impl ToolResult for SearchSessionsOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct SearchSessionsTool;

#[async_trait::async_trait]
impl Tool for SearchSessionsTool {
    type Input = SearchSessionsInput;
    type Output = SearchSessionsOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Search across code-assistant's stored sessions (past conversations). Every set ",
            "filter must match (logical AND); unset filters are ignored. `project`, ",
            "`name_contains` and the time filters are answered from a cheap index without ",
            "reading conversations; `tool_call` and `text_contains` scan the matching ",
            "conversations. Returns compact matches (newest first) — use `get_session_content` ",
            "afterwards to read a specific session. Typical use: find which sessions created or ",
            "edited a file by matching the `write_file`/`edit` tool calls on its `path`."
        );
        ToolSpec {
            name: "search_sessions".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Only sessions whose project name equals this exactly (e.g. \"mlflow\")."
                    },
                    "name_contains": {
                        "type": "string",
                        "description": "Case-insensitive substring of the session name."
                    },
                    "updated_after": {
                        "type": "string",
                        "description": "Only sessions updated at or after this RFC 3339 timestamp (e.g. \"2025-01-01T00:00:00Z\")."
                    },
                    "updated_before": {
                        "type": "string",
                        "description": "Only sessions updated at or before this RFC 3339 timestamp."
                    },
                    "tool_call": {
                        "type": "object",
                        "description": "Match sessions containing a tool call. Combine a tool-name list with an argument matcher.",
                        "properties": {
                            "names": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Tool names to match (e.g. [\"write_file\", \"edit\"]). Empty or omitted matches any tool."
                            },
                            "arg": {
                                "type": "string",
                                "description": "Restrict to calls carrying this input argument (e.g. \"path\"). Without `value`, any value matches as long as the argument is present."
                            },
                            "value": {
                                "type": "object",
                                "description": "How to match the `arg` value. Exactly one key. Requires `arg`.",
                                "properties": {
                                    "equals":   { "type": "string", "description": "Value equals this string." },
                                    "contains": { "type": "string", "description": "Value contains this substring." },
                                    "glob":     { "type": "string", "description": "Value matches this glob (e.g. \"**/docs/*.md\"; * spans path separators)." },
                                    "regex":    { "type": "string", "description": "Value matches this regular expression." }
                                }
                            }
                        }
                    },
                    "text_contains": {
                        "type": "string",
                        "description": "Case-insensitive substring appearing anywhere in message, thinking or tool-result text."
                    },
                    "include_all_branches": {
                        "type": "boolean",
                        "description": "Search every branch, not just the active conversation path. Default false. Use to discover tool calls made in abandoned branches."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of sessions to return (newest first)."
                    }
                }
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true
            })),
            capabilities: ToolSpec::capabilities(&[
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Searching sessions"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let source = session_source(context);

        let query = SessionSearchQuery {
            project: input.project.clone(),
            name_contains: input.name_contains.clone(),
            updated_after: parse_time(input.updated_after.as_deref(), "updated_after")?,
            updated_before: parse_time(input.updated_before.as_deref(), "updated_before")?,
            tool_call: input.tool_call.clone(),
            text_contains: input.text_contains.clone(),
            include_all_branches: input.include_all_branches,
            limit: input.limit,
        };

        let matches = search_sessions(source.as_ref(), &query)?;
        Ok(SearchSessionsOutput {
            total: matches.len(),
            sessions: matches.into_iter().map(to_data).collect(),
        })
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

fn parse_time(value: Option<&str>, field: &str) -> Result<Option<SystemTime>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("`{field}` must be an RFC 3339 timestamp, got {value:?}"))?;
    Ok(Some(parsed.into()))
}

fn to_data(m: SessionMatch) -> SessionMatchData {
    let updated_at: DateTime<Utc> = m.updated_at.into();
    SessionMatchData {
        session_id: m.session_id,
        name: m.name,
        project: m.project,
        updated_at: updated_at.to_rfc3339(),
        message_count: m.message_count,
        matched_tool_calls: m
            .matched_tool_calls
            .into_iter()
            .map(|tc| ToolCallMatchData {
                tool_name: tc.tool_name,
                matched_value: tc.matched_value,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::persistence::ChatSession;
    use crate::session::SessionConfig;
    use crate::session_query::test_source::InMemorySource;
    use crate::tools::core::ResourcesTracker;
    use llm::{ContentBlock, Message};

    fn wrote(id: &str, project: &str, tool: &str, path: &str) -> ChatSession {
        let mut session = ChatSession::new_empty(
            id.to_string(),
            format!("Session {id}"),
            SessionConfig {
                initial_project: project.to_string(),
                ..SessionConfig::default()
            },
            None,
        );
        session.add_message(Message::new_user("please update the docs"));
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_tool_use(
                format!("{id}-call"),
                tool,
                serde_json::json!({ "path": path }),
            ),
        ]));
        session
    }

    async fn run(source: InMemorySource, params: serde_json::Value) -> SearchSessionsOutput {
        let mut fixture = ToolTestFixture::new().with_session_source(Arc::new(source));
        let mut context = fixture.context();
        let mut input: SearchSessionsInput = serde_json::from_value(params).unwrap();
        SearchSessionsTool
            .execute(&mut context, &mut input)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn finds_sessions_that_wrote_a_docs_file() {
        let source = InMemorySource::new()
            .with_session(wrote(
                "a",
                "mlflow",
                "write_file",
                "selfhosting-poc/docs/plan.md",
            ))
            .with_session(wrote("b", "mlflow", "edit", "src/main.rs"))
            .with_session(wrote(
                "c",
                "other",
                "write_file",
                "selfhosting-poc/docs/x.md",
            ));

        let output = run(
            source,
            json!({
                "project": "mlflow",
                "tool_call": {
                    "names": ["write_file", "edit"],
                    "arg": "path",
                    "value": { "contains": "docs/" }
                }
            }),
        )
        .await;

        assert_eq!(output.total, 1);
        assert_eq!(output.sessions[0].session_id, "a");
        assert_eq!(
            output.sessions[0].matched_tool_calls[0]
                .matched_value
                .as_deref(),
            Some("selfhosting-poc/docs/plan.md")
        );

        let rendered = output.render(&mut ResourcesTracker::new());
        assert!(rendered.contains("selfhosting-poc/docs/plan.md"));
    }

    #[tokio::test]
    async fn invalid_timestamp_is_reported() {
        let mut fixture =
            ToolTestFixture::new().with_session_source(Arc::new(InMemorySource::new()));
        let mut context = fixture.context();
        let mut input = SearchSessionsInput {
            updated_after: Some("not-a-date".to_string()),
            ..Default::default()
        };
        let err = SearchSessionsTool
            .execute(&mut context, &mut input)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("updated_after"));
    }

    #[tokio::test]
    async fn empty_result_renders_gracefully() {
        let output = run(InMemorySource::new(), json!({ "project": "nope" })).await;
        assert_eq!(output.total, 0);
        let rendered = output.render(&mut ResourcesTracker::new());
        assert!(rendered.contains("No sessions matched"));
    }
}
