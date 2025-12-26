use super::{PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason};
use crate::acp::ACPUserUI;
use agent_client_protocol::{self as acp, Client};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::{runtime::Handle, task::block_in_place};

const ALLOW_ALWAYS_OPTION_ID: &str = "allow-always";
const ALLOW_OPTION_ID: &str = "allow-once";
const DENY_OPTION_ID: &str = "deny-once";

pub struct AcpPermissionMediator {
    session_id: acp::SessionId,
    connection: Arc<acp::AgentSideConnection>,
    ui: Arc<ACPUserUI>,
    allow_execute_command_always: AtomicBool,
}

impl AcpPermissionMediator {
    pub fn new(
        session_id: acp::SessionId,
        connection: Arc<acp::AgentSideConnection>,
        ui: Arc<ACPUserUI>,
    ) -> Self {
        Self {
            session_id,
            connection,
            ui,
            allow_execute_command_always: AtomicBool::new(false),
        }
    }

    fn tool_call_update(&self, request: &PermissionRequest<'_>) -> acp::ToolCallUpdate {
        if let Some(id) = request.tool_id {
            if let Some(snapshot) = self.ui.tool_call_update(id) {
                return snapshot;
            }
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
                    self.reason_summary(&request.reason),
                )),
            ))])
            .raw_input(self.reason_metadata(&request.reason));

        acp::ToolCallUpdate::new(acp::ToolCallId::new(id), fields)
    }

    fn reason_summary(&self, reason: &PermissionRequestReason<'_>) -> String {
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
        let options = vec![
            acp::PermissionOption::new(
                ALLOW_ALWAYS_OPTION_ID,
                "Always allow in this session",
                acp::PermissionOptionKind::AllowAlways,
            ),
            acp::PermissionOption::new(
                ALLOW_OPTION_ID,
                "Allow this command",
                acp::PermissionOptionKind::AllowOnce,
            ),
            acp::PermissionOption::new(
                DENY_OPTION_ID,
                "Deny",
                acp::PermissionOptionKind::RejectOnce,
            ),
        ];

        let acp_request =
            acp::RequestPermissionRequest::new(self.session_id.clone(), tool_call, options);

        let connection = self.connection.clone();
        let handle = Handle::current();
        let response = block_in_place(|| {
            handle.block_on(async move { connection.request_permission(acp_request).await })
        })?;

        let decision = match response.outcome {
            acp::RequestPermissionOutcome::Cancelled => PermissionDecision::Denied,
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id == acp::PermissionOptionId::from(ALLOW_ALWAYS_OPTION_ID) =>
            {
                if matches!(
                    permission_request.reason,
                    PermissionRequestReason::ExecuteCommand { .. }
                ) {
                    self.allow_execute_command_always
                        .store(true, Ordering::Relaxed);
                }
                PermissionDecision::GrantedSession
            }
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id == acp::PermissionOptionId::from(ALLOW_OPTION_ID) =>
            {
                PermissionDecision::GrantedOnce
            }
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id == acp::PermissionOptionId::from(DENY_OPTION_ID) =>
            {
                PermissionDecision::Denied
            }
            acp::RequestPermissionOutcome::Selected(selected) => {
                return Err(anyhow!(
                    "Unknown permission option selected: {}",
                    selected.option_id.0
                ))
            }
            // Non-exhaustive enum - handle future variants
            _ => return Err(anyhow!("Unknown permission outcome variant")),
        };

        Ok(decision)
    }
}
