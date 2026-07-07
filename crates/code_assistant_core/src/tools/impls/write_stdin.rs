//! Interact with a PTY session started by `execute_command`'s session mode:
//! send input, poll for new output, or interrupt the process.

use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::ToolServicesAccess;
use crate::ui::streaming::DisplayFragment;
use anyhow::{anyhow, Result};
use pty_session::PtySessionStatus;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Yield-time bounds, mirroring the tool schema.
const MIN_YIELD_TIME_MS: u64 = 250;
/// Writes expect a quick reaction; polls may wait for slow processes.
const MAX_WRITE_YIELD_TIME_MS: u64 = 30_000;
const MAX_POLL_YIELD_TIME_MS: u64 = 300_000;
const DEFAULT_WRITE_YIELD_TIME_MS: u64 = 1_000;
const DEFAULT_POLL_YIELD_TIME_MS: u64 = 10_000;

#[derive(Deserialize, Serialize)]
pub struct WriteStdinInput {
    pub session_id: u32,
    /// Characters to send verbatim (no newline is appended). Empty polls
    /// for output without writing.
    #[serde(default)]
    pub chars: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct WriteStdinOutput {
    pub session_id: u32,
    pub output: String,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl Render for WriteStdinOutput {
    fn status(&self) -> String {
        if self.running {
            format!("Session {} is still running", self.session_id)
        } else {
            format!(
                "Session {} exited{}",
                self.session_id,
                self.exit_code
                    .map(|code| format!(" with code {code}"))
                    .unwrap_or_default()
            )
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut formatted = String::new();
        if self.running {
            formatted.push_str(&format!(
                "Status: Still running (session_id: {})\n",
                self.session_id
            ));
        } else {
            formatted.push_str("Status: Exited");
            if let Some(code) = self.exit_code {
                formatted.push_str(&format!(" (exit code: {code})"));
            }
            formatted.push('\n');
        }
        formatted.push_str(">>>>> OUTPUT:\n");
        formatted.push_str(&self.output);
        formatted.push_str("\n<<<<< END OF OUTPUT");
        formatted
    }

    fn render_for_ui(&self, _tracker: &mut ResourcesTracker) -> String {
        self.output.trim_end().to_string()
    }
}

impl ToolResult for WriteStdinOutput {
    fn is_success(&self) -> bool {
        // The interaction succeeded; the process' own exit code is data,
        // not a tool failure.
        true
    }
}

pub struct WriteStdinTool;

#[async_trait::async_trait]
impl Tool for WriteStdinTool {
    type Input = WriteStdinInput;
    type Output = WriteStdinOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Interact with a running session started by execute_command (session mode): ",
            "write characters to its stdin and return the output produced since the last call. ",
            "Leave `chars` empty to just poll for new output. ",
            "Send \"\\u0003\" (Ctrl-C) to interrupt the process. ",
            "Characters are sent verbatim — remember the trailing newline to submit a line."
        );
        ToolSpec {
            name: "write_stdin".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "integer",
                        "description": "Session id returned by execute_command when the process was still running"
                    },
                    "chars": {
                        "type": "string",
                        "description": "Characters to write to the session's stdin, verbatim. Empty (default) polls for new output without writing.",
                        "default": ""
                    },
                    "yield_time_ms": {
                        "type": "integer",
                        "description": "How long to wait for output before returning, in milliseconds. Defaults to 1000 after a write (max 30000) and 10000 for a poll (max 300000)."
                    }
                },
                "required": ["session_id"]
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
            multiline_params: &["chars"],
            // Hidden from the UI: the originating execute_command terminal
            // card already streams this session's raw (colored) output live,
            // including write_stdin's reactions, so a separate block would
            // just duplicate it.
            hidden: true,
            title_template: Some("Session input: {chars}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let Some(manager) = context
            .extension::<crate::tools::ToolServices>()
            .and_then(|services| services.pty_sessions.clone())
        else {
            return Err(anyhow!(
                "Interactive sessions are not available in this environment"
            ));
        };

        let Some(session) = manager.get(input.session_id) else {
            return Err(anyhow!(
                "Unknown session_id {}: the process may have exited and been cleaned up, or the id is stale",
                input.session_id
            ));
        };

        if !input.chars.is_empty() {
            if input.chars == "\u{3}" {
                // Works for both transports: PTY line discipline turns ETX
                // into SIGINT; pipe sessions get the signal directly.
                session.interrupt();
            } else {
                session.write(input.chars.as_bytes())?;
            }
        }

        let (default_yield, max_yield) = if input.chars.is_empty() {
            (DEFAULT_POLL_YIELD_TIME_MS, MAX_POLL_YIELD_TIME_MS)
        } else {
            (DEFAULT_WRITE_YIELD_TIME_MS, MAX_WRITE_YIELD_TIME_MS)
        };
        let yield_time = std::time::Duration::from_millis(
            input
                .yield_time_ms
                .unwrap_or(default_yield)
                .clamp(MIN_YIELD_TIME_MS, max_yield),
        );

        // The session's own sink (bound when execute_command created it)
        // streams raw colored output live to that command's terminal card,
        // continuously and across turns. Here we only poll for the
        // sanitized window text: the model result, plus a plain ToolOutput
        // chunk on this write_stdin card as a record of what came back.
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
            PtySessionStatus::Running => WriteStdinOutput {
                session_id: input.session_id,
                output: collected.output,
                running: true,
                exit_code: None,
            },
            PtySessionStatus::Exited(code) => {
                // The process is gone; stop tracking the session.
                manager.remove(input.session_id);
                WriteStdinOutput {
                    session_id: input.session_id,
                    output: collected.output,
                    running: false,
                    exit_code: code,
                }
            }
        };

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use pty_session::{PtySession, PtySpawnConfig};
    use std::sync::Arc;

    fn fixture_with_session(command_line: &str, tty: bool) -> (ToolTestFixture, u32) {
        let fixture = ToolTestFixture::new().with_pty_sessions();
        let mut config = PtySpawnConfig::shell_command(command_line);
        config.tty = tty;
        let session = Arc::new(PtySession::spawn(config).unwrap());
        let session_id = fixture
            .pty_sessions()
            .unwrap()
            .register(session, command_line);
        (fixture, session_id)
    }

    fn input(session_id: u32, chars: &str, yield_time_ms: u64) -> WriteStdinInput {
        WriteStdinInput {
            session_id,
            chars: chars.to_string(),
            yield_time_ms: Some(yield_time_ms),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_reaches_the_process_and_returns_output() -> Result<()> {
        let (mut fixture, session_id) = fixture_with_session("cat", true);
        let mut context = fixture.context();

        let mut input = input(session_id, "hello-session\n", 1_000);
        let result = WriteStdinTool.execute(&mut context, &mut input).await?;

        assert!(result.running);
        assert!(
            result.output.contains("hello-session"),
            "output: {}",
            result.output
        );

        drop(context);
        fixture.pty_sessions().unwrap().terminate_all();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_chars_polls_and_reaps_exited_session() -> Result<()> {
        let (mut fixture, session_id) =
            fixture_with_session("sleep 0.3; echo late-output; exit 5", true);
        let mut context = fixture.context();

        let mut poll = input(session_id, "", 10_000);
        let result = WriteStdinTool.execute(&mut context, &mut poll).await?;

        assert!(!result.running);
        assert_eq!(result.exit_code, Some(5));
        assert!(
            result.output.contains("late-output"),
            "output: {}",
            result.output
        );

        drop(context);
        assert!(
            fixture.pty_sessions().unwrap().get(session_id).is_none(),
            "exited session should be removed from the registry"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ctrl_c_interrupts_the_process() -> Result<()> {
        let (mut fixture, session_id) = fixture_with_session("sleep 30", true);
        let mut context = fixture.context();

        let mut interrupt = input(session_id, "\u{3}", 10_000);
        let result = WriteStdinTool.execute(&mut context, &mut interrupt).await?;

        assert!(!result.running, "process should have been interrupted");

        drop(context);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_id_is_an_error() {
        let mut fixture = ToolTestFixture::new().with_pty_sessions();
        let mut context = fixture.context();

        let mut poll = input(4711, "", 250);
        let result = WriteStdinTool.execute(&mut context, &mut poll).await;
        let error = result.err().expect("unknown session id should fail");
        assert!(error.to_string().contains("4711"), "error: {error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_to_piped_session_fails_but_ctrl_c_works() -> Result<()> {
        let (mut fixture, session_id) = fixture_with_session("sleep 30", false);
        let mut context = fixture.context();

        let mut write = input(session_id, "nope\n", 250);
        let result = WriteStdinTool.execute(&mut context, &mut write).await;
        assert!(result.is_err(), "piped sessions have no stdin");

        let mut interrupt = input(session_id, "\u{3}", 10_000);
        let result = WriteStdinTool.execute(&mut context, &mut interrupt).await?;
        assert!(!result.running, "SIGINT should stop the piped process");

        drop(context);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_registry_fails_gracefully() {
        let mut fixture = ToolTestFixture::new();
        let mut context = fixture.context();

        let mut poll = input(1, "", 250);
        let result = WriteStdinTool.execute(&mut context, &mut poll).await;
        let error = result.err().expect("no registry should fail");
        assert!(
            error.to_string().contains("not available"),
            "error: {error}"
        );
    }
}
