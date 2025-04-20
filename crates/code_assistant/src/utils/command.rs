use anyhow::Result;
use std::path::PathBuf;

#[derive(Clone)]
pub struct CommandOutput {
    pub success: bool,
    pub output: String,
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
        // Validate working_dir first
        if let Some(dir) = working_dir {
            if !dir.exists() {
                return Err(anyhow::anyhow!(
                    "Working directory does not exist: {}",
                    dir.display()
                ));
            }
            if !dir.is_dir() {
                return Err(anyhow::anyhow!(
                    "Path is not a directory: {}",
                    dir.display()
                ));
            }
        }

        // Create shell command using login shell or fallback
        #[cfg(target_family = "unix")]
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        #[cfg(target_family = "unix")]
        let mut cmd = std::process::Command::new(shell);
        #[cfg(target_family = "unix")]
        cmd.args(["-c", &format!("{} 2>&1", command_line)]);

        #[cfg(target_family = "windows")]
        let mut cmd = std::process::Command::new("cmd");
        #[cfg(target_family = "windows")]
        cmd.args(["/C", &format!("{} 2>&1", command_line)]);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        let output = cmd.output()?;

        Ok(CommandOutput {
            success: output.status.success(),
            output: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}
