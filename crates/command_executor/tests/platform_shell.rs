//! Commands run on every platform, through the same paths `execute_command`
//! uses: the blocking PTY executor, the plain executor, and a PTY session.
//!
//! CI runs this file on Linux, macOS and Windows (`build.yml`), the rest of
//! the workspace tests only on Linux. Only syntax every shell understands
//! (`echo`, `exit N`) appears here, so it holds for bash, zsh, PowerShell
//! and cmd.exe alike.
//!
//! Each command gets a deadline far below the executor's own 300 s: a
//! command that never starts (as on Windows, when ConPTY's startup cursor
//! query went unanswered) must fail the test quickly, not hang CI.

use command_executor::{
    CommandExecutor, CommandOutput, DefaultCommandExecutor, PtyCommandExecutor, PtySpawnSpec,
};
use pty_session::{PtySession, PtySessionStatus, PtySpawnConfig};
use std::future::Future;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(60);

async fn within_deadline<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {DEADLINE:?}"))
}

async fn run_on_pty(command_line: &str) -> CommandOutput {
    within_deadline(
        command_line,
        PtyCommandExecutor.execute_streaming(command_line, None, None, None),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_command_runs_on_the_pty() {
    let output = run_on_pty("echo platform-check").await;
    assert!(
        output.output.contains("platform-check"),
        "output: {:?}",
        output.output
    );
    assert_eq!(output.exit_code, Some(0));
    assert!(output.success);
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_command_reports_its_exit_code() {
    let output = run_on_pty("exit 3").await;
    assert_eq!(output.exit_code, Some(3), "output: {:?}", output.output);
    assert!(!output.success);
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_executor_runs_a_command() {
    let output = within_deadline(
        "plain echo",
        DefaultCommandExecutor.execute("echo plain-check", None, None),
    )
    .await
    .unwrap();
    assert!(
        output.output.contains("plain-check"),
        "output: {:?}",
        output.output
    );
    assert_eq!(output.exit_code, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn session_runs_a_command_on_the_pty() {
    // execute_command's session mode spawns the executor's prepared spec.
    let spec = PtySpawnSpec::shell("echo session-check");
    let mut config = PtySpawnConfig::from_argv(spec.argv);
    config.env = spec.env;
    let session = PtySession::spawn(config).unwrap();

    let mut output = String::new();
    let status = within_deadline("session echo", async {
        loop {
            let collected = session.collect_output(Duration::from_millis(500)).await;
            output.push_str(&collected.output);
            if let PtySessionStatus::Exited(code) = collected.status {
                return code;
            }
        }
    })
    .await;
    assert!(output.contains("session-check"), "output: {output:?}");
    assert_eq!(status, Some(0));
}
