//! The servers behind file viewers.
//!
//! A viewer names a URL. When something answers there (a server the user
//! runs, a remote one), that is used. Otherwise the viewer's start command
//! runs in the plugin's folder, and the viewer waits until the URL answers.
//! A started server is shared by every viewer of that plugin viewer and
//! stopped (its whole process group) a while after the last one closed, so
//! switching between files does not restart it, or when the app quits
//! ([`stop_all`]).

use super::super::catalog::PluginFileViewer;
use super::super::manifest::substitute_root;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long a started server outlives its last viewer.
pub const LINGER: Duration = Duration::from_secs(300);
/// How long a connection attempt may take.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// Pause between connection attempts while a server starts.
const POLL: Duration = Duration::from_millis(250);
/// Lines of the server's output kept for an error message.
const OUTPUT_TAIL: usize = 20;

/// A server the app started. Its process group is stopped on drop.
pub struct StartedServer {
    child: Mutex<Option<Child>>,
    /// The last lines it printed.
    output: Arc<Mutex<Vec<String>>>,
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

impl Drop for StartedServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().ok().and_then(|mut c| c.take()) {
            stop(&mut child);
        }
    }
}

/// Stop `child` and what it started.
fn stop(child: &mut Child) {
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

static STARTED: Mutex<Option<HashMap<String, Weak<StartedServer>>>> = Mutex::new(None);
/// Serializes starts, so two viewers opening at once start one server.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Make sure the server behind `viewer` answers. Returns the started server
/// to hold while the viewer lives (`None` when it was running already).
pub async fn ensure(viewer: &PluginFileViewer) -> Result<Option<Arc<StartedServer>>> {
    let address = address_of(&viewer.spec.url)?;
    let key = viewer.key();
    let _starting = STARTING.lock().await;
    let existing = STARTED
        .lock()
        .ok()
        .and_then(|started| started.as_ref()?.get(&key)?.upgrade());
    if answers(&address).await {
        return Ok(existing);
    }
    let Some(start) = &viewer.spec.start else {
        bail!(
            "Nothing answers at {}. Start its server, or give the viewer a start command.",
            viewer.spec.url
        );
    };
    // A server we started that stopped answering is replaced.
    drop(existing);
    let server = Arc::new(spawn(viewer)?);
    if let Ok(mut started) = STARTED.lock() {
        started
            .get_or_insert_with(HashMap::new)
            .insert(key.clone(), Arc::downgrade(&server));
    }
    let deadline = Instant::now() + Duration::from_secs(start.timeout_seconds.max(1));
    loop {
        if answers(&address).await {
            tracing::info!("Viewer server of {key} answers at {}", viewer.spec.url);
            return Ok(Some(server));
        }
        let exited = server
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.as_mut()?.try_wait().ok().flatten());
        if let Some(status) = exited {
            bail!(
                "The start command of {key} exited ({status}) before {} answered.\n{}",
                viewer.spec.url,
                server.output_tail()
            );
        }
        if Instant::now() > deadline {
            bail!(
                "{} did not answer within {}s after starting the server of {key}.\n{}",
                viewer.spec.url,
                start.timeout_seconds,
                server.output_tail()
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Run the viewer's start command in its plugin's folder.
fn spawn(viewer: &PluginFileViewer) -> Result<StartedServer> {
    let start = viewer
        .spec
        .start
        .as_ref()
        .ok_or_else(|| anyhow!("no start command"))?;
    let root = &viewer.root;
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
        .env("VIEWER_URL", &viewer.spec.url)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    tracing::info!(
        "Starting the viewer server of {}: {}",
        viewer.key(),
        program.display()
    );
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", program.display()))?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let key = viewer.key();
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
                tracing::debug!("viewer server {key}: {line}");
                if let Ok(mut output) = output.lock() {
                    output.push(line);
                    let excess = output.len().saturating_sub(OUTPUT_TAIL);
                    output.drain(..excess);
                }
            }
        });
    }
    Ok(StartedServer {
        child: Mutex::new(Some(child)),
        output,
    })
}

/// Stop every server the app started (the app quits). Servers still in use
/// stop too; their viewers go away with the app.
pub fn stop_all() {
    let servers: Vec<Arc<StartedServer>> = STARTED
        .lock()
        .ok()
        .and_then(|mut started| started.take())
        .into_iter()
        .flat_map(|started| started.into_values())
        .filter_map(|server| server.upgrade())
        .collect();
    for server in servers {
        if let Some(mut child) = server.child.lock().ok().and_then(|mut c| c.take()) {
            stop(&mut child);
        }
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
    use crate::plugin_host::manifest::{FileViewerSpec, ServerStart, ViewerProtocol};

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
        assert!(address_of("example.com").is_err());
    }

    fn viewer(root: &std::path::Path, url: String, start: Option<ServerStart>) -> PluginFileViewer {
        PluginFileViewer {
            plugin: "test".into(),
            root: root.to_path_buf(),
            spec: FileViewerSpec {
                id: format!("v{}", url.len()),
                label: "V".into(),
                files: vec!["*.x".into()],
                url,
                protocol: ViewerProtocol::None,
                start,
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
    async fn a_running_server_is_used_and_a_missing_one_reported() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let running = viewer(dir.path(), format!("http://127.0.0.1:{port}/"), None);
        assert!(ensure(&running).await.unwrap().is_none());

        let missing = viewer(
            dir.path(),
            format!("http://127.0.0.1:{}/", free_port()),
            None,
        );
        let error = ensure(&missing).await.unwrap_err().to_string();
        assert!(error.contains("Nothing answers"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_start_command_runs_until_the_url_answers_and_stops_on_drop() {
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
        let start = ServerStart {
            command: "sh".into(),
            args: vec!["${PLUGIN_ROOT}/serve.sh".into(), port.to_string()],
            env: HashMap::new(),
            timeout_seconds: 20,
        };
        let viewer = viewer(dir.path(), format!("http://127.0.0.1:{port}/"), Some(start));
        let server = ensure(&viewer).await.unwrap().expect("started");
        let again = ensure(&viewer).await.unwrap().expect("shared");
        assert!(Arc::ptr_eq(&server, &again));
        drop((server, again));
        let address = format!("127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(10);
        while answers(&address).await {
            assert!(Instant::now() < deadline, "the server keeps running");
            tokio::time::sleep(POLL).await;
        }
    }

    #[tokio::test]
    async fn a_start_command_that_exits_is_reported_with_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let start = ServerStart {
            command: "sh".into(),
            args: vec!["-c".into(), "echo cannot bind; exit 3".into()],
            env: HashMap::new(),
            timeout_seconds: 20,
        };
        let viewer = viewer(
            dir.path(),
            format!("http://127.0.0.1:{}/exit", free_port()),
            Some(start),
        );
        let error = format!("{:#}", ensure(&viewer).await.unwrap_err());
        assert!(error.contains("exited"), "{error}");
    }
}
