use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::ToolServicesAccess;
use crate::ui::streaming::DisplayFragment;
use crate::ui::UserInterface;
use anyhow::{anyhow, Result};
use command_executor::{SandboxCommandRequest, StreamingCallback};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use tools_core::permissions::{PermissionDecision, PermissionRequest, PermissionRequestReason};

// Input type for the execute_command tool
#[derive(Deserialize, Serialize)]
pub struct ExecuteCommandInput {
    pub project: String,
    pub command_line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ask_user_approval: bool,
    /// Session mode: allocate a real terminal (PTY) with open stdin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<bool>,
    /// Session mode: how long to wait for output before returning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ExecuteCommandOutput {
    #[allow(dead_code)]
    pub project: String,
    pub command_line: String,
    #[allow(dead_code)]
    pub working_dir: Option<PathBuf>,
    pub output: String,
    pub success: bool,
    /// Session mode: id under which the still-running process is tracked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_session_id: Option<u32>,
    /// Session mode: exit code, when the process exited within the yield
    /// window and reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Session mode: the process was still running when the yield window
    /// closed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
}

// Render implementation for output formatting
impl Render for ExecuteCommandOutput {
    fn status(&self) -> String {
        if self.running {
            format!(
                "Command running in session {}: {}",
                self.pty_session_id.unwrap_or(0),
                self.command_line
            )
        } else if self.success {
            format!("Command executed successfully: {}", self.command_line)
        } else {
            format!("Command failed: {}", self.command_line)
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut formatted = String::new();

        // Add execution status
        if self.running {
            formatted.push_str(&format!(
                "Status: Still running (session_id: {})\n",
                self.pty_session_id.unwrap_or(0)
            ));
        } else if self.success {
            formatted.push_str("Status: Success\n");
        } else {
            formatted.push_str("Status: Failed\n");
        }
        if let Some(code) = self.exit_code {
            formatted.push_str(&format!("Exit code: {code}\n"));
        }

        // Add command output with formatting
        formatted.push_str(">>>>> OUTPUT:\n");
        formatted.push_str(&self.output);
        formatted.push_str("\n<<<<< END OF OUTPUT");

        if let Some(session_id) = self.pty_session_id.filter(|_| self.running) {
            formatted.push_str(&format!(
                "\nThe process keeps running in the background. Use the write_stdin tool with session_id {session_id} to send input, poll for more output (empty chars), or interrupt it (chars \"\\u0003\")."
            ));
        }

        formatted
    }

    /// UI display uses raw command output only. The status and delimiters
    /// shown in render() are meant for the LLM context. The terminal card
    /// already conveys success/failure through its header chrome, so
    /// repeating "Status: Success" in the output body is redundant and
    /// causes visual flicker when the card switches between live-PTY and
    /// display-only terminal paths.
    fn render_for_ui(&self, _tracker: &mut ResourcesTracker) -> String {
        self.output.trim_end().to_string()
    }
}

// ToolResult implementation
impl ToolResult for ExecuteCommandOutput {
    fn is_success(&self) -> bool {
        self.success
    }
}

/// Streaming callback implementation for tool output
struct ToolOutputStreamer<'a> {
    ui: &'a dyn UserInterface,
    tool_id: String,
    /// Set by the UI's terminal-card stop button to interrupt this command.
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl<'a> StreamingCallback for ToolOutputStreamer<'a> {
    fn on_output_chunk(&self, chunk: &str) -> Result<()> {
        let fragment = DisplayFragment::ToolOutput {
            tool_id: self.tool_id.clone(),
            chunk: chunk.to_string(),
        };

        // Send to UI synchronously (don't spawn a task to avoid lifetime issues)
        let _ = self.ui.display_fragment(&fragment);

        Ok(())
    }

    fn on_terminal_output_chunk(&self, bytes: &[u8]) -> Result<()> {
        let fragment = DisplayFragment::ToolTerminalOutput {
            tool_id: self.tool_id.clone(),
            bytes: bytes.to_vec(),
        };

        let _ = self.ui.display_fragment(&fragment);

        Ok(())
    }

    fn on_terminal_attached(&self, terminal_id: &str) -> Result<()> {
        let fragment = DisplayFragment::ToolTerminal {
            tool_id: self.tool_id.clone(),
            terminal_id: terminal_id.to_string(),
        };

        let _ = self.ui.display_fragment(&fragment);

        Ok(())
    }

    fn on_terminal_exit(&self, exit_code: Option<i32>) -> Result<()> {
        self.ui.stream_terminal_exit(&self.tool_id, exit_code);
        Ok(())
    }

    fn tool_id(&self) -> Option<&str> {
        Some(&self.tool_id)
    }

    fn should_continue(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|flag| !flag.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(true)
    }
}

// Tool implementation
pub struct ExecuteCommandTool;

#[async_trait::async_trait]
impl Tool for ExecuteCommandTool {
    type Input = ExecuteCommandInput;
    type Output = ExecuteCommandOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Execute a command line or shell script within a specified project. ",
            "By default, blocks until the command returns by itself and then provides all output at once; ",
            "in that mode it must not be used with commands that would keep running forever, unless combined with a timeout. ",
            "Setting `tty` and/or `yield_time_ms` switches to session mode: the command runs in an interactive terminal session, ",
            "the tool returns after `yield_time_ms` with the output so far, and if the process is still running you get a ",
            "session_id for the write_stdin tool to send input, poll for more output, or interrupt. ",
            "Use session mode for long-running processes (builds, servers) and interactive programs (ssh, REPLs, sudo)."
        );
        ToolSpec {
            name: "execute_command".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "examples": ["project-name"],
                        "type": "string",
                        "description": "Name of the project context for the command/script. The reserved values `:config:` and `:system:` instead address the shared user and bundled skill directories (used to run bundled skill scripts)."
                    },
                    "command_line": {
                        "type": "string",
                        "description": "The complete command or shell script to execute"
                    },
                    "working_dir": {
                        "examples": ["Working directory here (optional)"],
                        "type": "string",
                        "description": "Optional: working directory (relative to project root)"
                    },
                    "ask_user_approval": {
                        "type": "boolean",
                        "description": "Set to true if this command should request user approval to run outside the sandbox",
                        "default": false
                    },
                    "tty": {
                        "type": "boolean",
                        "description": "Session mode: run the command in a PTY (real terminal) with stdin kept open, so interactive programs (ssh, REPLs, sudo prompts) work and write_stdin can send input. Set to false for a non-interactive background session (plain pipes, stdin closed). Defaults to true when session mode is active."
                    },
                    "yield_time_ms": {
                        "type": "integer",
                        "description": "Session mode: return after this many milliseconds with the output so far instead of blocking until exit (250-30000, default 10000). If the process is still running, the result carries a session_id for the write_stdin tool."
                    }
                },
                "required": ["project", "command_line"]
            }),
            annotations: Some(json!({
                "readOnlyHint": false,
                "idempotentHint": false
            })),
            capabilities: ToolSpec::capabilities(&[
                capabilities::SCOPE_MCP,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
                capabilities::SCOPE_SUBAGENT_DEFAULT,
                capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
            ]),
            multiline_params: &["command_line"],
            hidden: false,
            title_template: Some("Running: {command_line}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Resolve the scope (a project name, or a reserved skills-scope token
        // such as `:config:` / `:system:`) to a sandboxed explorer. This lets
        // bundled skill scripts under `:config:` / `:system:` be executed.
        let explorer = crate::config::explorer_for_scope(context.project_manager(), &input.project)
            .map_err(|e| anyhow!("Failed to resolve scope {}: {}", input.project, e))?;

        let project_root = explorer.root_dir();

        // Create a PathBuf for the working directory if provided
        let working_dir_path = input.working_dir.as_ref().map(PathBuf::from);

        // Check if working directory is absolute and handle it properly
        if let Some(dir) = &working_dir_path {
            if dir.is_absolute() {
                return Err(anyhow!(
                    "Working directory must be relative to project root"
                ));
            }
        }

        // Prepare effective working directory
        let effective_working_dir = working_dir_path
            .as_ref()
            .map(|dir| project_root.join(dir))
            .unwrap_or_else(|| project_root.clone());

        let mut bypass_sandbox = false;
        if input.ask_user_approval {
            let handler = context.permission_handler.ok_or_else(|| {
                anyhow!(
                    "Cannot request user approval: no permission handler configured for execute_command"
                )
            })?;

            let decision = handler
                .request_permission(PermissionRequest {
                    tool_id: context.tool_id.as_deref(),
                    tool_name: "execute_command",
                    reason: PermissionRequestReason::ExecuteCommand {
                        command_line: &input.command_line,
                        working_dir: Some(effective_working_dir.as_path()),
                    },
                })
                .await?;

            match decision {
                PermissionDecision::Denied => {
                    return Err(anyhow!(
                        "Command execution cancelled: user denied permission"
                    ))
                }
                PermissionDecision::GrantedOnce | PermissionDecision::GrantedSession => {
                    bypass_sandbox = true;
                }
            }
        }

        let mut sandbox_request = SandboxCommandRequest::default();
        sandbox_request.writable_roots.push(project_root.clone());
        sandbox_request.bypass_sandbox = bypass_sandbox;

        // Session mode: run in a PTY/pipe session that can outlive this call.
        if input.tty.is_some() || input.yield_time_ms.is_some() {
            return self
                .execute_session_mode(context, input, effective_working_dir, &sandbox_request)
                .await;
        }

        // A cancel flag lets the UI's stop button interrupt this foreground
        // command; registered under the tool_id, polled by the callback,
        // removed once the command returns.
        let interrupts = context
            .extension::<crate::tools::ToolServices>()
            .and_then(|services| services.terminal_interrupts.clone());
        let cancel = match (&interrupts, &context.tool_id) {
            (Some(interrupts), Some(tool_id)) => Some(interrupts.register(tool_id)),
            _ => None,
        };

        // Execute the command using streaming
        let result = match (context.ui(), &context.tool_id) {
            (Some(ui), Some(tool_id)) => {
                // Create streaming callback for UI output
                let callback = ToolOutputStreamer {
                    ui,
                    tool_id: tool_id.clone(),
                    cancel: cancel.clone(),
                };

                context
                    .command_executor
                    .execute_streaming(
                        &input.command_line,
                        Some(&effective_working_dir),
                        Some(&callback),
                        Some(&sandbox_request),
                    )
                    .await
            }
            _ => {
                // No UI available, use regular execution
                context
                    .command_executor
                    .execute_streaming(
                        &input.command_line,
                        Some(&effective_working_dir),
                        None,
                        Some(&sandbox_request),
                    )
                    .await
            }
        };

        // Always stop tracking the cancel flag, whether the command
        // succeeded, failed, or was interrupted.
        if let (Some(interrupts), Some(tool_id)) = (&interrupts, &context.tool_id) {
            interrupts.unregister(tool_id);
        }
        let result = result?;

        Ok(ExecuteCommandOutput {
            project: input.project.clone(),
            command_line: input.command_line.clone(),
            working_dir: working_dir_path,
            output: result.output,
            success: result.success,
            pty_session_id: None,
            exit_code: None,
            running: false,
        })
    }
}

/// Yield-time bounds for session mode, mirroring the tool schema.
const MIN_YIELD_TIME_MS: u64 = 250;
const MAX_YIELD_TIME_MS: u64 = 30_000;
const DEFAULT_YIELD_TIME_MS: u64 = 10_000;

/// Forwards a PTY session's raw output to a tool card for the session's
/// whole lifetime — including between turns, so a background process keeps
/// streaming live colored output while the agent does other work. Bound to
/// the `execute_command` tool_id that started the session; `write_stdin`
/// reactions surface on the same card.
struct UiTerminalSink {
    ui: std::sync::Arc<dyn UserInterface>,
    tool_id: String,
}

impl pty_session::TerminalOutputSink for UiTerminalSink {
    fn emit(&self, bytes: &[u8]) {
        self.ui.stream_terminal_output(&self.tool_id, bytes);
    }

    fn on_exit(&self, exit_code: Option<i32>) {
        self.ui.stream_terminal_exit(&self.tool_id, exit_code);
    }
}

/// Build the live-output sink for a new session, if the context can stream
/// to a UI. Shared by `execute_command` (session creation) so the sink is
/// baked into the session and outlives the creating tool call.
pub(crate) fn terminal_output_sink(
    context: &ToolContext<'_>,
) -> Option<std::sync::Arc<dyn pty_session::TerminalOutputSink>> {
    let services = context.extension::<crate::tools::ToolServices>()?;
    let ui = services.ui.clone()?;
    let tool_id = context.tool_id.clone()?;
    Some(std::sync::Arc::new(UiTerminalSink { ui, tool_id }))
}

impl ExecuteCommandTool {
    async fn execute_session_mode<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &ExecuteCommandInput,
        effective_working_dir: PathBuf,
        sandbox_request: &SandboxCommandRequest,
    ) -> Result<ExecuteCommandOutput> {
        let Some(manager) = context
            .extension::<crate::tools::ToolServices>()
            .and_then(|services| services.pty_sessions.clone())
        else {
            return Err(anyhow!(
                "Interactive sessions are not available in this environment; run the command without tty/yield_time_ms"
            ));
        };

        let spec = context.command_executor.prepare_pty_spawn(
            &input.command_line,
            &effective_working_dir,
            Some(sandbox_request),
        )?;

        let mut config = pty_session::PtySpawnConfig::from_argv(spec.argv);
        config.env = spec.env;
        config.keep_alive = spec.keep_alive;
        config.tty = input.tty.unwrap_or(true);
        config.working_dir = Some(effective_working_dir);
        // Bind the live-output sink before spawning so raw colored output
        // streams to the card for the session's whole life, independent of
        // this (or any later) tool call's polling window.
        config.output_sink = terminal_output_sink(context);

        let session = std::sync::Arc::new(pty_session::PtySession::spawn(config)?);

        // Announce the terminal before any output exists, so a UI can show
        // the live terminal card (with its stop button) even while the
        // process stays silent. Same signal the blocking PTY executor sends
        // via StreamingCallback::on_terminal_attached.
        if let (Some(ui), Some(tool_id)) = (context.ui(), &context.tool_id) {
            let _ = ui.display_fragment(&DisplayFragment::ToolTerminal {
                tool_id: tool_id.clone(),
                terminal_id: "backend-pty".to_string(),
            });
        }

        let yield_time = std::time::Duration::from_millis(
            input
                .yield_time_ms
                .unwrap_or(DEFAULT_YIELD_TIME_MS)
                .clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS),
        );

        // The sink already streams raw colored output live; here we just
        // wait for the window and emit the sanitized text as one plain
        // ToolOutput chunk (model result + text frontends).
        let collected = session.collect_output(yield_time).await;

        if !collected.output.is_empty() {
            if let (Some(ui), Some(tool_id)) = (context.ui(), &context.tool_id) {
                let _ = ui.display_fragment(&DisplayFragment::ToolOutput {
                    tool_id: tool_id.clone(),
                    chunk: collected.output.clone(),
                });
            }
        }

        let output = match collected.status {
            pty_session::PtySessionStatus::Running => {
                let session_id = manager.register_with_tool_id(
                    session,
                    &input.command_line,
                    context.tool_id.clone(),
                );
                ExecuteCommandOutput {
                    project: input.project.clone(),
                    command_line: input.command_line.clone(),
                    working_dir: input.working_dir.as_ref().map(PathBuf::from),
                    output: collected.output,
                    success: true,
                    pty_session_id: Some(session_id),
                    exit_code: None,
                    running: true,
                }
            }
            pty_session::PtySessionStatus::Exited(code) => ExecuteCommandOutput {
                project: input.project.clone(),
                command_line: input.command_line.clone(),
                working_dir: input.working_dir.as_ref().map(PathBuf::from),
                output: collected.output,
                success: code == Some(0),
                pty_session_id: None,
                exit_code: code,
                running: false,
            },
        };

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use command_executor::CommandOutput;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tools_core::permissions::PermissionMediator;

    struct TestPermissionMediator {
        decision: PermissionDecision,
        call_count: AtomicUsize,
    }

    impl TestPermissionMediator {
        fn new(decision: PermissionDecision) -> Self {
            Self {
                decision,
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PermissionMediator for TestPermissionMediator {
        async fn request_permission(
            &self,
            _request: PermissionRequest<'_>,
        ) -> Result<PermissionDecision> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.decision)
        }
    }

    #[tokio::test]
    async fn test_execute_command_output_rendering() {
        // Create output with test data
        let output = ExecuteCommandOutput {
            project: "test-project".to_string(),
            command_line: "ls -la".to_string(),
            working_dir: Some(PathBuf::from("src")),
            output: "file1.rs\nfile2.rs".to_string(),
            success: true,
            pty_session_id: None,
            exit_code: None,
            running: false,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering
        assert!(rendered.contains("Status: Success"));
        assert!(rendered.contains("file1.rs\nfile2.rs"));
    }

    #[tokio::test]
    async fn test_execute_command_failure_rendering() {
        // Create output with failed command data
        let output = ExecuteCommandOutput {
            project: "test-project".to_string(),
            command_line: "rm -rf /tmp/nonexistent".to_string(),
            working_dir: None,
            output: "rm: cannot remove '/tmp/nonexistent': No such file or directory".to_string(),
            success: false,
            pty_session_id: None,
            exit_code: None,
            running: false,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering for failed command
        assert!(rendered.contains("Status: Failed"));
        assert!(rendered.contains("cannot remove"));
    }

    #[tokio::test]
    async fn test_execute_command_success() -> Result<()> {
        // Create test fixture with command executor and UI
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: true,
            output: "Command output".to_string(),
        })])
        .with_ui()
        .with_tool_id("test-tool-1".to_string());
        let mut context = fixture.context();

        // Create input
        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "ls -la".to_string(),
            working_dir: Some("src".to_string()),
            ask_user_approval: false,
            tty: None,
            yield_time_ms: None,
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result
        assert_eq!(result.command_line, "ls -la");
        assert_eq!(result.output, "Command output"); // Match expected output from mock
        assert!(result.success);

        // Verify command was executed with correct parameters
        let commands = fixture.command_executor().get_captured_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line, "ls -la");
        assert_eq!(commands[0].working_dir, Some(PathBuf::from("./root/src")));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_failure() -> Result<()> {
        // Create test fixture with failing command executor and UI
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: false,
            output: "Command failed: permission denied".to_string(),
        })])
        .with_ui()
        .with_tool_id("test-tool-2".to_string());
        let mut context = fixture.context();

        // Create input
        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "rm -rf /tmp/nonexistent".to_string(),
            working_dir: None,
            ask_user_approval: false,
            tty: None,
            yield_time_ms: None,
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result shows failure
        assert_eq!(result.command_line, "rm -rf /tmp/nonexistent");
        assert_eq!(result.output, "Command failed: permission denied");
        assert!(!result.success);

        // Verify command was executed
        let commands = fixture.command_executor().get_captured_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line, "rm -rf /tmp/nonexistent");
        assert_eq!(commands[0].working_dir, Some(PathBuf::from("./root")));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_streaming() -> Result<()> {
        // Create test fixture with multi-line output and UI for streaming
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: true,
            output: "Line 1\nLine 2\nLine 3\n".to_string(),
        })])
        .with_ui()
        .with_tool_id("test-streaming-tool".to_string());
        let mut context = fixture.context();

        // Create input
        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "echo 'test'".to_string(),
            working_dir: None,
            ask_user_approval: false,
            tty: None,
            yield_time_ms: None,
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result
        assert!(result.success);
        assert_eq!(result.output, "Line 1\nLine 2\nLine 3\n");

        // Verify streaming output was captured
        let streaming_output = fixture.ui().unwrap().get_streaming_output();
        assert!(
            !streaming_output.is_empty(),
            "Should have received streaming output"
        );

        // The streaming output should contain the individual lines
        println!("Streaming output received: {streaming_output:?}");

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_without_permission_flag_does_not_prompt() -> Result<()> {
        let mediator = Arc::new(TestPermissionMediator::new(PermissionDecision::GrantedOnce));
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: true,
            output: "Command output".to_string(),
        })])
        .with_permission_handler(mediator.clone())
        .with_ui()
        .with_tool_id("test-tool-permission-free".to_string());
        let mut context = fixture.context();

        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "ls".to_string(),
            working_dir: None,
            ask_user_approval: false,
            tty: None,
            yield_time_ms: None,
        };

        let tool = ExecuteCommandTool;
        let _ = tool.execute(&mut context, &mut input).await?;

        assert_eq!(
            mediator.calls(),
            0,
            "Permission handler should not be invoked without flag"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_permission_denied() {
        let mediator = Arc::new(TestPermissionMediator::new(PermissionDecision::Denied));
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: true,
            output: "Command output".to_string(),
        })])
        .with_permission_handler(mediator.clone())
        .with_ui()
        .with_tool_id("test-tool-permission-denied".to_string());
        let mut context = fixture.context();

        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "ls".to_string(),
            working_dir: None,
            ask_user_approval: true,
            tty: None,
            yield_time_ms: None,
        };

        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await;
        assert!(result.is_err(), "Execution should fail when user denies");
        assert_eq!(mediator.calls(), 1);
    }

    #[tokio::test]
    async fn test_execute_command_permission_bypasses_sandbox() -> Result<()> {
        let mediator = Arc::new(TestPermissionMediator::new(PermissionDecision::GrantedOnce));
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: true,
            output: "Command output".to_string(),
        })])
        .with_permission_handler(mediator.clone())
        .with_ui()
        .with_tool_id("test-tool-permission-bypass".to_string());
        let mut context = fixture.context();

        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "ls".to_string(),
            working_dir: None,
            ask_user_approval: true,
            tty: None,
            yield_time_ms: None,
        };

        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;
        assert!(result.success);
        assert_eq!(mediator.calls(), 1);

        let commands = fixture.command_executor().get_captured_commands();
        assert_eq!(commands.len(), 1);
        let sandbox_request = commands[0]
            .sandbox_request
            .as_ref()
            .expect("sandbox request should be present");
        assert!(
            sandbox_request.bypass_sandbox,
            "bypass flag should be set after approval"
        );

        Ok(())
    }

    /// Session-mode tests spawn real processes, so they need a project
    /// whose root exists on disk.
    fn session_mode_fixture(dir: &std::path::Path) -> ToolTestFixture {
        let explorer =
            crate::mocks::MockExplorer::new(Default::default(), None).with_root(dir.to_path_buf());
        let project_manager = crate::mocks::MockProjectManager::new().with_project_path(
            "real",
            dir.to_path_buf(),
            Box::new(explorer),
        );
        ToolTestFixture::with_project_manager(project_manager).with_pty_sessions()
    }

    fn session_mode_input(command_line: &str, yield_time_ms: u64) -> ExecuteCommandInput {
        ExecuteCommandInput {
            project: "real".to_string(),
            command_line: command_line.to_string(),
            working_dir: None,
            ask_user_approval: false,
            tty: None,
            yield_time_ms: Some(yield_time_ms),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_mode_returns_session_id_while_running() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut fixture = session_mode_fixture(dir.path());
        let mut context = fixture.context();

        let mut input = session_mode_input("echo started; sleep 30", 500);
        let result = ExecuteCommandTool.execute(&mut context, &mut input).await?;

        assert!(result.running, "process should still be running");
        assert!(result.success, "a running session is not a failure");
        assert!(
            result.output.contains("started"),
            "output: {}",
            result.output
        );
        let session_id = result
            .pty_session_id
            .expect("session id for running process");
        assert_eq!(result.exit_code, None);

        drop(context);
        let manager = fixture.pty_sessions().unwrap();
        let session = manager.get(session_id).expect("session should be tracked");
        session.terminate();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_mode_reports_exit_within_yield_window() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut fixture = session_mode_fixture(dir.path());
        let mut context = fixture.context();

        let mut input = session_mode_input("echo done; exit 3", 10_000);
        let result = ExecuteCommandTool.execute(&mut context, &mut input).await?;

        assert!(!result.running);
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.pty_session_id, None);
        assert!(result.output.contains("done"), "output: {}", result.output);

        drop(context);
        assert!(
            fixture.pty_sessions().unwrap().list().is_empty(),
            "exited processes should not be tracked"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_mode_without_registry_fails_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let explorer = crate::mocks::MockExplorer::new(Default::default(), None)
            .with_root(dir.path().to_path_buf());
        let project_manager = crate::mocks::MockProjectManager::new().with_project_path(
            "real",
            dir.path().to_path_buf(),
            Box::new(explorer),
        );
        // No .with_pty_sessions() — e.g. the MCP server context.
        let mut fixture = ToolTestFixture::with_project_manager(project_manager);
        let mut context = fixture.context();

        let mut input = session_mode_input("echo hi", 500);
        let result = ExecuteCommandTool.execute(&mut context, &mut input).await;
        let error = result
            .err()
            .expect("session mode should fail without a registry");
        assert!(
            error.to_string().contains("not available"),
            "error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_mode_streams_raw_chunks_and_plain_text() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut fixture = session_mode_fixture(dir.path())
            .with_ui()
            .with_tool_id("tool-stream-1".to_string());
        let mut context = fixture.context();

        let mut input = session_mode_input("printf '\\033[32mgreen\\033[0m\\n'", 10_000);
        let result = ExecuteCommandTool.execute(&mut context, &mut input).await?;

        assert!(!result.running);
        assert!(result.output.contains("green"), "output: {}", result.output);
        assert!(
            !result.output.contains('\u{1b}'),
            "LLM-facing output must be ANSI-free: {:?}",
            result.output
        );

        drop(context);
        let streaming = fixture.ui().unwrap().get_streaming_output();
        assert!(
            streaming.iter().any(|s| s.starts_with("[terminal-bytes:")),
            "raw terminal chunks should stream live: {streaming:?}"
        );
        assert!(
            streaming
                .iter()
                .any(|s| s.contains("green") && !s.contains('\u{1b}')),
            "a plain-text chunk should stream too: {streaming:?}"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn background_session_keeps_streaming_after_the_tool_call_returns() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut fixture = session_mode_fixture(dir.path())
            .with_ui()
            .with_tool_id("tool-bg-1".to_string());

        // Short yield: the tool returns while the process is still running,
        // before it prints the delayed "LATE" marker.
        let session_id = {
            let mut context = fixture.context();
            let mut input = session_mode_input(
                "printf 'EARLY\\n'; sleep 0.6; printf 'LATE\\n'; sleep 30",
                300,
            );
            let result = ExecuteCommandTool.execute(&mut context, &mut input).await?;
            assert!(result.running, "process should outlive the tool call");
            result.pty_session_id.expect("running session has an id")
        };
        // The tool call is over (context dropped). The agent would now be
        // doing other work — no tool is polling the session.

        let streamed_at_return = fixture.ui().unwrap().get_terminal_output_text();
        assert!(
            streamed_at_return.contains("EARLY"),
            "early output should have streamed: {streamed_at_return:?}"
        );
        assert!(
            !streamed_at_return.contains("LATE"),
            "the delayed output cannot have streamed yet: {streamed_at_return:?}"
        );

        // Wait past the delay without any tool call touching the session.
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;

        let streamed_later = fixture.ui().unwrap().get_terminal_output_text();
        assert!(
            streamed_later.contains("LATE"),
            "output produced between turns should keep streaming to the card: {streamed_later:?}"
        );

        fixture
            .pty_sessions()
            .unwrap()
            .get(session_id)
            .unwrap()
            .terminate();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_mode_render_advertises_write_stdin() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut fixture = session_mode_fixture(dir.path());
        let mut context = fixture.context();

        let mut input = session_mode_input("sleep 30", 300);
        let result = ExecuteCommandTool.execute(&mut context, &mut input).await?;

        let mut tracker = ResourcesTracker::new();
        let rendered = result.render(&mut tracker);
        let session_id = result.pty_session_id.unwrap();
        assert!(rendered.contains("Still running"));
        assert!(rendered.contains(&format!("session_id: {session_id}")));
        assert!(rendered.contains("write_stdin"));

        drop(context);
        fixture.pty_sessions().unwrap().terminate_all();
        Ok(())
    }
}
