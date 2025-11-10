use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use command_executor::{CommandExecutor, CommandOutput, StreamingCallback};
use sandbox::SandboxPolicy;
use tracing::trace;

/// Wraps a command executor with sandbox policy metadata. Actual enforcement will be
/// introduced per-platform; for now this records intent and keeps the policy accessible.
pub struct SandboxedCommandExecutor {
    inner: Box<dyn CommandExecutor>,
    policy: SandboxPolicy,
    session_id: Option<String>,
}

impl SandboxedCommandExecutor {
    pub fn new(
        inner: Box<dyn CommandExecutor>,
        policy: SandboxPolicy,
        session_id: Option<String>,
    ) -> Self {
        Self {
            inner,
            policy,
            session_id,
        }
    }

    #[allow(dead_code)]
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }
}

#[async_trait]
impl CommandExecutor for SandboxedCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput> {
        trace!(
            session_id = ?self.session_id,
            ?self.policy,
            working_dir = ?working_dir,
            "Executing command under sandbox policy"
        );
        self.inner.execute(command_line, working_dir).await
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        trace!(
            session_id = ?self.session_id,
            ?self.policy,
            working_dir = ?working_dir,
            "Executing streaming command under sandbox policy"
        );
        self.inner
            .execute_streaming(command_line, working_dir, callback)
            .await
    }
}
