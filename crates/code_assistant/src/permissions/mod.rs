pub mod acp;

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

pub use acp::AcpPermissionMediator;

/// Context about why permission is being requested.
#[derive(Debug)]
pub enum PermissionRequestReason<'a> {
    ExecuteCommand {
        command_line: &'a str,
        working_dir: Option<&'a Path>,
    },
}

/// Request payload passed to a [`PermissionMediator`].
#[derive(Debug)]
pub struct PermissionRequest<'a> {
    pub tool_id: Option<&'a str>,
    pub tool_name: &'a str,
    pub reason: PermissionRequestReason<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    GrantedOnce,
    GrantedSession,
    Denied,
}

#[async_trait]
pub trait PermissionMediator: Send + Sync {
    async fn request_permission(
        &self,
        request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision>;
}
