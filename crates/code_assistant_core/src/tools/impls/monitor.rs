//! `monitor` / `stop_monitor`: the agent runs a command in the background
//! whose output lines are events, and is told about each one with a message
//! instead of polling. See `crate::session::monitor` and
//! `docs/background-monitors.md`.

use super::execute_command::{PreparedCommand, prepare_command};
use crate::session::monitor::{MonitorStart, monitor_output};
use crate::tools::ToolServicesAccess;
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Monitors die with the process; a week is plenty.
const MAX_TIMEOUT_SECONDS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_TIMEOUT_SECONDS: u64 = 60 * 60;
/// The monitor reads output as it arrives; the process' own buffer only
/// needs to hold what nobody reads.
const MONITOR_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MonitorInput {
    pub project: String,
    pub command_line: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ask_user_approval: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorOutput {
    pub monitor_id: u64,
    pub description: String,
    pub timeout_seconds: u64,
}

impl Render for MonitorOutput {
    fn status(&self) -> String {
        format!("Monitor #{} started: {}", self.monitor_id, self.description)
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let id = self.monitor_id;
        format!(
            "Monitor #{id} started. Every line the command prints reaches you as a message \
             starting with '[monitor #{id}]' (lines printed close together arrive together); \
             when the command ends, '[monitor #{id} ended]' follows. It is stopped after {}s, \
             with stop_monitor, or when the application exits. There is no need to wait for it: \
             continue with other work, or end your turn if there is nothing else to do.",
            self.timeout_seconds
        )
    }
}

impl ToolResult for MonitorOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct MonitorTool;

#[async_trait::async_trait]
impl Tool for MonitorTool {
    type Input = MonitorInput;
    type Output = MonitorOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Run a command in the background and get notified about its output: every line it prints ",
            "(stdout and stderr) is an event that reaches you as a message, starting a new turn when you are idle. ",
            "Use it to wait for something external without polling — e.g. a loop that checks a pull request's CI ",
            "status every minute and prints a line only when the status changes, `tail -f` filtered with grep, ",
            "or a file watcher. Print only meaningful changes: every line costs a turn, and a monitor printing ",
            "too often is stopped. The command runs until it exits, its timeout, or stop_monitor. ",
            "For a single long-running command whose end you want to know about, prefer execute_command's ",
            "session mode instead: you are told when it exits."
        );
        ToolSpec {
            name: "monitor".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project to run the command in"
                    },
                    "command_line": {
                        "type": "string",
                        "description": "The command or shell script to run. Each line it prints is an event; redirect noise away (e.g. 2>/dev/null)."
                    },
                    "description": {
                        "type": "string",
                        "description": "Short label for what is watched (e.g. 'CI of PR #42'), shown in every notification"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional: working directory (relative to project root)"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_SECONDS,
                        "description": "Stop the monitor after this many seconds (default 3600)"
                    },
                    "ask_user_approval": {
                        "type": "boolean",
                        "description": "Set to true if this command should request user approval to run outside the sandbox (e.g. for network access)",
                        "default": false
                    }
                },
                "required": ["project", "command_line", "description"]
            }),
            annotations: Some(json!({
                "readOnlyHint": false,
                "idempotentHint": false
            })),
            capabilities: ToolSpec::capabilities(&[
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &["command_line"],
            hidden: false,
            title_template: Some("Monitoring: {description}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let description = input.description.trim();
        if description.is_empty() {
            return Err(anyhow!("description must not be empty"));
        }
        if input.command_line.trim().is_empty() {
            return Err(anyhow!("command_line must not be empty"));
        }
        let timeout_seconds = input.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if timeout_seconds == 0 || timeout_seconds > MAX_TIMEOUT_SECONDS {
            return Err(anyhow!(
                "timeout_seconds must be between 1 and {MAX_TIMEOUT_SECONDS}"
            ));
        }
        let monitors = context
            .services()
            .monitors
            .clone()
            .ok_or_else(|| anyhow!("Monitors are unavailable in this context"))?;

        let PreparedCommand {
            effective_working_dir,
            sandbox_request,
        } = prepare_command(
            context,
            "monitor",
            &input.project,
            input.working_dir.as_deref(),
            &input.command_line,
            input.ask_user_approval,
        )
        .await?;

        let spec = context.command_executor.prepare_pty_spawn(
            &input.command_line,
            &effective_working_dir,
            Some(&sandbox_request),
        )?;
        let (sink, output) = monitor_output();
        let mut config = pty_session::PtySpawnConfig::from_argv(spec.argv);
        config.env = spec.env;
        config.keep_alive = spec.keep_alive;
        // Events are lines: no terminal, no prompts, stdin closed.
        config.tty = false;
        config.working_dir = Some(effective_working_dir);
        config.max_buffer_bytes = MONITOR_BUFFER_BYTES;
        config.output_sink = Some(sink);
        let process = Arc::new(pty_session::PtySession::spawn(config)?);

        let monitor_id = monitors.start(MonitorStart {
            description: description.to_string(),
            command_line: input.command_line.clone(),
            timeout: Duration::from_secs(timeout_seconds),
            process,
            output,
        });

        Ok(MonitorOutput {
            monitor_id,
            description: description.to_string(),
            timeout_seconds,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StopMonitorInput {
    pub monitor_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopMonitorOutput {
    pub monitor_id: u64,
    /// Whether it was still running.
    pub stopped: bool,
}

impl Render for StopMonitorOutput {
    fn status(&self) -> String {
        if self.stopped {
            format!("Monitor #{} stopped", self.monitor_id)
        } else {
            format!("Monitor #{} was not running", self.monitor_id)
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if self.stopped {
            format!(
                "Monitor #{} stopped; its command was terminated.",
                self.monitor_id
            )
        } else {
            format!(
                "Monitor #{} was not running (it already ended, or the id is unknown).",
                self.monitor_id
            )
        }
    }
}

impl ToolResult for StopMonitorOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct StopMonitorTool;

#[async_trait::async_trait]
impl Tool for StopMonitorTool {
    type Input = StopMonitorInput;
    type Output = StopMonitorOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "stop_monitor".into(),
            description: "Stop a monitor started with the monitor tool and terminate its command. \
                No further messages arrive from it."
                .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "monitor_id": {
                        "type": "integer",
                        "description": "The id returned by the monitor tool."
                    }
                },
                "required": ["monitor_id"]
            }),
            annotations: None,
            capabilities: ToolSpec::capabilities(&[
                capabilities::READ_ONLY,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Stopping monitor #{monitor_id}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let monitors = context
            .services()
            .monitors
            .as_ref()
            .ok_or_else(|| anyhow!("Monitors are unavailable in this context"))?;
        Ok(StopMonitorOutput {
            monitor_id: input.monitor_id,
            stopped: monitors.stop(input.monitor_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{MockExplorer, MockProjectManager, ToolTestFixture};

    fn fixture(dir: &std::path::Path) -> (ToolTestFixture, crate::mocks::RecordingMonitorSink) {
        let explorer = MockExplorer::new(Default::default(), None).with_root(dir.to_path_buf());
        let project_manager = MockProjectManager::new().with_project_path(
            "real",
            dir.to_path_buf(),
            Box::new(explorer),
        );
        ToolTestFixture::with_project_manager(project_manager).with_recorded_monitors()
    }

    fn input(command_line: &str) -> MonitorInput {
        MonitorInput {
            project: "real".into(),
            command_line: command_line.into(),
            description: "status".into(),
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn monitor_lines_arrive_as_messages() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (mut fixture, notifications) = fixture(dir.path());
        let mut context = fixture.context();

        let mut input = input("echo pending; sleep 1; echo passed");
        let output = MonitorTool.execute(&mut context, &mut input).await?;
        assert_eq!(output.monitor_id, 1);
        assert_eq!(output.timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
        assert!(
            output
                .render(&mut ResourcesTracker::new())
                .contains("[monitor #1]")
        );

        let delivered = notifications.wait_for(2).await;
        assert_eq!(delivered[0], "[monitor #1] status\npending");
        assert!(
            delivered[1].starts_with("[monitor #1 ended] status\n"),
            "{}",
            delivered[1]
        );
        assert!(delivered[1].ends_with("\npassed"), "{}", delivered[1]);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_monitor_stops_it_silently() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (mut fixture, notifications) = fixture(dir.path());
        let mut context = fixture.context();

        let mut start = input("sleep 30");
        let started = MonitorTool.execute(&mut context, &mut start).await?;

        let mut stop = StopMonitorInput {
            monitor_id: started.monitor_id,
        };
        assert!(
            StopMonitorTool
                .execute(&mut context, &mut stop)
                .await?
                .stopped
        );
        assert!(
            !StopMonitorTool
                .execute(&mut context, &mut stop)
                .await?
                .stopped
        );
        drop(context);
        assert!(
            fixture
                .monitors()
                .unwrap()
                .handle
                .active("test-session")
                .is_empty()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(notifications.delivered().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn monitor_validates_its_input() {
        let dir = tempfile::tempdir().unwrap();
        let (mut fixture, _) = fixture(dir.path());
        let mut context = fixture.context();

        let mut no_description = MonitorInput {
            description: "  ".into(),
            ..input("true")
        };
        assert!(
            MonitorTool
                .execute(&mut context, &mut no_description)
                .await
                .is_err()
        );

        let mut zero_timeout = MonitorInput {
            timeout_seconds: Some(0),
            ..input("true")
        };
        assert!(
            MonitorTool
                .execute(&mut context, &mut zero_timeout)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn tools_error_without_monitor_context() {
        let mut fixture = ToolTestFixture::new();
        let mut context = fixture.context();
        let err = MonitorTool
            .execute(&mut context, &mut input("true"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unavailable"));
        let err = StopMonitorTool
            .execute(&mut context, &mut StopMonitorInput { monitor_id: 1 })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }
}
