//! The voice agent's tools: they act on conversations (sessions) and do
//! nothing else.
//!
//! Every tool returns at once. `message_conversation` starts or queues a
//! turn and leaves it running; the agent hears about its end through a
//! notification.

use super::source::OpenRequests;
use crate::persistence::ChatMetadata;
use crate::session::SessionService;
use crate::session::instance::SessionActivityState;
use crate::session_query::{ContentItem, ContentKind, ContentPart, ContentProjection};
use anyhow::{Context, Result, anyhow, bail};
use llm::realtime::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::SystemTime;

/// Turns `get_conversation` returns with `full`.
pub const FULL_TURNS: usize = 10;
/// Characters per message `get_conversation` returns.
pub const MAX_MESSAGE_CHARS: usize = 2_000;
/// Characters of all messages of one `get_conversation` call.
pub const MAX_TOTAL_CHARS: usize = 16_000;
const DEFAULT_LIST_LIMIT: usize = 20;
const MAX_LIST_LIMIT: usize = 50;

/// What a tool call means for the voice agent beyond its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEffect {
    /// The model read the conversation: its notification is known.
    Read(String),
    /// The model created or messaged the conversation.
    Touched(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// JSON text for the function call output.
    pub output: String,
    pub effects: Vec<ToolEffect>,
}

impl ToolOutcome {
    pub fn error(error: anyhow::Error) -> Self {
        Self {
            output: json!({ "error": format!("{error:#}") }).to_string(),
            effects: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct VoiceTools {
    service: SessionService,
    open_requests: OpenRequests,
}

impl VoiceTools {
    pub fn new(service: SessionService, open_requests: OpenRequests) -> Self {
        Self {
            service,
            open_requests,
        }
    }

    pub fn definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "list_conversations".into(),
                description: "List the user's conversations with coding agents, most recently \
                    active first, with their state (running, idle, errored, or waiting for the \
                    user)."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Only conversations of this project." },
                        "include_settled": { "type": "boolean", "description": "Also list settled (archived) conversations. Default false." },
                        "limit": { "type": "integer", "description": "Maximum number of conversations, default 20." }
                    },
                }),
            },
            ToolDefinition {
                name: "list_projects".into(),
                description: "List the projects a new conversation can be started in.".into(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            ToolDefinition {
                name: "message_conversation".into(),
                description: "Send a message to a coding agent conversation. Without \
                    conversation_id a new conversation is started in `project`. If the agent is \
                    busy the message is queued for its next turn. Returns immediately; you get a \
                    notification when the agent finishes."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "conversation_id": { "type": "string", "description": "The conversation to message; empty to start a new one." },
                        "project": { "type": "string", "description": "Project of a new conversation (see list_projects)." },
                        "title": { "type": "string", "description": "Short title of a new conversation." },
                        "message": { "type": "string", "description": "The message, written as the user's instruction to the agent." }
                    },
                    "required": ["message"],
                }),
            },
            ToolDefinition {
                name: "get_conversation".into(),
                description: "Read a conversation. By default returns its state and the agent's \
                    last message. With full=true returns the last 10 turns, each with the user's \
                    message and the agent's final message of that turn."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "conversation_id": { "type": "string" },
                        "full": { "type": "boolean", "description": "Return the last 10 turns. Default false." }
                    },
                    "required": ["conversation_id"],
                }),
            },
        ]
    }

    /// Run a tool. Failures become an `{"error": …}` output the model can
    /// read; they never end the voice session.
    pub async fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        let arguments: Value = if arguments.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(arguments) {
                Ok(value) => value,
                Err(e) => return ToolOutcome::error(anyhow!("invalid arguments: {e}")),
            }
        };
        let result = match name {
            "list_conversations" => self.list_conversations(arguments).await,
            "list_projects" => self.list_projects().await,
            "message_conversation" => self.message_conversation(arguments).await,
            "get_conversation" => self.get_conversation(arguments).await,
            other => Err(anyhow!("unknown tool {other}")),
        };
        result.unwrap_or_else(ToolOutcome::error)
    }

    async fn list_conversations(&self, arguments: Value) -> Result<ToolOutcome> {
        #[derive(Deserialize)]
        struct Args {
            project: Option<String>,
            #[serde(default)]
            include_settled: bool,
            limit: Option<usize>,
        }
        let args: Args = serde_json::from_value(arguments)?;
        let mut sessions = self.service.list_sessions().await?;
        let lifecycles = self.service.list_session_lifecycles().await?;
        let states = self.service.session_activity_states().await?;
        let now = SystemTime::now();

        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        let limit = args
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);
        let conversations: Vec<Value> = sessions
            .iter()
            .filter(|s| {
                args.project
                    .as_deref()
                    .is_none_or(|p| s.initial_project.eq_ignore_ascii_case(p))
            })
            .filter(|s| {
                args.include_settled || !lifecycles.get(&s.id).is_some_and(|l| l.is_settled())
            })
            .take(limit)
            .map(|s| {
                json!({
                    "id": s.id,
                    "title": s.name,
                    "project": s.initial_project,
                    "branch": s.branch,
                    "state": self.state_label(&s.id, states.get(&s.id)),
                    "last_active": relative_time(now, s.updated_at),
                })
            })
            .collect();
        Ok(ToolOutcome {
            output: json!({ "conversations": conversations }).to_string(),
            effects: Vec::new(),
        })
    }

    async fn list_projects(&self) -> Result<ToolOutcome> {
        let projects = self.project_names().await?;
        Ok(ToolOutcome {
            output: json!({ "projects": projects }).to_string(),
            effects: Vec::new(),
        })
    }

    async fn project_names(&self) -> Result<Vec<String>> {
        let mut names: BTreeSet<String> = crate::config::load_projects()
            .unwrap_or_default()
            .into_keys()
            .collect();
        for session in self.service.list_sessions().await? {
            if !session.initial_project.is_empty() {
                names.insert(session.initial_project);
            }
        }
        Ok(names.into_iter().collect())
    }

    async fn message_conversation(&self, arguments: Value) -> Result<ToolOutcome> {
        #[derive(Deserialize)]
        struct Args {
            conversation_id: Option<String>,
            project: Option<String>,
            title: Option<String>,
            message: String,
        }
        let args: Args = serde_json::from_value(arguments)?;
        if args.message.trim().is_empty() {
            bail!("message is empty");
        }
        let existing = args.conversation_id.filter(|id| !id.trim().is_empty());

        let (conversation_id, status) = match existing {
            Some(id) => {
                let busy = self.service.is_session_busy(id.clone()).await?;
                self.service
                    .send_or_queue_user_message(id.clone(), args.message, Vec::new())
                    .await?;
                (id, if busy { "queued" } else { "started" })
            }
            None => {
                let project = args
                    .project
                    .filter(|p| !p.trim().is_empty())
                    .context("project is required to start a new conversation")?;
                let known = self.project_names().await?;
                let project = known
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(&project))
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!(
                            "unknown project '{project}'; known projects: {}",
                            known.join(", ")
                        )
                    })?;
                let title = args.title.filter(|t| !t.trim().is_empty());
                let id = self.service.create_session(title, Some(project)).await?;
                self.service
                    .send_user_message(id.clone(), args.message, Vec::new(), None)
                    .await?;
                (id, "started")
            }
        };
        Ok(ToolOutcome {
            output: json!({ "conversation_id": conversation_id, "status": status }).to_string(),
            effects: vec![ToolEffect::Touched(conversation_id)],
        })
    }

    async fn get_conversation(&self, arguments: Value) -> Result<ToolOutcome> {
        #[derive(Deserialize)]
        struct Args {
            conversation_id: String,
            #[serde(default)]
            full: bool,
        }
        let args: Args = serde_json::from_value(arguments)?;
        let id = args.conversation_id;
        let content = self
            .service
            .session_content(
                id.clone(),
                ContentProjection {
                    parts: vec![ContentPart::UserText, ContentPart::AssistantText],
                    ..Default::default()
                },
            )
            .await?;
        let states = self.service.session_activity_states().await?;
        let turns = group_turns(&content.items);

        let mut output = json!({
            "id": id,
            "title": content.name,
            "project": content.project,
            "state": self.state_label(&id, states.get(&id)),
        });
        if args.full {
            output["turns"] = json!(last_turns(&turns, FULL_TURNS));
        } else {
            let last = turns
                .iter()
                .rev()
                .find_map(|turn| turn.agent.as_deref())
                .map(|text| truncate(text, MAX_MESSAGE_CHARS));
            output["last_agent_message"] = json!(last);
        }
        Ok(ToolOutcome {
            output: output.to_string(),
            effects: vec![ToolEffect::Read(id)],
        })
    }

    fn state_label(&self, id: &str, state: Option<&SessionActivityState>) -> String {
        if let Some(waiting) = self.open_requests.waiting_for(id) {
            return waiting;
        }
        match state {
            None | Some(SessionActivityState::Idle) => "idle".into(),
            Some(SessionActivityState::Errored { message }) => format!("errored: {message}"),
            Some(SessionActivityState::RunningExternally) => "running in another window".into(),
            Some(
                SessionActivityState::AgentRunning
                | SessionActivityState::WaitingForResponse
                | SessionActivityState::RateLimited { .. },
            ) => "running".into(),
        }
    }
}

/// One exchange: a user message and the agent's last text before the next
/// user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub user: Option<String>,
    pub agent: Option<String>,
}

pub fn group_turns(items: &[ContentItem]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for item in items {
        match item.kind {
            ContentKind::UserText => turns.push(Turn {
                user: Some(item.text.clone()),
                agent: None,
            }),
            ContentKind::AssistantText => {
                if item.text.trim().is_empty() {
                    continue;
                }
                match turns.last_mut() {
                    Some(turn) => turn.agent = Some(item.text.clone()),
                    None => turns.push(Turn {
                        user: None,
                        agent: Some(item.text.clone()),
                    }),
                }
            }
            _ => {}
        }
    }
    turns
}

/// The last `count` turns as JSON, each message truncated, oldest dropped
/// first when the total exceeds [`MAX_TOTAL_CHARS`].
fn last_turns(turns: &[Turn], count: usize) -> Vec<Value> {
    let start = turns.len().saturating_sub(count);
    let mut selected: Vec<(Option<String>, Option<String>)> = turns[start..]
        .iter()
        .map(|turn| {
            (
                turn.user.as_deref().map(|t| truncate(t, MAX_MESSAGE_CHARS)),
                turn.agent
                    .as_deref()
                    .map(|t| truncate(t, MAX_MESSAGE_CHARS)),
            )
        })
        .collect();
    let size = |turns: &[(Option<String>, Option<String>)]| -> usize {
        turns
            .iter()
            .map(|(u, a)| u.as_ref().map_or(0, |t| t.len()) + a.as_ref().map_or(0, |t| t.len()))
            .sum()
    };
    while selected.len() > 1 && size(&selected) > MAX_TOTAL_CHARS {
        selected.remove(0);
    }
    selected
        .into_iter()
        .map(|(user, agent)| json!({ "user": user, "agent": agent }))
        .collect()
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}… [truncated]")
}

fn relative_time(now: SystemTime, then: SystemTime) -> String {
    let seconds = now.duration_since(then).map(|d| d.as_secs()).unwrap_or(0);
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} minutes ago", seconds / 60),
        3600..86_400 => format!("{} hours ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// The metadata of a conversation, for notifications.
pub(super) fn find_metadata<'a>(
    sessions: &'a [ChatMetadata],
    id: &str,
) -> Option<&'a ChatMetadata> {
    sessions.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::NodeId;
    use crate::session_query::Role;

    fn item(kind: ContentKind, text: &str) -> ContentItem {
        ContentItem {
            message_index: 0,
            node_id: NodeId::default(),
            role: if kind == ContentKind::UserText {
                Role::User
            } else {
                Role::Assistant
            },
            kind,
            text: text.into(),
            tool_name: None,
            tool_input: None,
            is_error: None,
            truncated: false,
        }
    }

    #[test]
    fn turns_keep_only_the_last_agent_text() {
        let items = vec![
            item(ContentKind::UserText, "fix it"),
            item(ContentKind::AssistantText, "looking"),
            item(ContentKind::AssistantText, "fixed"),
            item(ContentKind::UserText, "thanks"),
        ];
        assert_eq!(
            group_turns(&items),
            vec![
                Turn {
                    user: Some("fix it".into()),
                    agent: Some("fixed".into())
                },
                Turn {
                    user: Some("thanks".into()),
                    agent: None
                },
            ]
        );
    }

    #[test]
    fn last_turns_are_limited_in_count_and_size() {
        let turns: Vec<Turn> = (0..15)
            .map(|i| Turn {
                user: Some(format!("q{i}")),
                agent: Some("a".repeat(5_000)),
            })
            .collect();
        let selected = last_turns(&turns, FULL_TURNS);
        // 2 000 chars per message (+ marker) caps at 7 turns of 16 000.
        assert!(selected.len() <= FULL_TURNS);
        assert_eq!(selected.last().unwrap()["user"], "q14");
        let total: usize = selected
            .iter()
            .map(|t| t["agent"].as_str().unwrap().len() + t["user"].as_str().unwrap().len())
            .sum();
        assert!(total <= MAX_TOTAL_CHARS);
        assert!(
            selected[0]["agent"]
                .as_str()
                .unwrap()
                .ends_with("[truncated]")
        );

        let few = last_turns(&turns[..3], FULL_TURNS);
        assert_eq!(few.len(), 3);
    }

    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate("äöü", 3), "äöü");
        assert_eq!(truncate("äöüß", 3), "äöü… [truncated]");
    }
}
