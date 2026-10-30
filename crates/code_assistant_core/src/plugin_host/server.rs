//! The servers plugin file renderers need.
//!
//! A renderer with a `start` command names the address its server answers
//! at. When something answers there (a server the user runs, a remote one),
//! that is used. Otherwise the start command runs in the plugin's folder,
//! and the renderer waits until the address answers. A started server is
//! shared by every view of that renderer, kept while one holds a
//! [`ServerLease`] and for [`LINGER`] after the last one let go (switching
//! between files does not restart it), then stopped; [`stop_all`] stops every
//! one when the app quits.
//!
//! Stopping runs the plugin's `stop` command first, when it has one (a
//! container outlives the `docker run` that started it), gives the start
//! command [`GRACE`] to exit, and then ends it: with its whole process group
//! on Unix, the process alone on Windows.

use super::manifest::{ServerStart, substitute_root};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long a started server outlives its last view.
pub const LINGER: Duration = Duration::from_secs(300);
/// How long a connection attempt may take.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// Pause between connection attempts while a server starts.
const POLL: Duration = Duration::from_millis(250);
/// Lines of the server's output kept for an error message.
const OUTPUT_TAIL: usize = 20;
/// How long the start command may take to exit after the stop command.
pub const GRACE: Duration = Duration::from_secs(5);

/// A server the app started. Stopped on drop (in the background).
pub struct StartedServer {
    key: String,
    child: Mutex<Option<Child>>,
    /// The last lines it printed.
    output: Arc<Mutex<Vec<String>>>,
    /// The plugin's stop command, ready to run.
    stop: Mutex<Option<StopCommand>>,
}

/// A stop command with its arguments and environment filled in.
struct StopCommand {
    command: std::process::Command,
    timeout: Duration,
}

impl std::fmt::Debug for StartedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedServer").finish_non_exhaustive()
    }
}

impl StartedServer {
    fn output_tail(&self) -> String {
        self.output.lock().map(|o| o.join("\n")).unwrap_or_default()
    }
}

impl StartedServer {
    /// Stop the server on a thread of its own (the stop command may take a
    /// while); `None` when it was stopped already.
    fn shut_down(&self) -> Option<std::thread::JoinHandle<()>> {
        let mut child = self.child.lock().ok()?.take()?;
        let stop = self.stop.lock().ok().and_then(|mut stop| stop.take());
        let key = self.key.clone();
        let thread = std::thread::Builder::new()
            .name(format!("stop {key}"))
            .spawn(move || {
                if let Some(stop) = stop {
                    run_stop_command(&key, stop);
                    wait_for_exit(&mut child, GRACE);
                }
                terminate(&mut child);
            });
        match thread {
            Ok(thread) => Some(thread),
            Err(e) => {
                tracing::warn!("Could not stop the server of {}: {e}", self.key);
                None
            }
        }
    }
}

impl Drop for StartedServer {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Run the plugin's stop command and wait for it, ending it when it takes
/// too long.
fn run_stop_command(key: &str, mut stop: StopCommand) {
    tracing::info!("Stopping the server of {key}");
    let mut process = match stop.command.spawn() {
        Ok(process) => process,
        Err(e) => {
            tracing::warn!("Could not run the stop command of {key}: {e}");
            return;
        }
    };
    let deadline = Instant::now() + stop.timeout;
    loop {
        match process.try_wait() {
            Ok(Some(status)) if status.success() => return,
            Ok(Some(status)) => {
                tracing::warn!("The stop command of {key} failed ({status})");
                return;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                tracing::warn!("The stop command of {key} did not finish in time");
                let _ = process.kill();
                let _ = process.wait();
                return;
            }
        }
    }
}

/// Wait up to `grace` for `child` to exit by itself.
fn wait_for_exit(child: &mut Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// End `child` and what it started, if it still runs.
fn terminate(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The child leads its own process group (see `spawn`): signal all of it.
        // SAFETY: plain syscall on a process group we created.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    let _ = child.start_kill();
}

/// A server to make sure of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerTarget {
    /// Names the server in logs and shares it: `<plugin>/<renderer>`.
    pub key: String,
    /// Where it answers once it runs.
    pub url: String,
    /// The plugin's folder, where the start command runs.
    pub root: PathBuf,
    pub start: ServerStart,
}

/// Keeps a started server running; dropping the last one stops it after
/// [`LINGER`]. A lease on a server the app did not start holds nothing.
pub struct ServerLease {
    server: Option<Arc<StartedServer>>,
    /// Where the delayed stop waits.
    runtime: Option<tokio::runtime::Handle>,
}

impl Drop for ServerLease {
    fn drop(&mut self) {
        let Some(server) = self.server.take() else {
            return;
        };
        match &self.runtime {
            Some(runtime) => {
                runtime.spawn(async move {
                    tokio::time::sleep(LINGER).await;
                    drop(server);
                });
            }
            None => drop(server),
        }
    }
}

impl std::fmt::Debug for ServerLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerLease")
            .field("started", &self.server.is_some())
            .finish()
    }
}

static STARTED: Mutex<Option<HashMap<String, Weak<StartedServer>>>> = Mutex::new(None);
/// Serializes starts, so two views opening at once start one server.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Make sure the server of `target` answers, starting it when nothing does.
/// Needs a tokio runtime. Hold the lease while the server is needed.
pub async fn ensure(target: &ServerTarget) -> Result<ServerLease> {
    let address = address_of(&target.url)?;
    let key = &target.key;
    let lease = |server| ServerLease {
        server,
        runtime: tokio::runtime::Handle::try_current().ok(),
    };
    let _starting = STARTING.lock().await;
    let existing = STARTED
        .lock()
        .ok()
        .and_then(|started| started.as_ref()?.get(key)?.upgrade());
    if answers(&address).await {
        return Ok(lease(existing));
    }
    // A server we started that stopped answering is replaced.
    drop(existing);
    let server = Arc::new(spawn(target)?);
    if let Ok(mut started) = STARTED.lock() {
        started
            .get_or_insert_with(HashMap::new)
            .insert(key.clone(), Arc::downgrade(&server));
    }
    let timeout = target.start.timeout_seconds.max(1);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if answers(&address).await {
            tracing::info!("The server of {key} answers at {}", target.url);
            return Ok(lease(Some(server)));
        }
        let exited = server
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.as_mut()?.try_wait().ok().flatten());
        if let Some(status) = exited {
            bail!(
                "The start command of {key} exited ({status}) before {} answered.\n{}",
                target.url,
                server.output_tail()
            );
        }
        if Instant::now() > deadline {
            bail!(
                "{} did not answer within {timeout}s after starting the server of {key}.\n{}",
                target.url,
                server.output_tail()
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Run the start command in the plugin's folder.
fn spawn(target: &ServerTarget) -> Result<StartedServer> {
    let start = &target.start;
    let root = &target.root;
    let program = start.program(root);
    let mut command = Command::new(&program);
    command
        .args(start.args.iter().map(|arg| substitute_root(arg, root)))
        .envs(
            start
                .env
                .iter()
                .map(|(key, value)| (key, substitute_root(value, root))),
        )
        .env("PLUGIN_ROOT", root)
        .env("SERVER_URL", &target.url)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    tracing::info!(
        "Starting the server of {}: {}",
        target.key,
        program.display()
    );
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", program.display()))?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let key = target.key.clone();
    let stdout = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>);
    let stderr = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>);
    for stream in [stdout, stderr].into_iter().flatten() {
        let output = output.clone();
        let key = key.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("server of {key}: {line}");
                if let Ok(mut output) = output.lock() {
                    output.push(line);
                    let excess = output.len().saturating_sub(OUTPUT_TAIL);
                    output.drain(..excess);
                }
            }
        });
    }
    let stop = start.stop.as_ref().map(|stop| {
        let mut command = std::process::Command::new(stop.program(root));
        command
            .args(stop.args.iter().map(|arg| substitute_root(arg, root)))
            .envs(
                stop.env
                    .iter()
                    .map(|(key, value)| (key, substitute_root(value, root))),
            )
            .env("PLUGIN_ROOT", root)
            .env("SERVER_URL", &target.url)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        StopCommand {
            command,
            timeout: Duration::from_secs(stop.timeout_seconds.max(1)),
        }
    });
    Ok(StartedServer {
        key: target.key.clone(),
        child: Mutex::new(Some(child)),
        output,
        stop: Mutex::new(stop),
    })
}

/// Stop every server the app started (the app quits) and wait until they
/// are stopped, each bounded by its stop command's timeout and [`GRACE`].
/// Servers still in use stop too; their views go away with the app.
pub fn stop_all() {
    let servers: Vec<Arc<StartedServer>> = STARTED
        .lock()
        .ok()
        .and_then(|mut started| started.take())
        .into_iter()
        .flat_map(|started| started.into_values())
        .filter_map(|server| server.upgrade())
        .collect();
    let stopping: Vec<_> = servers.iter().filter_map(|s| s.shut_down()).collect();
    for thread in stopping {
        let _ = thread.join();
    }
}

/// `host:port` of an http(s) URL.
pub fn address_of(url: &str) -> Result<String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| anyhow!("not a URL: {url}"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        bail!("no host in {url}");
    }
    let has_port = match authority.rfind(':') {
        Some(colon) => !authority[colon..].contains(']'),
        None => false,
    };
    if has_port {
        return Ok(authority.to_string());
    }
    let port = match scheme {
        "https" => 443,
        "http" => 80,
        other => bail!("unsupported scheme {other}"),
    };
    Ok(format!("{authority}:{port}"))
}

/// Whether something accepts connections at `address`.
async fn answers(address: &str) -> bool {
    matches!(
        tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(address)).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_come_from_urls() {
        assert_eq!(
            address_of("http://localhost:8080/x?y").unwrap(),
            "localhost:8080"
        );
        assert_eq!(
            address_of("https://example.com").unwrap(),
            "example.com:443"
        );
        assert_eq!(address_of("http://[::1]/").unwrap(), "[::1]:80");
        assert_eq!(address_of("http://[::1]:9/").unwrap(), "[::1]:9");
        assert_eq!(
            address_of("http://localhost:8080/?dark={dark}").unwrap(),
            "localhost:8080"
        );
        assert!(address_of("example.com").is_err());
    }

    fn target(root: &std::path::Path, url: String, command: &str, args: &[String]) -> ServerTarget {
        ServerTarget {
            key: format!("test/{url}"),
            url,
            root: root.to_path_buf(),
            start: ServerStart {
                command: command.into(),
                url: None,
                args: args.to_vec(),
                env: HashMap::new(),
                timeout_seconds: 20,
                stop: None,
            },
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn a_running_server_is_used_as_it_is() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        // A start command that would fail if it ran.
        let running = target(
            dir.path(),
            format!("http://127.0.0.1:{port}/"),
            "false",
            &[],
        );
        let lease = ensure(&running).await.unwrap();
        assert!(lease.server.is_none(), "nothing was started");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_start_command_runs_until_the_url_answers_and_stops_with_its_last_lease() {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: no python3");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("serve.sh"),
            "#!/bin/sh\necho starting on \"$1\"\nexec python3 -m http.server \"$1\" --bind 127.0.0.1\n",
        )
        .unwrap();
        let port = free_port();
        // Through `sh`: the script is not marked executable.
        let args = ["${PLUGIN_ROOT}/serve.sh".to_string(), port.to_string()];
        let target = target(dir.path(), format!("http://127.0.0.1:{port}/"), "sh", &args);
        let mut lease = ensure(&target).await.unwrap();
        let mut again = ensure(&target).await.unwrap();
        let (first, second) = (lease.server.take().unwrap(), again.server.take().unwrap());
        assert!(Arc::ptr_eq(&first, &second), "shared");
        // The last lease's delayed stop, without the wait.
        drop((first, second));
        let address = format!("127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(10);
        while answers(&address).await {
            assert!(Instant::now() < deadline, "the server keeps running");
            tokio::time::sleep(POLL).await;
        }
    }

    /// A server the start command only hands over to (a container, a
    /// daemon) is stopped by the plugin's stop command, which runs before
    /// the start command is ended.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_stop_command_stops_the_server() {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: no python3");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // The server runs detached from the start command, like a container
        // from `docker run`; the start command just stays.
        std::fs::write(
            dir.path().join("serve.sh"),
            "#!/bin/sh\nsetsid python3 -m http.server \"$1\" --bind 127.0.0.1 &\n\
             echo $! > server.pid\nexec sleep 600\n",
        )
        .unwrap();
        let port = free_port();
        let args = ["${PLUGIN_ROOT}/serve.sh".to_string(), port.to_string()];
        let mut target = target(dir.path(), format!("http://127.0.0.1:{port}/"), "sh", &args);
        target.start.stop = Some(crate::plugin_host::manifest::ServerStop {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "kill $(cat server.pid) && touch stopped".into(),
            ],
            env: HashMap::new(),
            timeout_seconds: 10,
        });
        let mut lease = ensure(&target).await.unwrap();
        let server = lease.server.take().expect("started");
        let stopping = server.shut_down().expect("it was running");
        tokio::task::spawn_blocking(move || stopping.join().unwrap())
            .await
            .unwrap();
        assert!(dir.path().join("stopped").exists(), "the stop command ran");
        let address = format!("127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(10);
        while answers(&address).await {
            assert!(Instant::now() < deadline, "the server keeps running");
            tokio::time::sleep(POLL).await;
        }
        assert!(server.shut_down().is_none(), "stopped once");
    }

    #[tokio::test]
    async fn a_start_command_that_exits_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let args = ["-c".to_string(), "echo cannot bind; exit 3".to_string()];
        let target = target(
            dir.path(),
            format!("http://127.0.0.1:{}/exit", free_port()),
            "sh",
            &args,
        );
        let error = format!("{:#}", ensure(&target).await.unwrap_err());
        assert!(error.contains("exited"), "{error}");
    }
}
