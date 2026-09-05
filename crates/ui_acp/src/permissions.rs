// The generic permission types live with the tool core; re-exported here so
// the mediator and its types share one import path.
pub use tools_core::permissions::{
    PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason,
};

use crate::{ACPUserUI, ClientConn};
use agent_client_protocol::schema as acp;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use code_assistant_core::session::permissions::permission_options_for;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Stable option id for a decision, round-tripped through the ACP protocol.
fn option_id(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::GrantedOnce => "granted-once",
        PermissionDecision::GrantedSession => "granted-session",
        PermissionDecision::GrantedPersistent => "granted-persistent",
        PermissionDecision::Denied => "denied",
    }
}

/// The ACP option kind that renders each decision appropriately.
fn option_kind(decision: PermissionDecision) -> acp::PermissionOptionKind {
    match decision {
        PermissionDecision::GrantedOnce => acp::PermissionOptionKind::AllowOnce,
        PermissionDecision::GrantedSession | PermissionDecision::GrantedPersistent => {
            acp::PermissionOptionKind::AllowAlways
        }
        PermissionDecision::Denied => acp::PermissionOptionKind::RejectOnce,
    }
}

pub struct AcpPermissionMediator {
    session_id: acp::SessionId,
    conn: ClientConn,
    ui: Arc<ACPUserUI>,
    allow_execute_command_always: AtomicBool,
}

impl AcpPermissionMediator {
    pub fn new(session_id: acp::SessionId, conn: ClientConn, ui: Arc<ACPUserUI>) -> Self {
        Self {
            session_id,
            conn,
            ui,
            allow_execute_command_always: AtomicBool::new(false),
        }
    }

    fn tool_call_update(&self, request: &PermissionRequest<'_>) -> acp::ToolCallUpdate {
        if let Some(id) = request.tool_id
            && let Some(snapshot) = self.ui.tool_call_update(id)
        {
            return snapshot;
        }

        let id = request
            .tool_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| format!("permission-{}", request.tool_name));

        let fields = acp::ToolCallUpdateFields::new()
            .kind(acp::ToolKind::Execute)
            .status(acp::ToolCallStatus::Pending)
            .title(format!("{} (permission required)", request.tool_name))
            .content(vec![acp::ToolCallContent::Content(acp::Content::new(
                acp::ContentBlock::Text(acp::TextContent::new(
                    self.reason_summary(request.tool_name, &request.reason),
                )),
            ))])
            .raw_input(self.reason_metadata(&request.reason));

        acp::ToolCallUpdate::new(acp::ToolCallId::new(id), fields)
    }

    fn reason_summary(&self, tool_name: &str, reason: &PermissionRequestReason<'_>) -> String {
        match reason {
            PermissionRequestReason::ExecuteCommand {
                command_line,
                working_dir,
            } => match working_dir {
                Some(dir) => format!(
                    "Command: `{}`\nWorking directory: {}",
                    command_line,
                    dir.display()
                ),
                None => format!("Command: `{}`", command_line),
            },
            PermissionRequestReason::ToolInvocation { params } => {
                format!(
                    "Tool: `{}`\nParameters: {}",
                    tool_name,
                    serde_json::to_string_pretty(params).unwrap_or_else(|_| params.to_string())
                )
            }
            PermissionRequestReason::TrustLocalMcp {
                project_dir,
                server_names,
            } => format!(
                "Trust MCP servers from .mcp.json in {}?\nLaunches: {}",
                project_dir.display(),
                if server_names.is_empty() {
                    "(none)".to_string()
                } else {
                    server_names.join(", ")
                }
            ),
        }
    }

    fn reason_metadata(&self, reason: &PermissionRequestReason<'_>) -> serde_json::Value {
        match reason {
            PermissionRequestReason::ExecuteCommand {
                command_line,
                working_dir,
            } => json!({
                "type": "execute_command",
                "command_line": command_line,
                "working_dir": working_dir.map(|dir| dir.display().to_string()),
            }),
            PermissionRequestReason::ToolInvocation { params } => json!({
                "type": "tool_invocation",
                "params": params,
            }),
            PermissionRequestReason::TrustLocalMcp {
                project_dir,
                server_names,
            } => json!({
                "type": "trust_local_mcp",
                "project_dir": project_dir.display().to_string(),
                "server_names": server_names,
            }),
        }
    }
}

#[async_trait]
impl PermissionMediator for AcpPermissionMediator {
    async fn request_permission(
        &self,
        permission_request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision> {
        if matches!(
            permission_request.reason,
            PermissionRequestReason::ExecuteCommand { .. }
        ) && self.allow_execute_command_always.load(Ordering::Relaxed)
        {
            return Ok(PermissionDecision::GrantedSession);
        }

        let tool_call = self.tool_call_update(&permission_request);
        // The choices are the shared source of truth; ACP just renders them
        // and maps the selection back to a decision.
        let options = permission_options_for(&permission_request.reason)
            .into_iter()
            .map(|option| {
                acp::PermissionOption::new(
                    option_id(option.decision),
                    option.label,
                    option_kind(option.decision),
                )
            })
            .collect();

        let acp_request =
            acp::RequestPermissionRequest::new(self.session_id.clone(), tool_call, options);

        // The SDK connection is `Send`, so we can simply await the request from
        // the agent task (no `block_in_place` needed).
        let response = self
            .conn
            .send_request(acp_request)
            .block_task()
            .await
            .map_err(|e| anyhow!("Failed to request permission: {e}"))?;

        let decision = match response.outcome {
            acp::RequestPermissionOutcome::Cancelled => PermissionDecision::Denied,
            acp::RequestPermissionOutcome::Selected(selected) => [
                PermissionDecision::GrantedOnce,
                PermissionDecision::GrantedSession,
                PermissionDecision::GrantedPersistent,
                PermissionDecision::Denied,
            ]
            .into_iter()
            .find(|decision| {
                selected.option_id == acp::PermissionOptionId::from(option_id(*decision))
            })
            .ok_or_else(|| {
                anyhow!(
                    "Unknown permission option selected: {}",
                    selected.option_id.0
                )
            })?,
            // Non-exhaustive enum - handle future variants
            _ => return Err(anyhow!("Unknown permission outcome variant")),
        };

        // Remember an "always allow" for command execution so we stop asking.
        if decision == PermissionDecision::GrantedSession
            && matches!(
                permission_request.reason,
                PermissionRequestReason::ExecuteCommand { .. }
            )
        {
            self.allow_execute_command_always
                .store(true, Ordering::Relaxed);
        }

        Ok(decision)
    }
}
