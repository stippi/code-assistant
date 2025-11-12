use super::{PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason};
use crate::acp::ACPUserUI;
use agent_client_protocol::{self as acp, Client};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::{runtime::Handle, task::block_in_place};

const ALLOW_OPTION_ID: &str = "allow-once";
const DENY_OPTION_ID: &str = "deny-once";

pub struct AcpPermissionMediator {
    session_id: acp::SessionId,
    connection: Arc<acp::AgentSideConnection>,
    ui: Arc<ACPUserUI>,
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
        acp::ToolCallUpdate {
            id: acp::ToolCallId(id.into()),
            meta: None,
            fields: acp::ToolCallUpdateFields {
                kind: Some(acp::ToolKind::Execute),
                status: Some(acp::ToolCallStatus::Pending),
                title: Some(format!("{} (permission required)", request.tool_name)),
                content: Some(vec![acp::ToolCallContent::Content {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: self.reason_summary(&request.reason),
                        meta: None,
                    }),
                }]),
                locations: None,
                raw_input: Some(self.reason_metadata(&request.reason)),
                raw_output: None,
            },
        }
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
        request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision> {
        let tool_call = self.tool_call_update(&request);
        let options = vec![
            acp::PermissionOption {
                id: acp::PermissionOptionId::from(ALLOW_OPTION_ID),
                name: "Allow this command".into(),
                kind: acp::PermissionOptionKind::AllowOnce,
                meta: None,
            },
            acp::PermissionOption {
                id: acp::PermissionOptionId::from(DENY_OPTION_ID),
                name: "Deny".into(),
                kind: acp::PermissionOptionKind::RejectOnce,
                meta: None,
            },
        ];

        let request = acp::RequestPermissionRequest {
            session_id: self.session_id.clone(),
            tool_call,
            options,
            meta: None,
        };

        let connection = self.connection.clone();
        let handle = Handle::current();
        let response = block_in_place(|| {
            handle.block_on(async move { connection.request_permission(request).await })
        })?;

        let decision = match response.outcome {
            acp::RequestPermissionOutcome::Cancelled => PermissionDecision::Denied,
            acp::RequestPermissionOutcome::Selected { option_id }
                if option_id == acp::PermissionOptionId::from(ALLOW_OPTION_ID) =>
            {
                PermissionDecision::Granted
            }
            acp::RequestPermissionOutcome::Selected { option_id }
                if option_id == acp::PermissionOptionId::from(DENY_OPTION_ID) =>
            {
                PermissionDecision::Denied
            }
            acp::RequestPermissionOutcome::Selected { option_id } => {
                return Err(anyhow!(
                    "Unknown permission option selected: {}",
                    option_id.0
                ))
            }
        };

        Ok(decision)
    }
}
