use anyhow::Result;
use std::path::{Path, PathBuf};

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

/// Quote a path for the current platform so spaces and special chars are preserved when passed
/// through the shell. This is a best-effort helper; it does not aim to be a full shell-quoting lib.
pub fn shell_quote_path(path: &Path) -> String {
    #[cfg(target_family = "unix")]
    {
        let s = path.to_string_lossy();
        // Only quote if whitespace is present; basic behavior for tests
        if s.chars().any(|c| c.is_whitespace()) {
            let escaped = s.replace('\'', "'\\''");
            format!("'{escaped}'")
        } else {
            s.to_string()
        }
    }

    #[cfg(target_family = "windows")]
    {
        let s = path.to_string_lossy();
        if s.chars().any(|c| c.is_whitespace()) {
            // Surround with double quotes and escape internal quotes by doubling them
            let escaped = s.replace('"', "\"\"");
            format!("\"{escaped}\"")
        } else {
            s.to_string()
        }
    }
}

/// Build a formatter command line from a template. If the template contains the {path} placeholder,
/// it will be replaced with the (quoted) relative path. If not present, the template is returned as-is.
pub fn build_format_command(template: &str, relative_path: &Path) -> String {
    if template.contains("{path}") {
        let quoted = shell_quote_path(relative_path);
        template.replace("{path}", &quoted)
    } else {
        template.to_string()
    }
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
        cmd.args(["-c", &format!("{command_line} 2>&1")]);

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
