use anyhow::Result;
use std::path::PathBuf;

pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput>;
}

pub struct DefaultCommandExecutor;

#[async_trait::async_trait]
impl CommandExecutor for DefaultCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput> {
        let mut parts = command_line.split_whitespace();
        let command = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("Empty command line"))?;

        let mut cmd = std::process::Command::new(command);
        cmd.args(parts);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let output = cmd.output()?;

        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
