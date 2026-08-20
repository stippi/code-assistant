//! Cross-store session search: cheap metadata pre-filter plus optional
//! content inspection (tool calls, free text).

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use super::SessionSource;
use super::extract::{ContentKind, extract_items};
use super::matcher::StringMatch;
use crate::persistence::NodeId;

/// What to search for. All set fields must match (logical AND). Unset fields
/// are ignored.
///
/// The `project` / `name_contains` / time fields are answered from the cheap
/// metadata index without loading any session. `tool_call` and
/// `text_contains` require loading each surviving candidate.
#[derive(Debug, Clone, Default)]
pub struct SessionSearchQuery {
    /// Exact `initial_project` name (e.g. `"mlflow"`).
    pub project: Option<String>,
    /// Case-insensitive substring of the session name.
    pub name_contains: Option<String>,
    /// Only sessions updated at or after this time.
    pub updated_after: Option<SystemTime>,
    /// Only sessions updated at or before this time.
    pub updated_before: Option<SystemTime>,
    /// Only sessions containing a matching tool call.
    pub tool_call: Option<ToolCallFilter>,
    /// Only sessions containing this case-insensitive substring anywhere in
    /// message/thinking/tool-result text.
    pub text_contains: Option<String>,
    /// Inspect all branches, not just the active path (see
    /// [`extract_items`](super::extract::extract_items)).
    pub include_all_branches: bool,
    /// Cap the number of results (newest first).
    pub limit: Option<usize>,
}

/// Match a session by the tool calls it contains.
///
/// Intuitive JSON, e.g.:
/// `{"names": ["write_file", "edit"], "arg": "path", "value": {"contains": "docs/"}}`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallFilter {
    /// Tool names to match; empty matches any tool.
    #[serde(default)]
    pub names: Vec<String>,
    /// Restrict to calls that carry this input argument (e.g. `"path"`).
    #[serde(default)]
    pub arg: Option<String>,
    /// Match the `arg` value with this matcher. Requires `arg`.
    #[serde(default)]
    pub value: Option<StringMatch>,
}

/// A matching session with the tool calls that caused the match (empty when
/// the match came only from metadata / text filters).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMatch {
    pub session_id: String,
    pub name: String,
    pub project: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub message_count: usize,
    pub matched_tool_calls: Vec<ToolCallMatch>,
}

/// A single tool call that matched a [`ToolCallFilter`].
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallMatch {
    pub node_id: NodeId,
    pub tool_name: String,
    pub input: serde_json::Value,
    /// The `arg` value that matched (when an `arg` was specified).
    pub matched_value: Option<String>,
}

/// Search the store. Results are newest first (as the metadata index orders
/// them), capped by `limit`.
pub fn search_sessions(
    source: &dyn SessionSource,
    query: &SessionSearchQuery,
) -> Result<Vec<SessionMatch>> {
    // Compile and validate the tool-call filter once, up front.
    let compiled_tool_filter = query
        .tool_call
        .as_ref()
        .map(CompiledToolFilter::compile)
        .transpose()?;

    let needs_content = compiled_tool_filter.is_some() || query.text_contains.is_some();
    let text_needle = query.text_contains.as_ref().map(|s| s.to_lowercase());

    let mut results = Vec::new();
    for meta in source.list_metadata()? {
        if !metadata_matches(&meta, query) {
            continue;
        }

        if !needs_content {
            results.push(SessionMatch {
                session_id: meta.id.clone(),
                name: meta.name.clone(),
                project: meta.initial_project.clone(),
                created_at: meta.created_at,
                updated_at: meta.updated_at,
                message_count: meta.message_count,
                matched_tool_calls: Vec::new(),
            });
        } else {
            let Some(session) = source.load(&meta.id)? else {
                continue;
            };
            let items = extract_items(&session, query.include_all_branches);

            // Tool-call filter (AND).
            let matched_tool_calls = match &compiled_tool_filter {
                Some(filter) => {
                    let matches = filter.matches(&items);
                    if matches.is_empty() {
                        continue;
                    }
                    matches
                }
                None => Vec::new(),
            };

            // Text filter (AND).
            if let Some(needle) = &text_needle
                && !items.iter().any(|i| i.text.to_lowercase().contains(needle))
            {
                continue;
            }

            results.push(SessionMatch {
                session_id: meta.id.clone(),
                name: meta.name.clone(),
                project: meta.initial_project.clone(),
                created_at: meta.created_at,
                updated_at: meta.updated_at,
                message_count: meta.message_count,
                matched_tool_calls,
            });
        }

        if let Some(limit) = query.limit
            && results.len() >= limit
        {
            break;
        }
    }

    Ok(results)
}

fn metadata_matches(meta: &crate::persistence::ChatMetadata, query: &SessionSearchQuery) -> bool {
    if let Some(project) = &query.project
        && &meta.initial_project != project
    {
        return false;
    }
    if let Some(name) = &query.name_contains
        && !meta.name.to_lowercase().contains(&name.to_lowercase())
    {
        return false;
    }
    if let Some(after) = query.updated_after
        && meta.updated_at < after
    {
        return false;
    }
    if let Some(before) = query.updated_before
        && meta.updated_at > before
    {
        return false;
    }
    true
}

/// A validated tool-call filter.
struct CompiledToolFilter {
    names: Vec<String>,
    arg: Option<String>,
    value: Option<super::matcher::CompiledMatch>,
}

impl CompiledToolFilter {
    fn compile(filter: &ToolCallFilter) -> Result<Self> {
        if filter.value.is_some() && filter.arg.is_none() {
            bail!("tool_call.value requires tool_call.arg to be set");
        }
        Ok(Self {
            names: filter.names.clone(),
            arg: filter.arg.clone(),
            value: filter
                .value
                .as_ref()
                .map(StringMatch::compile)
                .transpose()?,
        })
    }

    fn name_ok(&self, name: &str) -> bool {
        self.names.is_empty() || self.names.iter().any(|n| n == name)
    }

    fn matches(&self, items: &[super::extract::ExtractedItem]) -> Vec<ToolCallMatch> {
        let mut matches = Vec::new();
        for item in items {
            if item.kind != ContentKind::ToolCall {
                continue;
            }
            let Some(tool_name) = &item.tool_name else {
                continue;
            };
            if !self.name_ok(tool_name) {
                continue;
            }

            let matched_value = match &self.arg {
                None => None,
                Some(arg) => {
                    // The call must carry the argument at all.
                    let Some(raw) = item.tool_input.as_ref().and_then(|input| input.get(arg))
                    else {
                        continue;
                    };
                    let candidate = raw
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| raw.to_string());
                    if let Some(matcher) = &self.value
                        && !matcher.is_match(&candidate)
                    {
                        continue;
                    }
                    Some(candidate)
                }
            };

            matches.push(ToolCallMatch {
                node_id: item.node_id,
                tool_name: tool_name.clone(),
                input: item.tool_input.clone().unwrap_or(serde_json::Value::Null),
                matched_value,
            });
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::ChatSession;
    use crate::session::SessionConfig;
    use crate::session_query::test_source::InMemorySource;
    use llm::{ContentBlock, Message};

    fn session_in_project(id: &str, project: &str) -> ChatSession {
        ChatSession::new_empty(
            id.to_string(),
            id.to_string(),
            SessionConfig {
                initial_project: project.to_string(),
                ..SessionConfig::default()
            },
            None,
        )
    }

    fn wrote(id: &str, project: &str, tool: &str, path: &str) -> ChatSession {
        let mut session = session_in_project(id, project);
        session.add_message(Message::new_user("do it"));
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_tool_use(
                format!("{id}-call"),
                tool,
                serde_json::json!({ "path": path }),
            ),
        ]));
        session
    }

    #[test]
    fn filters_by_project_and_tool_path() {
        let source = InMemorySource::new()
            .with_session(wrote(
                "a",
                "mlflow",
                "write_file",
                "selfhosting-poc/docs/plan.md",
            ))
            .with_session(wrote("b", "mlflow", "write_file", "src/main.rs"))
            .with_session(wrote(
                "c",
                "other",
                "write_file",
                "selfhosting-poc/docs/x.md",
            ));

        let query = SessionSearchQuery {
            project: Some("mlflow".to_string()),
            tool_call: Some(ToolCallFilter {
                names: vec!["write_file".to_string(), "edit".to_string()],
                arg: Some("path".to_string()),
                value: Some(StringMatch::Contains("docs/".to_string())),
            }),
            ..Default::default()
        };

        let matches = search_sessions(&source, &query).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, "a");
        assert_eq!(matches[0].matched_tool_calls.len(), 1);
        assert_eq!(
            matches[0].matched_tool_calls[0].matched_value.as_deref(),
            Some("selfhosting-poc/docs/plan.md")
        );
        assert_eq!(matches[0].matched_tool_calls[0].tool_name, "write_file");
    }

    #[test]
    fn metadata_only_filter_returns_all_in_project() {
        let source = InMemorySource::new()
            .with_session(wrote("a", "mlflow", "write_file", "docs/plan.md"))
            .with_session(wrote("b", "mlflow", "edit", "docs/other.md"))
            .with_session(wrote("c", "other", "write_file", "docs/z.md"));

        let query = SessionSearchQuery {
            project: Some("mlflow".to_string()),
            ..Default::default()
        };
        let mut ids: Vec<String> = search_sessions(&source, &query)
            .unwrap()
            .into_iter()
            .map(|m| m.session_id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn text_contains_matches_message_text() {
        let mut hit = session_in_project("hit", "mlflow");
        hit.add_message(Message::new_user("Investigate the OAuth token refresh"));
        let mut miss = session_in_project("miss", "mlflow");
        miss.add_message(Message::new_user("Unrelated topic"));

        let source = InMemorySource::new().with_session(hit).with_session(miss);

        let query = SessionSearchQuery {
            text_contains: Some("oauth token".to_string()),
            ..Default::default()
        };
        let matches = search_sessions(&source, &query).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, "hit");
    }

    #[test]
    fn arg_presence_without_value_matches_any_value() {
        let source =
            InMemorySource::new().with_session(wrote("a", "mlflow", "write_file", "anything.md"));

        let query = SessionSearchQuery {
            tool_call: Some(ToolCallFilter {
                names: vec!["write_file".to_string()],
                arg: Some("path".to_string()),
                value: None,
            }),
            ..Default::default()
        };
        let matches = search_sessions(&source, &query).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].matched_tool_calls[0].matched_value.as_deref(),
            Some("anything.md")
        );
    }

    #[test]
    fn value_without_arg_is_an_error() {
        let source = InMemorySource::new();
        let query = SessionSearchQuery {
            tool_call: Some(ToolCallFilter {
                names: vec![],
                arg: None,
                value: Some(StringMatch::Contains("x".to_string())),
            }),
            ..Default::default()
        };
        assert!(search_sessions(&source, &query).is_err());
    }

    #[test]
    fn limit_caps_results() {
        let source = InMemorySource::new()
            .with_session(wrote("a", "mlflow", "write_file", "1.md"))
            .with_session(wrote("b", "mlflow", "write_file", "2.md"))
            .with_session(wrote("c", "mlflow", "write_file", "3.md"));
        let query = SessionSearchQuery {
            project: Some("mlflow".to_string()),
            limit: Some(2),
            ..Default::default()
        };
        assert_eq!(search_sessions(&source, &query).unwrap().len(), 2);
    }

    #[test]
    fn all_branches_scope_finds_off_path_calls() {
        let mut session = session_in_project("a", "mlflow");
        session.add_message(Message::new_user("root")); // node 1
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_tool_use("on", "write_file", serde_json::json!({ "path": "on.md" })),
        ])); // node 2
        session.add_message_with_parent(
            Message::new_assistant_content(vec![ContentBlock::new_tool_use(
                "off",
                "write_file",
                serde_json::json!({ "path": "secret-docs/off.md" }),
            )]),
            Some(1),
        ); // node 3
        session.switch_branch(2).unwrap(); // active path excludes node 3

        let source = InMemorySource::new().with_session(session);

        let base = ToolCallFilter {
            names: vec!["write_file".to_string()],
            arg: Some("path".to_string()),
            value: Some(StringMatch::Contains("secret-docs/".to_string())),
        };

        // Active-path scope misses the off-path call.
        let active = SessionSearchQuery {
            tool_call: Some(base.clone()),
            ..Default::default()
        };
        assert_eq!(search_sessions(&source, &active).unwrap().len(), 0);

        // All-branches scope finds it.
        let all = SessionSearchQuery {
            tool_call: Some(base),
            include_all_branches: true,
            ..Default::default()
        };
        assert_eq!(search_sessions(&source, &all).unwrap().len(), 1);
    }
}
